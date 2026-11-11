//! School admin pages: overview, safeguarding duties, activation, pupil import
//! with login cards, and parent invite letters.

use crate::session::Session;
use crate::{ctx, go, render, AppState, Ctx, WebResult};
use askama::Template;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use identity::domain::ImportRow;
use infra::auth::Principal;
use infra::read;
use kernel::{AppError, Duty, Role};
use serde::Deserialize;
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/school", get(overview))
        .route("/school/duty", post(duty))
        .route("/school/activate", post(activate))
        .route("/school/pupils", get(pupils))
        .route("/school/pupils/check", post(check))
        .route("/school/pupils/create", post(create))
        .route("/school/parents", get(parents))
}

fn admin(s: &Session) -> WebResult<&Principal> {
    s.require(&[Role::SchoolAdmin])?;
    Ok(&s.0)
}

pub struct StaffRow {
    pub id: Uuid,
    pub name: String,
    pub email: String,
    pub role: String,
    pub lead: bool,
    pub deputy: bool,
}

#[derive(Template)]
#[template(path = "admin/overview.html")]
struct Overview {
    c: Ctx,
    school_name: String,
    code: String,
    active: bool,
    pupils: i64,
    staff_n: i64,
    parents: i64,
    cases: i64,
    staff: Vec<StaffRow>,
    activation_problem: String,
    message: String,
}

async fn overview_page(st: &AppState, p: &Principal, message: String) -> WebResult<Response> {
    let school = p.school()?;
    let (pupils, staff_n, parents, cases) = read::school_counts(st.pool(), school).await?;
    let sref = infra::auth::school_by_code(st.pool(), &p.school_code).await?.ok_or(AppError::NotFound)?;
    let staff = read::staff(st.pool(), school)
        .await?
        .into_iter()
        .map(|s| StaffRow {
            id: s.membership_id,
            name: format!("{} {}", s.first_name, s.last_name),
            email: s.email.unwrap_or_default(),
            role: Role::parse(&s.role).map(|r| r.label().to_string()).unwrap_or(s.role.clone()),
            lead: s.duties.iter().any(|d| d == "dsl"),
            deputy: s.duties.iter().any(|d| d == "deputy_dsl"),
        })
        .collect();
    let duties = read::school_duties(st.pool(), school).await?;
    let activation_problem = identity::domain::can_activate(&duties).err().unwrap_or("").to_string();
    let c = ctx(st, p, "School", "/school").await?;
    Ok(render(Overview { c, school_name: sref.name, code: sref.code, active: sref.active, pupils, staff_n, parents, cases, staff, activation_problem, message })?.into_response())
}

async fn overview(State(st): State<AppState>, s: Session) -> WebResult<Response> {
    let p = admin(&s)?;
    overview_page(&st, p, String::new()).await
}

#[derive(Deserialize)]
struct DutyForm {
    staff: Uuid,
    duty: String,
}

async fn duty(State(st): State<AppState>, s: Session, Form(f): Form<DutyForm>) -> WebResult<Response> {
    let p = admin(&s)?;
    let school = p.school()?;
    let duty = match f.duty.as_str() {
        "dsl" => Duty::SafeguardingLead,
        "deputy_dsl" => Duty::DeputySafeguardingLead,
        _ => return Err(AppError::Invalid("Unknown duty.".into()).into()),
    };
    read::set_duty(st.pool(), school, f.staff, duty.as_str(), true).await?;
    infra::outbox::audit(st.pool(), Some(p.membership_id.0), Some(school.0), "duty_assigned", serde_json::json!({ "staff": f.staff, "duty": duty.as_str() })).await;
    overview_page(&st, p, "Saved.".into()).await
}

#[derive(Deserialize)]
struct ActivateForm {
    active: bool,
}

async fn activate(State(st): State<AppState>, s: Session, Form(f): Form<ActivateForm>) -> WebResult<Response> {
    let p = admin(&s)?;
    let school = p.school()?;
    if f.active {
        let duties = read::school_duties(st.pool(), school).await?;
        if let Err(e) = identity::domain::can_activate(&duties) {
            return overview_page(&st, p, e.to_string()).await;
        }
    }
    read::set_school_active(st.pool(), school, f.active).await?;
    infra::outbox::audit(st.pool(), Some(p.membership_id.0), Some(school.0), "school_active", serde_json::json!({ "active": f.active })).await;
    overview_page(&st, p, if f.active { "Pupils can now sign in.".into() } else { "Pupil sign-in is switched off.".into() }).await
}

// ------------------------------------------------------------------ pupils and logins
#[derive(Template)]
#[template(path = "admin/pupils.html")]
struct PupilsPage {
    c: Ctx,
    csv: String,
    rows: Vec<ImportRow>,
    checked: bool,
    good: usize,
    error: String,
}

