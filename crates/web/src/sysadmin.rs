//! System admin pages: schools, creating a school, content health, reports, AI and outbox.

use crate::session::Session;
use crate::{ctx, render, AppState, Ctx, WebResult};
use askama::Template;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use infra::auth::Principal;
use infra::read;
use kernel::Role;
use serde::Deserialize;
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/platform", get(schools))
        .route("/platform/schools", post(create_school))
        .route("/platform/content", get(content))
        .route("/platform/reports/{id}", post(resolve))
}

fn sysadmin(s: &Session) -> WebResult<&Principal> {
    s.require(&[Role::SystemAdmin])?;
    Ok(&s.0)
}

#[derive(Template)]
#[template(path = "sysadmin/schools.html")]
struct SchoolsPage {
    c: Ctx,
    schools: Vec<read::SchoolSummary>,
    created: String,
    error: String,
    form: NewSchool,
}

#[derive(Deserialize, Default, Clone)]
struct NewSchool {
    code: String,
    name: String,
    first: String,
    last: String,
    email: String,
}

async fn page(st: &AppState, p: &Principal, created: String, error: String, form: NewSchool) -> WebResult<Response> {
    let schools = read::schools(st.pool()).await?;
    let c = ctx(st, p, "Schools", "/platform").await?;
    Ok(render(SchoolsPage { c, schools, created, error, form })?.into_response())
}

async fn schools(State(st): State<AppState>, s: Session) -> WebResult<Response> {
    let p = sysadmin(&s)?;
    page(&st, p, String::new(), String::new(), NewSchool::default()).await
}

async fn create_school(State(st): State<AppState>, s: Session, Form(f): Form<NewSchool>) -> WebResult<Response> {
    let p = sysadmin(&s)?;
    let Some(code) = identity::domain::normalise_school_code(&f.code) else {
        return page(&st, p, String::new(), "School code must be 3 to 12 letters or numbers.".into(), f).await;
    };
    if f.name.trim().is_empty() || f.first.trim().is_empty() || !f.email.contains('@') {
        return page(&st, p, String::new(), "Fill in the school name, and the admin's name and email.".into(), f).await;
    }
    match read::create_school(st.pool(), &code, f.name.trim(), f.first.trim(), f.last.trim(), &f.email.trim().to_lowercase()).await {
        Ok(pw) => {
            infra::outbox::audit(st.pool(), Some(p.membership_id.0), None, "school_created", serde_json::json!({ "code": code })).await;
            let msg = format!(
                "{} created with code {code}. The school admin signs in as {} with temporary password {pw}. Send it to them securely. Pupils can't sign in until the school names its safeguarding leads.",
                f.name.trim(),
                f.email.trim().to_lowercase()
            );
            page(&st, p, msg, String::new(), NewSchool::default()).await
        }
        Err(kernel::AppError::Conflict(m)) | Err(kernel::AppError::Invalid(m)) => page(&st, p, String::new(), m, f).await,
        Err(e) => Err(e.into()),
    }
}

#[derive(Template)]
#[template(path = "sysadmin/content.html")]
struct ContentPage {
    c: Ctx,
    courses: Vec<read::ContentSummary>,
    reports: Vec<(Uuid, String, String, String)>,
    ai_live: bool,
    ai_calls: i64,
    ai_in: i64,
    ai_out: i64,
    lag: i64,
    pending: i64,
    failed: i64,
}

async fn content(State(st): State<AppState>, s: Session) -> WebResult<Response> {
    let p = sysadmin(&s)?;
    let courses = read::content_summary(st.pool()).await?;
    let reports = read::open_reports(st.pool()).await?.into_iter().map(|(id, prompt, note, at)| (id, prompt, note, kernel::short_date(at.date()))).collect();
    let (ai_calls, ai_in, ai_out) = read::ai_usage_summary(st.pool()).await?;
    let lag = infra::outbox::lag_seconds(st.pool()).await?.round() as i64;
    let (pending, failed): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE processed_at IS NULL), count(*) FILTER (WHERE processed_at IS NULL AND attempts >= 5) FROM platform.outbox",
    )
    .fetch_one(st.pool())
    .await
    .unwrap_or((0, 0));
    let c = ctx(&st, p, "Content and AI", "/platform/content").await?;
    Ok(render(ContentPage { c, courses, reports, ai_live: st.ai.is_live(), ai_calls, ai_in, ai_out, lag, pending, failed })?.into_response())
}

#[derive(Deserialize)]
struct ResolveForm {
    #[serde(default)]
    retire: Option<String>,
}

async fn resolve(State(st): State<AppState>, s: Session, Path(id): Path<Uuid>, Form(f): Form<ResolveForm>) -> WebResult<Response> {
    let p = sysadmin(&s)?;
    read::resolve_report(st.pool(), id, f.retire.is_some()).await?;
    infra::outbox::audit(st.pool(), Some(p.membership_id.0), None, "report_resolved", serde_json::json!({ "report": id, "retired": f.retire.is_some() })).await;
    Ok(crate::go("/platform/content"))
}
