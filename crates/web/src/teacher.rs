//! Teacher pages: classes, class heatmap, needs attention, weekly review, pupil detail.

use crate::session::Session;
use crate::{ctx, render, AppState, Ctx, WebResult};
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use infra::auth::Principal;
use infra::read;
use kernel::{AppError, Role};
use serde::Deserialize;
use time::Duration;
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/teach", get(classes))
        .route("/teach/class/{class}", get(class))
        .route("/teach/pupil/{m}", get(pupil))
        .route("/teach/pupil/{m}/reset", post(reset_password))
        .route("/teach/pupil/{m}/task", post(set_work))
}

fn staff(s: &Session) -> WebResult<&Principal> {
    s.require(&[Role::Teacher, Role::SchoolAdmin])?;
    Ok(&s.0)
}

/// Teachers see their own classes; school admins see every class in the school.
async fn may_see_class(st: &AppState, p: &Principal, class: Uuid) -> WebResult<read::ClassView> {
    let school = p.school()?;
    let all = p.role == Role::SchoolAdmin;
    read::classes_for(st.pool(), school, p.membership_id, all)
        .await?
        .into_iter()
        .find(|c| c.id == class)
        .ok_or_else(|| AppError::Forbidden.into())
}

async fn may_see_pupil(st: &AppState, p: &Principal, m: Uuid) -> WebResult<read::PupilRow> {
    let school = p.school()?;
    let pupil = read::pupil_in_school(st.pool(), school, m).await.map_err(|_| AppError::NotFound)?;
    if p.role == Role::SchoolAdmin {
        return Ok(pupil);
    }
    let shared: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM identity.class_members a JOIN identity.class_members b ON a.class_id = b.class_id
                        WHERE a.membership_id = $1 AND b.membership_id = $2)",
    )
    .bind(p.membership_id.0)
    .bind(m)
    .fetch_one(st.pool())
    .await
    .map_err(|e| AppError::Infra(e.to_string()))?;
    if shared { Ok(pupil) } else { Err(AppError::Forbidden.into()) }
}

#[derive(Template)]
#[template(path = "teacher/classes.html")]
struct ClassesPage {
    c: Ctx,
    classes: Vec<read::ClassView>,
}

async fn classes(State(st): State<AppState>, s: Session) -> WebResult<Response> {
    let p = staff(&s)?;
    let classes = read::classes_for(st.pool(), p.school()?, p.membership_id, p.role == Role::SchoolAdmin).await?;
    if classes.len() == 1 && p.role == Role::Teacher {
        return Ok(crate::go(&format!("/teach/class/{}", classes[0].id)));
    }
    let c = ctx(&st, p, "My classes", "/teach").await?;
    Ok(render(ClassesPage { c, classes })?.into_response())
}

pub struct Cell {
    pub level: u8,
    pub pct: i64,
    pub label: &'static str,
}

pub struct HeatRow {
    pub id: Uuid,
    pub name: String,
    pub cells: Vec<Cell>,
}

#[derive(Template)]
#[template(path = "teacher/class.html")]
struct ClassPage {
    c: Ctx,
    class: read::ClassView,
    courses: Vec<(Uuid, String, String)>,
    course: Uuid,
    concepts: Vec<(Uuid, String)>,
    rows: Vec<HeatRow>,
    attention: Vec<(Uuid, String, String)>,
    review: String,
    week: String,
    studied: i64,
    pupils: i64,
}

#[derive(Deserialize)]
struct CourseQ {
    course: Option<Uuid>,
}

const LEVELS: [&str; 5] = ["Not started", "Learning", "Reviewing", "Nearly secure", "Secured"];

async fn class(State(st): State<AppState>, s: Session, Path(class_id): Path<Uuid>, Query(q): Query<CourseQ>) -> WebResult<Response> {
    let p = staff(&s)?;
    let class = may_see_class(&st, p, class_id).await?;
    let courses = read::courses_for_year(st.pool(), class.year_group).await?;
    let course = q.course.filter(|c| courses.iter().any(|x| x.0 == *c)).or_else(|| courses.first().map(|c| c.0)).unwrap_or(Uuid::nil());
    let hm = read::heatmap(st.pool(), class_id, course).await?;
    let rows = hm
        .rows
        .into_iter()
        .map(|(pr, cells)| HeatRow {
            id: pr.id,
            name: pr.name(),
            cells: cells.into_iter().map(|(l, m)| Cell { level: l.min(4), pct: (m * 100.0).round() as i64, label: LEVELS[l.min(4) as usize] }).collect(),
        })
        .collect();
    let today = st.clock.today();
    let attention = read::needs_attention(st.pool(), class_id, today).await?;
    let week = kernel::week_start(today);
    let last_week = week - Duration::days(7);
    let (pupils, studied) = read::class_studied(st.pool(), class_id, last_week, week - Duration::days(1)).await?;
    let review = match read::weekly_review(st.pool(), class_id, week).await? {
        Some(r) => r,
        None => {
            let mistakes = read::class_mistakes(st.pool(), class_id, 7).await?;
            reporting::domain::class_review(&class.name, pupils as usize, studied as usize, &mistakes)
        }
    };
    let title = format!("Class {}", class.name);
    let c = ctx(&st, p, &title, "/teach").await?;
    Ok(render(ClassPage { c, class, courses, course, concepts: hm.concepts, rows, attention, review, week: kernel::long_date(week), studied, pupils })?.into_response())
}