async fn pupils(State(st): State<AppState>, s: Session) -> WebResult<Response> {
    let p = admin(&s)?;
    let c = ctx(&st, p, "Pupils and logins", "/school/pupils").await?;
    Ok(render(PupilsPage { c, csv: String::new(), rows: vec![], checked: false, good: 0, error: String::new() })?.into_response())
}

#[derive(Deserialize)]
struct CsvForm {
    csv: String,
}

fn parse_csv(text: &str) -> Result<Vec<(String, String, String, String)>, String> {
    let mut rdr = csv::ReaderBuilder::new().has_headers(false).flexible(true).trim(csv::Trim::All).from_reader(text.as_bytes());
    let mut out = Vec::new();
    for (i, rec) in rdr.records().enumerate() {
        let rec = rec.map_err(|e| format!("Line {} can't be read: {e}", i + 1))?;
        let get = |n: usize| rec.get(n).unwrap_or("").to_string();
        if i == 0 && get(0).to_lowercase().contains("first") {
            continue; // header row
        }
        if rec.iter().all(|f| f.is_empty()) {
            continue;
        }
        out.push((get(0), get(1), get(2), get(3)));
    }
    if out.is_empty() {
        return Err("Paste at least one pupil: first name, last name, year, class.".into());
    }
    if out.len() > 500 {
        return Err("Import up to 500 pupils at a time.".into());
    }
    Ok(out)
}

async fn checked_rows(st: &AppState, p: &Principal, csv: &str) -> WebResult<Result<Vec<ImportRow>, String>> {
    let rows = match parse_csv(csv) {
        Ok(r) => r,
        Err(e) => return Ok(Err(e)),
    };
    let existing = read::usernames(st.pool(), p.school()?).await?;
    Ok(Ok(identity::domain::check_import(&rows, &existing)))
}

async fn check(State(st): State<AppState>, s: Session, Form(f): Form<CsvForm>) -> WebResult<Response> {
    let p = admin(&s)?;
    let c = ctx(&st, p, "Pupils and logins", "/school/pupils").await?;
    match checked_rows(&st, p, &f.csv).await? {
        Ok(mut rows) => {
            for r in rows.iter_mut() {
                if r.problem.is_none() && r.class_name.is_empty() {
                    r.problem = Some("Class missing".into());
                }
            }
            let good = rows.iter().filter(|r| r.problem.is_none()).count();
            Ok(render(PupilsPage { c, csv: f.csv, rows, checked: true, good, error: String::new() })?.into_response())
        }
        Err(e) => Ok(render(PupilsPage { c, csv: f.csv, rows: vec![], checked: false, good: 0, error: e })?.into_response()),
    }
}

pub struct LoginCard {
    pub name: String,
    pub class_name: String,
    pub username: String,
    pub password: String,
}

#[derive(Template)]
#[template(path = "admin/cards.html")]
struct CardsPage {
    c: Ctx,
    school_name: String,
    code: String,
    cards: Vec<LoginCard>,
}

async fn create(State(st): State<AppState>, s: Session, Form(f): Form<CsvForm>) -> WebResult<Response> {
    let p = admin(&s)?;
    let rows = match checked_rows(&st, p, &f.csv).await? {
        Ok(r) => r,
        Err(_) => return Ok(go("/school/pupils")),
    };
    let good: Vec<ImportRow> = rows.into_iter().filter(|r| r.problem.is_none() && !r.class_name.is_empty()).collect();
    let made = read::create_pupils(st.pool(), p.school()?, &good).await?;
    infra::outbox::audit(st.pool(), Some(p.membership_id.0), p.school_id.map(|s| s.0), "pupils_created", serde_json::json!({ "count": made.len() })).await;
    let cards = made.into_iter().map(|(name, class_name, username, password)| LoginCard { name, class_name, username, password }).collect();
    let c = ctx(&st, p, "Login cards", "/school/pupils").await?;
    Ok(render(CardsPage { c, school_name: p.school_name.clone(), code: p.school_code.clone(), cards })?.into_response())
}

#[derive(Template)]
#[template(path = "admin/parents.html")]
struct ParentsPage {
    c: Ctx,
    school_name: String,
    invites: Vec<read::InviteView>,
    linked: usize,
}

async fn parents(State(st): State<AppState>, s: Session) -> WebResult<Response> {
    let p = admin(&s)?;
    let invites = read::parent_invites(st.pool(), p.school()?).await?;
    let linked = invites.iter().filter(|i| i.linked).count();
    let c = ctx(&st, p, "Parents", "/school/parents").await?;
    Ok(render(ParentsPage { c, school_name: p.school_name.clone(), invites, linked })?.into_response())
}
