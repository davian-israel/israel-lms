//! Parent and carer pages: a weekly view of each child, with ways to help.

use crate::session::Session;
use crate::{ctx, render, AppState, Ctx, WebResult};
use askama::Template;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use infra::auth::Principal;
use infra::read;
use kernel::{MembershipId, Role};
use reporting::domain::PupilWeek;
use serde::Deserialize;
use time::Duration;
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new().route("/family", get(family)).route("/family/link", post(link))
}

fn parent(s: &Session) -> WebResult<&Principal> {
    s.require(&[Role::Parent])?;
    Ok(&s.0)
}

#[derive(Deserialize)]
struct FamilyQ {
    child: Option<Uuid>,
    #[serde(default)]
    weeks_ago: i64,
}

pub struct Bar {
    pub day: String,
    pub minutes: i32,
    pub height: i32,
}

pub struct Fix {
    pub title: String,
    pub when: String,
    pub done: bool,
}

#[derive(Template)]
#[template(path = "parent/family.html")]
struct FamilyPage {
    c: Ctx,
    children: Vec<(Uuid, String, bool)>,
    child_id: Uuid,
    child_name: String,
    week: PupilWeek,
    week_label: String,
    weeks_ago: i64,
    bars: Vec<Bar>,
    fixes: Vec<Fix>,
    reviews_coming: usize,
    help: Vec<(String, String, String)>,
    teacher: String,
    teacher_email: String,
    paused: bool,
    message: String,
    error: String,
}

async fn page(st: &AppState, p: &Principal, q: FamilyQ, message: String, error: String) -> WebResult<Response> {
    let kids = read::children(st.pool(), p.membership_id).await?;
    let child = q.child.and_then(|c| kids.iter().find(|k| k.id == c)).or(kids.first()).cloned();
    let children = kids.iter().map(|k| (k.id, k.first_name.clone(), Some(k.id) == child.as_ref().map(|c| c.id))).collect();
    let today = st.clock.today();
    let weeks_ago = q.weeks_ago.clamp(0, 12);
    let from = kernel::week_start(today) - Duration::days(7 * weeks_ago);
    let (mut week, mut bars, mut fixes, mut reviews_coming, mut help, mut teacher, mut teacher_email, mut paused) =
        (PupilWeek::default(), vec![], vec![], 0, vec![], String::new(), String::new(), false);
    let (child_id, child_name) = match &child {
        Some(k) => (k.id, k.first_name.clone()),
        None => (Uuid::nil(), String::new()),
    };
    if let Some(k) = &child {
        week = read::pupil_week(st.pool(), k.id, from).await?;
        let max = week.minutes_by_day.iter().map(|d| d.1).max().unwrap_or(0).max(30);
        bars = week.minutes_by_day.iter().map(|(d, m)| Bar { day: d.clone(), minutes: *m, height: (m * 100 / max).max(if *m > 0 { 6 } else { 2 }) }).collect();
        let tasks = read::tasks_between(st.pool(), MembershipId(k.id), from, from + Duration::days(6)).await?;
        fixes = tasks
            .iter()
            .filter(|t| t.kind == "fix")
            .map(|t| Fix { title: t.concept_title.clone(), when: kernel::weekday_short(t.on_date).into(), done: t.done })
            .collect();
        reviews_coming = read::tasks_between(st.pool(), MembershipId(k.id), today, today + Duration::days(6)).await?.iter().filter(|t| t.kind == "review" && !t.done).count();
        help = read::help_ideas(st.pool(), k.id).await?;
        if let Some((name, email)) = read::teacher_contact(st.pool(), k.id).await? {
            teacher = name;
            teacher_email = email.unwrap_or_default();
        }
        paused = read::plan_paused(st.pool(), k.id).await?;
    }
    let week_label = match weeks_ago {
        0 => format!("This week, from {}", kernel::long_date(from)),
        1 => format!("Last week, from {}", kernel::long_date(from)),
        _ => format!("Week of {}", kernel::long_date(from)),
    };
    let c = ctx(st, p, "This week", "/family").await?;
    Ok(render(FamilyPage {
        c, children, child_id, child_name, week, week_label, weeks_ago, bars, fixes, reviews_coming, help, teacher, teacher_email, paused, message, error,
    })?
    .into_response())
}

async fn family(State(st): State<AppState>, s: Session, Query(q): Query<FamilyQ>) -> WebResult<Response> {
    let p = parent(&s)?;
    page(&st, p, q, String::new(), String::new()).await
}

#[derive(Deserialize)]
struct LinkForm {
    code: String,
}

async fn link(State(st): State<AppState>, s: Session, Form(f): Form<LinkForm>) -> WebResult<Response> {
    let p = parent(&s)?;
    let code: String = f.code.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let ok = !code.is_empty() && read::link_child(st.pool(), p.membership_id, &code).await?;
    let q = FamilyQ { child: None, weeks_ago: 0 };
    if ok {
        infra::outbox::audit(st.pool(), Some(p.membership_id.0), None, "parent_linked", serde_json::json!({})).await;
        page(&st, p, q, "Connected. You can now see your child's week.".into(), String::new()).await
    } else {
        page(&st, p, q, String::new(), "That code doesn't match, or it has already been used. Check the letter, or ask the school office for a new one.".into()).await
    }
}