pub struct StateRow {
    pub concept: String,
    pub course: String,
    pub pct: i64,
    pub stage: String,
    pub due: String,
    pub lapses: i32,
}

pub struct RespRow {
    pub prompt: String,
    pub answer: String,
    pub correct: bool,
    pub misconception: String,
    pub when: String,
}

#[derive(Template)]
#[template(path = "teacher/pupil.html")]
struct PupilPage {
    c: Ctx,
    pupil: read::PupilRow,
    states: Vec<StateRow>,
    responses: Vec<RespRow>,
    reflections: Vec<(String, String, i16, String)>,
    week: reporting::domain::PupilWeek,
    concepts: Vec<(Uuid, String)>,
    new_password: String,
    message: String,
    paused: bool,
}

fn stage_text(stage: i16) -> String {
    match stage {
        0 => "Not started".into(),
        6 => "Secured".into(),
        n => format!("Review {n} of 5"),
    }
}

async fn pupil_page(st: &AppState, p: &Principal, m: Uuid, new_password: String, message: String) -> WebResult<Response> {
    let pupil = may_see_pupil(st, p, m).await?;
    let states = read::pupil_states(st.pool(), m)
        .await?
        .into_iter()
        .map(|s| StateRow {
            concept: s.concept_title,
            course: s.course_title,
            pct: (s.mastery * 100.0).round() as i64,
            stage: stage_text(s.stage),
            due: s.due_on.map(kernel::short_date).unwrap_or_default(),
            lapses: s.lapses,
        })
        .collect();
    let responses = read::recent_responses(st.pool(), m, 12)
        .await?
        .into_iter()
        .map(|r| RespRow {
            prompt: r.prompt,
            answer: r.answer,
            correct: r.correct,
            misconception: r.misconception.unwrap_or_default(),
            when: kernel::short_date(r.created_at.date()),
        })
        .collect();
    let reflections = read::recent_reflections(st.pool(), m).await?;
    let week = read::pupil_week(st.pool(), m, kernel::week_start(st.clock.today())).await?;
    let concepts: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT c.id, co.title || ': ' || c.title FROM planning.enrolments e JOIN curriculum.courses co ON co.id = e.course_id
           JOIN curriculum.concepts c ON c.course_id = co.id WHERE e.membership_id = $1 ORDER BY co.title, c.position",
    )
    .bind(m)
    .fetch_all(st.pool())
    .await
    .map_err(|e| AppError::Infra(e.to_string()))?;
    let paused = read::plan_paused(st.pool(), m).await?;
    let title = pupil.name();
    let c = ctx(st, p, &title, "/teach").await?;
    Ok(render(PupilPage { c, pupil, states, responses, reflections, week, concepts, new_password, message, paused })?.into_response())
}

async fn pupil(State(st): State<AppState>, s: Session, Path(m): Path<Uuid>) -> WebResult<Response> {
    let p = staff(&s)?;
    pupil_page(&st, p, m, String::new(), String::new()).await
}

async fn reset_password(State(st): State<AppState>, s: Session, Path(m): Path<Uuid>) -> WebResult<Response> {
    let p = staff(&s)?;
    let _ = may_see_pupil(&st, p, m).await?;
    let pw = infra::auth::reset_student_password(st.pool(), p.school()?, kernel::MembershipId(m)).await?;
    infra::outbox::audit(st.pool(), Some(p.membership_id.0), p.school_id.map(|s| s.0), "pupil_password_reset", serde_json::json!({ "pupil": m })).await;
    pupil_page(&st, p, m, pw, String::new()).await
}

#[derive(Deserialize)]
struct SetWork {
    concept: Uuid,
    kind: String,
}

async fn set_work(State(st): State<AppState>, s: Session, Path(m): Path<Uuid>, Form(f): Form<SetWork>) -> WebResult<Response> {
    let p = staff(&s)?;
    let _ = may_see_pupil(&st, p, m).await?;
    if !read::can_study(st.pool(), kernel::MembershipId(m), kernel::ConceptId(f.concept)).await? {
        return Err(AppError::Invalid("That topic isn't in this pupil's books.".into()).into());
    }
    let (kind, minutes) = match f.kind.as_str() {
        "review" => ("review", 4),
        "fix" => ("fix", 5),
        _ => ("learn", 12),
    };
    read::add_task(st.pool(), m, f.concept, kind, minutes, st.clock.today()).await?;
    infra::outbox::audit(st.pool(), Some(p.membership_id.0), p.school_id.map(|s| s.0), "work_set", serde_json::json!({ "pupil": m, "concept": f.concept, "kind": kind })).await;
    pupil_page(&st, p, m, String::new(), "Added to today's plan.".into()).await
}
