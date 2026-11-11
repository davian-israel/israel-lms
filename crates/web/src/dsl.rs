//! Safeguarding lead pages: alert list, case detail, acknowledge, record action and close.

use crate::session::Session;
use crate::{ctx, go, render, AppState, Ctx, WebResult};
use askama::Template;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use infra::auth::Principal;
use kernel::{AppError, CaseId, DomainEvent};
use safeguarding::application::Cases;
use safeguarding::domain::{CaseStatus, SafeguardingCase, Severity};
use serde::Deserialize;
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/safeguarding", get(list))
        .route("/safeguarding/{case}", get(case))
        .route("/safeguarding/{case}/acknowledge", post(acknowledge))
        .route("/safeguarding/{case}/close", post(close))
}

fn lead(s: &Session) -> WebResult<&Principal> {
    if !s.0.is_dsl() {
        return Err(AppError::Forbidden.into());
    }
    Ok(&s.0)
}

pub struct CaseRow {
    pub id: Uuid,
    pub pupil: String,
    pub severity: String,
    pub high: bool,
    pub category: String,
    pub opened: String,
    pub status: String,
    pub open: bool,
}

fn status_text(s: CaseStatus) -> &'static str {
    match s {
        CaseStatus::Open => "New",
        CaseStatus::Acknowledged => "Being handled",
        CaseStatus::Closed => "Closed",
    }
}

fn when(t: time::OffsetDateTime) -> String {
    format!("{} at {:02}:{:02} UTC", kernel::short_date(t.date()), t.hour(), t.minute())
}

async fn row(st: &AppState, c: &SafeguardingCase) -> WebResult<CaseRow> {
    Ok(CaseRow {
        id: c.id.0,
        pupil: infra::read::membership_name(st.pool(), c.membership_id.0).await?,
        severity: if c.severity == Severity::High { "High".into() } else { "Medium".into() },
        high: c.severity == Severity::High,
        category: c.category.replace('_', " "),
        opened: when(c.opened_at),
        status: status_text(c.status).into(),
        open: c.status == CaseStatus::Open,
    })
}

#[derive(Template)]
#[template(path = "dsl/list.html")]
struct ListPage {
    c: Ctx,
    cases: Vec<CaseRow>,
}

async fn list(State(st): State<AppState>, s: Session) -> WebResult<Response> {
    let p = lead(&s)?;
    let mut cases = Vec::new();
    for c in Cases::list(&st.repos, p.school()?).await? {
        cases.push(row(&st, &c).await?);
    }
    let c = ctx(&st, p, "Safeguarding", "/safeguarding").await?;
    Ok(render(ListPage { c, cases })?.into_response())
}

#[derive(Template)]
#[template(path = "dsl/case.html")]
struct CasePage {
    c: Ctx,
    r: CaseRow,
    excerpt: String,
    acknowledged: String,
    action: String,
    notes: String,
    closed: String,
    is_closed: bool,
    error: String,
    paused: bool,
}

async fn case_page(st: &AppState, p: &Principal, id: Uuid, error: String) -> WebResult<Response> {
    let k = Cases::get(&st.repos, p.school()?, CaseId(id)).await?;
    infra::outbox::audit(st.pool(), Some(p.membership_id.0), Some(k.school_id.0), "case_viewed", serde_json::json!({ "case": id })).await;
    let r = row(st, &k).await?;
    let paused = infra::read::plan_paused(st.pool(), k.membership_id.0).await?;
    let c = ctx(st, p, "Safeguarding alert", "/safeguarding").await?;
    Ok(render(CasePage {
        c,
        r,
        excerpt: k.excerpt.clone(),
        acknowledged: k.acknowledged_at.map(when).unwrap_or_default(),
        action: k.action.clone().unwrap_or_default(),
        notes: k.notes.clone().unwrap_or_default(),
        closed: k.closed_at.map(when).unwrap_or_default(),
        is_closed: k.status == CaseStatus::Closed,
        error,
        paused,
    })?
    .into_response())
}

async fn case(State(st): State<AppState>, s: Session, Path(id): Path<Uuid>) -> WebResult<Response> {
    let p = lead(&s)?;
    case_page(&st, p, id, String::new()).await
}

async fn acknowledge(State(st): State<AppState>, s: Session, Path(id): Path<Uuid>) -> WebResult<Response> {
    let p = lead(&s)?;
    let mut k = Cases::get(&st.repos, p.school()?, CaseId(id)).await?;
    k.acknowledge(st.clock.now()).map_err(|e| AppError::Invalid(e.to_string()))?;
    Cases::save(&st.repos, &k).await?;
    infra::outbox::audit(st.pool(), Some(p.membership_id.0), Some(k.school_id.0), "case_acknowledged", serde_json::json!({ "case": id })).await;
    Ok(go(&format!("/safeguarding/{id}")))
}

#[derive(Deserialize)]
struct CloseForm {
    action: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    resume: Option<String>,
}

async fn close(State(st): State<AppState>, s: Session, Path(id): Path<Uuid>, Form(f): Form<CloseForm>) -> WebResult<Response> {
    let p = lead(&s)?;
    let mut k = Cases::get(&st.repos, p.school()?, CaseId(id)).await?;
    if let Err(e) = k.record_action(&f.action, &f.notes).and_then(|_| k.close(st.clock.now())) {
        let msg = match e {
            safeguarding::domain::CaseError::NoAction => "Write what action you took before closing the alert.".to_string(),
            other => other.to_string(),
        };
        return case_page(&st, p, id, msg).await;
    }
    Cases::save(&st.repos, &k).await?;
    let resume_plan = f.resume.is_some();
    infra::outbox::write(st.pool(), &DomainEvent::CaseClosed { case_id: k.id, membership_id: k.membership_id, resume_plan }).await?;
    infra::outbox::audit(st.pool(), Some(p.membership_id.0), Some(k.school_id.0), "case_closed", serde_json::json!({ "case": id, "resume_plan": resume_plan })).await;
    Ok(go(&format!("/safeguarding/{id}")))
}
