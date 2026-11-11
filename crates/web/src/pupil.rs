//! Pupil pages: today, plan, books, Read-Review-Reflect lessons, reviews,
//! fixes, practice tests and the hints-first helper.

use crate::session::Session;
use crate::{ctx, go, render, AppState, Ctx, WebResult};
use assessment::application::{Attempts, ItemBank};
use assessment::domain::{Item, ItemKind};
use askama::Template;
use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use infra::read;
use kernel::{AppError, AttemptId, ConceptId, CourseId, DomainEvent, ItemId, MembershipId, MockId, Role};
use planning::application::Plans;
use planning::domain::StudyPreferences;
use serde::Deserialize;
use std::collections::HashMap;
use time::{Date, Duration};
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/home", get(home))
        .route("/plan", get(plan))
        .route("/plan/preferences", post(save_preferences))
        .route("/books", get(books))
        .route("/books/add", post(add_book))
        .route("/books/{course}", get(book))
        .route("/learn/{concept}", get(learn))
        .route("/review/{concept}", get(review))
        .route("/fix/{concept}", get(fix))
        .route("/practice/{concept}/{kind}/start", post(start_practice))
        .route("/attempt/{attempt}/q/{n}", get(question))
        .route("/attempt/{attempt}/answer", post(answer))
        .route("/attempt/{attempt}/hint/{item}/{n}", get(hint))
        .route("/learn/{concept}/reflect", post(reflect))
        .route("/learn/{concept}/ask", post(ask))
        .route("/tests", get(tests))
        .route("/tests/{mock}", get(test_intro))
        .route("/tests/{mock}/start", post(test_start))
        .route("/tests/attempt/{attempt}/finish", post(test_finish))
        .route("/report/{item}", post(report))
}

fn pupil(s: &Session) -> WebResult<&infra::auth::Principal> {
    s.require(&[Role::Student])?;
    Ok(&s.0)
}

fn today(st: &AppState) -> Date {
    st.clock.today()
}

// ------------------------------------------------------------------ view helpers
pub struct TaskLink {
    pub href: String,
    pub kind: String,
    pub verb: String,
    pub title: String,
    pub course: String,
    pub colour: String,
    pub minutes: i32,
    pub done: bool,
    pub day: String,
}

impl TaskLink {
    fn from(t: &read::TaskView) -> Self {
        let (href, verb) = match t.kind.as_str() {
            "learn" => (format!("/learn/{}", t.concept_id), "Learn"),
            "review" => (format!("/review/{}", t.concept_id), match t.stage.unwrap_or(1) { 1 => "Review 1 of 5", 2 => "Review 2 of 5", 3 => "Review 3 of 5", 4 => "Review 4 of 5", _ => "Review 5 of 5" }),
            "fix" => (format!("/fix/{}", t.concept_id), "Fix a mistake"),
            _ => ("/tests".to_string(), "Practice test"),
        };
        TaskLink {
            href,
            kind: t.kind.clone(),
            verb: verb.into(),
            title: t.concept_title.clone(),
            course: t.course_title.clone(),
            colour: t.colour.clone(),
            minutes: t.minutes,
            done: t.done,
            day: kernel::weekday_short(t.on_date).into(),
        }
    }
}

// ------------------------------------------------------------------ today
#[derive(Template)]
#[template(path = "pupil/home.html")]
struct Home {
    c: Ctx,
    first_name: String,
    date: String,
    tasks: Vec<TaskLink>,
    minutes_left: i32,
    next_index: usize,
    paused: bool,
    upcoming: Vec<TaskLink>,
}

async fn home(State(st): State<AppState>, s: Session) -> WebResult<Response> {
    let p = pupil(&s)?;
    let day = today(&st);
    if !read::plan_exists_from(st.pool(), p.membership_id, day).await.unwrap_or(false) {
        let _ = planning::application::rebuild(&st.repos, p.membership_id, day).await;
    }
    let all = read::tasks_between(st.pool(), p.membership_id, day, day + Duration::days(6)).await?;
    let tasks: Vec<TaskLink> = all.iter().filter(|t| t.on_date == day).map(TaskLink::from).collect();
    let upcoming: Vec<TaskLink> = all.iter().filter(|t| t.on_date > day && !t.done).take(4).map(TaskLink::from).collect();
    let minutes_left = tasks.iter().filter(|t| !t.done).map(|t| t.minutes).sum();
    let next_index = tasks.iter().position(|t| !t.done).unwrap_or(usize::MAX);
    let paused = read::plan_paused(st.pool(), p.membership_id.0).await?;
    let c = ctx(&st, p, "Today", "/home").await?;
    Ok(render(Home { c, first_name: p.first_name.clone(), date: kernel::long_date(day), tasks, minutes_left, next_index, paused, upcoming })?.into_response())
}

// ------------------------------------------------------------------ plan
pub struct DayCol {
    pub date: String,
    pub today: bool,
    pub study: bool,
    pub tasks: Vec<TaskLink>,
}

#[derive(Template)]
#[template(path = "pupil/plan.html")]
struct PlanPage {
    c: Ctx,
    days: Vec<DayCol>,
    prefs_days: Vec<(u8, &'static str, bool)>,
    minutes: i32,
    saved: bool,
    error: String,
}

impl PlanPage {
    fn minute_options(&self) -> Vec<i32> {
        vec![15, 20, 25, 30, 40, 45, 60]
    }
}

impl LearnPage {
    fn concept_id(&self) -> Uuid {
        self.page.concept_id
    }
}

impl HelperView {
    fn concept_id(&self) -> Uuid {
        self.concept
    }
}

async fn plan_page(st: &AppState, p: &infra::auth::Principal, saved: bool, error: String) -> WebResult<Response> {
    let day = today(st);
    let start = kernel::week_start(day);
    let prefs = Plans::preferences(&st.repos, p.membership_id).await?;
    let tasks = read::tasks_between(st.pool(), p.membership_id, start, start + Duration::days(6)).await?;
    let days = (0..7)
        .map(|i| {
            let d = start + Duration::days(i);
            DayCol {
                date: kernel::short_date(d),
                today: d == day,
                study: prefs.is_study_day(d),
                tasks: tasks.iter().filter(|t| t.on_date == d).map(TaskLink::from).collect(),
            }
        })
        .collect();
    const NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let prefs_days = (1..=7u8).map(|n| (n, NAMES[(n - 1) as usize], prefs.days.contains(&n))).collect();
    let c = ctx(st, p, "My plan", "/plan").await?;
    Ok(render(PlanPage { c, days, prefs_days, minutes: prefs.minutes, saved, error })?.into_response())
}

async fn plan(State(st): State<AppState>, s: Session) -> WebResult<Response> {
    let p = pupil(&s)?;
    plan_page(&st, p, false, String::new()).await
}

async fn save_preferences(State(st): State<AppState>, s: Session, Form(f): Form<Vec<(String, String)>>) -> WebResult<Response> {
    let p = pupil(&s)?;
    let days: Vec<u8> = f.iter().filter(|(k, _)| k == "day").filter_map(|(_, v)| v.parse().ok()).collect();
    let minutes: i32 = f.iter().find(|(k, _)| k == "minutes").and_then(|(_, v)| v.parse().ok()).unwrap_or(25);
    let prefs = StudyPreferences { days, minutes };
    if let Err(e) = prefs.validate() {
        return plan_page(&st, p, false, e.to_string()).await;
    }
    Plans::save_preferences(&st.repos, p.membership_id, &prefs).await?;
    planning::application::rebuild(&st.repos, p.membership_id, today(&st)).await?;
    infra::outbox::write(st.pool(), &DomainEvent::StudyPreferencesChanged { membership_id: p.membership_id }).await?;
    plan_page(&st, p, true, String::new()).await
}

// ------------------------------------------------------------------ books
#[derive(Template)]
#[template(path = "pupil/books.html")]
struct BooksPage {
    c: Ctx,
    books: Vec<read::BookView>,
    addable: Vec<(Uuid, String, String)>,
}

async fn books(State(st): State<AppState>, s: Session) -> WebResult<Response> {
    let p = pupil(&s)?;
    let books = read::books(st.pool(), p.membership_id).await?;
    let addable = read::addable_courses(st.pool(), p.membership_id, p.year_group.unwrap_or(6)).await?;
    let c = ctx(&st, p, "My books", "/books").await?;
    Ok(render(BooksPage { c, books, addable })?.into_response())
}

#[derive(Deserialize)]
struct AddBook {
    course: Uuid,
}

async fn add_book(State(st): State<AppState>, s: Session, Form(f): Form<AddBook>) -> WebResult<Response> {
    let p = pupil(&s)?;
    let year = p.year_group.unwrap_or(6);
    let ok = read::addable_courses(st.pool(), p.membership_id, year).await?.iter().any(|c| c.0 == f.course);
    if !ok {
        return Err(AppError::Forbidden.into());
    }
    read::enrol(st.pool(), p.membership_id, CourseId(f.course)).await?;
    infra::outbox::write(st.pool(), &DomainEvent::PupilEnrolled { membership_id: p.membership_id, course_id: CourseId(f.course) }).await?;
    planning::application::rebuild(&st.repos, p.membership_id, today(&st)).await?;
    Ok(go("/books"))
}

pub struct ConceptRow {
    pub id: Uuid,
    pub title: String,
    pub status: String,
    pub due: String,
}

#[derive(Template)]
#[template(path = "pupil/book.html")]
struct BookPage {
    c: Ctx,
    title: String,
    colour: String,
    concepts: Vec<ConceptRow>,
}

async fn book(State(st): State<AppState>, s: Session, Path(course): Path<Uuid>) -> WebResult<Response> {
    let p = pupil(&s)?;
    let b = read::books(st.pool(), p.membership_id).await?.into_iter().find(|b| b.course_id == course).ok_or(AppError::NotFound)?;
    let concepts = read::course_concepts(st.pool(), course).await?;
    let states: Vec<(Uuid, i16, Option<Date>)> = sqlx::query_as(
        "SELECT concept_id, stage, due_on FROM learner.concept_states WHERE membership_id = $1",
    )
    .bind(p.membership_id.0)
    .fetch_all(st.pool())
    .await
    .map_err(|e| AppError::Infra(e.to_string()))?;
    let concepts = concepts
        .into_iter()
        .map(|(id, title)| {
            let s = states.iter().find(|s| s.0 == id);
            let (status, due) = match s {
                None | Some((_, 0, _)) => ("Not started".to_string(), String::new()),
                Some((_, 6, _)) => ("Secured".to_string(), String::new()),
                Some((_, n, d)) => (format!("Review {n} of 5"), d.map(kernel::short_date).unwrap_or_default()),
            };
            ConceptRow { id, title, status, due }
        })
        .collect();
    let c = ctx(&st, p, &b.title, "/books").await?;
    Ok(render(BookPage { c, title: b.title, colour: b.colour, concepts })?.into_response())
}

// ------------------------------------------------------------------ lesson (Read, Review, Reflect)
#[derive(Template)]
#[template(path = "pupil/learn.html")]
struct LearnPage {
    c: Ctx,
    page: read::ConceptPage,
    mode: &'static str,
    heading: String,
    intro: String,
    chat: Vec<(String, String)>,
    ai_live: bool,
}

async fn concept_for(st: &AppState, p: &infra::auth::Principal, concept: Uuid) -> WebResult<read::ConceptPage> {
    if !read::can_study(st.pool(), p.membership_id, ConceptId(concept)).await? {
        return Err(AppError::Forbidden.into());
    }
    Ok(read::concept_page(st.pool(), ConceptId(concept)).await?)
}

async fn lesson_like(st: AppState, s: Session, concept: Uuid, mode: &'static str) -> WebResult<Response> {
    let p = pupil(&s)?;
    let page = concept_for(&st, p, concept).await?;
    let chat = read::helper_messages(st.pool(), p.membership_id, ConceptId(concept)).await?;
    let (heading, intro) = match mode {
        "review" => (format!("Review: {}", page.title), "Three quick questions to keep this in your memory. Get two right to move on.".to_string()),
        "fix" => (format!("Fix a mistake: {}", page.title), "You got one of these wrong recently. Read the reminder, then try again.".to_string()),
        _ => (page.title.clone(), String::new()),
    };
    let c = ctx(&st, p, &heading, "/home").await?;
    Ok(render(LearnPage { c, page, mode, heading, intro, chat, ai_live: st.ai.is_live() })?.into_response())
}

async fn learn(State(st): State<AppState>, s: Session, Path(concept): Path<Uuid>) -> WebResult<Response> {
    lesson_like(st, s, concept, "learn").await
}
async fn review(State(st): State<AppState>, s: Session, Path(concept): Path<Uuid>) -> WebResult<Response> {
    lesson_like(st, s, concept, "review").await
}
async fn fix(State(st): State<AppState>, s: Session, Path(concept): Path<Uuid>) -> WebResult<Response> {
    lesson_like(st, s, concept, "fix").await
}

struct AttemptRow {
    membership: Uuid,
    kind: String,
    concept: Option<Uuid>,
    mock: Option<Uuid>,
    finished: bool,
}

async fn attempt_row(st: &AppState, p: &infra::auth::Principal, a: Uuid) -> WebResult<AttemptRow> {
    let r: Option<(Uuid, String, Option<Uuid>, Option<Uuid>, bool)> = sqlx::query_as(
        "SELECT membership_id, kind, concept_id, mock_id, finished_at IS NOT NULL FROM assessment.attempts WHERE id = $1",
    )
    .bind(a)
    .fetch_optional(st.pool())
    .await
    .map_err(|e| AppError::Infra(e.to_string()))?;
    let r = r.ok_or(AppError::NotFound)?;
    if r.0 != p.membership_id.0 {
        return Err(AppError::Forbidden.into());
    }
    Ok(AttemptRow { membership: r.0, kind: r.1, concept: r.2, mock: r.3, finished: r.4 })
}

async fn attempt_items(st: &AppState, a: &AttemptRow) -> WebResult<Vec<Item>> {
    let concept = ConceptId(a.concept.ok_or(AppError::NotFound)?);
    let usage = if a.kind == "review" { "review" } else { "quiz" };
    let mut items = st.repos.items(concept, usage).await?;
    if a.kind == "review" {
        items.truncate(3);
    }
    if items.is_empty() {
        items = st.repos.items(concept, "quiz").await?;
    }
    Ok(items)
}

pub struct OptView {
    pub label: String,
    pub id: String,
}

#[derive(Template)]
#[template(path = "partials/question.html")]
struct QuestionView {
    attempt: Uuid,
    n: usize,
    total: usize,
    item_id: Uuid,
    prompt: String,
    choice: bool,
    numeric: bool,
    options: Vec<OptView>,
    has_hints: bool,
}

fn question_view(attempt: Uuid, n: usize, items: &[Item]) -> QuestionView {
    let it = &items[n];
    QuestionView {
        attempt,
        n,
        total: items.len(),
        item_id: it.id.0,
        prompt: it.prompt.clone(),
        choice: it.kind == ItemKind::Choice,
        numeric: it.kind == ItemKind::Numeric,
        options: it.options.iter().enumerate().map(|(i, o)| OptView { label: o.label.clone(), id: format!("o{n}-{i}") }).collect(),
        has_hints: !it.hints.is_empty(),
    }
}

#[derive(Deserialize)]
struct KindPath {
    concept: Uuid,
    kind: String,
}

async fn start_practice(State(st): State<AppState>, s: Session, Path(kp): Path<KindPath>) -> WebResult<Response> {
    let p = pupil(&s)?;
    let _ = concept_for(&st, p, kp.concept).await?;
    let kind = match kp.kind.as_str() {
        "review" => "review",
        "fix" => "fix",
        _ => "lesson",
    };
    let a = Attempts::start(&st.repos, p.membership_id, kind, Some(ConceptId(kp.concept)), None).await?;
    let row = attempt_row(&st, p, a.0).await?;
    let items = attempt_items(&st, &row).await?;
    if items.is_empty() {
        return Ok(Html("<p class=\"note\">There are no questions for this yet.</p>".to_string()).into_response());
    }
    Ok(render(question_view(a.0, 0, &items))?.into_response())
}

async fn question(State(st): State<AppState>, s: Session, Path((attempt, n)): Path<(Uuid, usize)>) -> WebResult<Response> {
    let p = pupil(&s)?;
    let row = attempt_row(&st, p, attempt).await?;
    let items = attempt_items(&st, &row).await?;
    if n >= items.len() {
        return finish_practice(&st, p, attempt, &row, &items).await;
    }
    Ok(render(question_view(attempt, n, &items))?.into_response())
}

#[derive(Template)]
#[template(path = "partials/feedback.html")]
struct FeedbackView {
    attempt: Uuid,
    item_id: Uuid,
    correct: bool,
    given: String,
    answer: String,
    explanation: String,
    misconception: String,
    next: usize,
    last: bool,
}

#[derive(Deserialize)]
struct AnswerForm {
    item: Uuid,
    n: usize,
    #[serde(default)]
    answer: String,
}

async fn answer(State(st): State<AppState>, s: Session, Path(attempt): Path<Uuid>, Form(f): Form<AnswerForm>) -> WebResult<Response> {
    let p = pupil(&s)?;
    let row = attempt_row(&st, p, attempt).await?;
    if row.finished {
        return Ok(go("/home"));
    }
    let items = attempt_items(&st, &row).await?;
    let item = items.iter().find(|i| i.id.0 == f.item).ok_or(AppError::NotFound)?;
    if f.answer.trim().is_empty() {
        let mut q = question_view(attempt, f.n.min(items.len() - 1), &items);
        q.prompt = format!("{} (Choose or type an answer first.)", q.prompt);
        return Ok(render(q)?.into_response());
    }
    let marked = item.mark(&f.answer);
    Attempts::record_response(&st.repos, AttemptId(attempt), item.id, f.answer.trim(), marked.correct, marked.misconception.as_deref()).await?;
    // Reviews update the ladder once, at the end; lessons and fixes update mastery per answer.
    infra::outbox::write(
        st.pool(),
        &DomainEvent::AnswerMarked {
            membership_id: p.membership_id,
            concept_id: item.concept_id,
            item_id: item.id,
            correct: marked.correct,
            misconception: marked.misconception.clone(),
        },
    )
    .await?;
    let misconception = if marked.correct {
        String::new()
    } else {
        match &marked.misconception {
            Some(m) => misconception_text(m),
            None => String::new(),
        }
    };
    Ok(render(FeedbackView {
        attempt,
        item_id: item.id.0,
        correct: marked.correct,
        given: f.answer.trim().to_string(),
        answer: item.display_answer().to_string(),
        explanation: item.explanation.clone(),
        misconception,
        next: f.n + 1,
        last: f.n + 1 >= items.len(),
    })?
    .into_response())
}

/// The misconception note written for this distractor.
fn misconception_text(note: &str) -> String {
    note.trim().to_string()
}

#[derive(Template)]
#[template(path = "partials/hint.html")]
struct HintView {
    shown: Vec<String>,
    more: bool,
    attempt: Uuid,
    item: Uuid,
    next: usize,
}

async fn hint(State(st): State<AppState>, s: Session, Path((attempt, item, n)): Path<(Uuid, Uuid, usize)>) -> WebResult<Response> {
    let p = pupil(&s)?;
    let row = attempt_row(&st, p, attempt).await?;
    let items = attempt_items(&st, &row).await?;
    let it = items.iter().find(|i| i.id.0 == item).ok_or(AppError::NotFound)?;
    let mut ladder = tutoring::domain::HintLadder::new(it.hints.clone());
    let mut shown = Vec::new();
    for _ in 0..=n {
        if let Some(h) = ladder.next_hint() {
            shown.push(h.to_string());
        }
    }
    let more = shown.len() < it.hints.len();
    Ok(render(HintView { shown, more, attempt, item, next: n + 1 })?.into_response())
}

#[derive(Template)]
#[template(path = "partials/practice_done.html")]
struct PracticeDone {
    kind: String,
    concept: Uuid,
    right: i64,
    total: i64,
    passed: bool,
    reflect_prompt: String,
}

async fn finish_practice(st: &AppState, p: &infra::auth::Principal, attempt: Uuid, row: &AttemptRow, items: &[Item]) -> WebResult<Response> {
    let concept = row.concept.ok_or(AppError::NotFound)?;
    let (right, total): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE correct), count(*) FROM assessment.responses WHERE attempt_id = $1",
    )
    .bind(attempt)
    .fetch_one(st.pool())
    .await
    .map_err(|e| AppError::Infra(e.to_string()))?;
    let total = total.max(items.len() as i64);
    let day = today(st);
    let mut passed = right * 3 >= total * 2;
    if !row.finished {
        Attempts::finish(&st.repos, AttemptId(attempt), right as i32, total as i32).await?;
        let m = MembershipId(row.membership);
        match row.kind.as_str() {
            "review" => {
                passed = assessment::domain::review_passed(right as usize, total as usize);
                infra::outbox::write(st.pool(), &DomainEvent::ReviewCompleted { membership_id: m, concept_id: ConceptId(concept), passed, on: day }).await?;
                read::complete_tasks_for(st.pool(), m, ConceptId(concept), &["review"], day).await?;
            }
            "fix" => {
                if passed {
                    read::mark_misconceptions_fixed(st.pool(), m, ConceptId(concept)).await?;
                }
                read::complete_tasks_for(st.pool(), m, ConceptId(concept), &["fix"], day).await?;
            }
            _ => {}
        }
    }
    let page = read::concept_page(st.pool(), ConceptId(concept)).await?;
    let _ = p;
    Ok(render(PracticeDone { kind: row.kind.clone(), concept, right, total, passed, reflect_prompt: page.lesson.reflect_prompt })?.into_response())
}

#[derive(Template)]
#[template(path = "partials/reflected.html")]
struct Reflected {
    feedback: String,
    safeguarding: String,
}

#[derive(Deserialize)]
struct ReflectForm {
    body: String,
    #[serde(default)]
    confidence: Option<i16>,
}

async fn reflect(State(st): State<AppState>, s: Session, Path(concept): Path<Uuid>, Form(f): Form<ReflectForm>) -> WebResult<Response> {
    let p = pupil(&s)?;
    let page = concept_for(&st, p, concept).await?;
    let confidence = f.confidence.unwrap_or(2).clamp(1, 3);
    let body = f.body.trim();
    let mut safeguarding = String::new();
    if let Some(flag) = safeguarding::domain::screen(body) {
        raise_flag(&st, p, &flag, body).await?;
        safeguarding = safeguarding::domain::pupil_response(flag.severity).to_string();
    }
    let ideas: Vec<(&str, &str)> = page.lesson.reflect_ideas.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let feedback = tutoring::domain::reflection_feedback(body, &ideas);
    read::save_reflection(st.pool(), p.membership_id, ConceptId(concept), body, confidence, &feedback).await?;
    let day = today(&st);
    infra::outbox::write(st.pool(), &DomainEvent::ReflectionSubmitted { membership_id: p.membership_id, concept_id: ConceptId(concept), confidence: confidence as u8 }).await?;
    infra::outbox::write(st.pool(), &DomainEvent::LessonCompleted { membership_id: p.membership_id, concept_id: ConceptId(concept), on: day }).await?;
    read::complete_tasks_for(st.pool(), p.membership_id, ConceptId(concept), &["learn"], day).await?;
    Ok(render(Reflected { feedback, safeguarding })?.into_response())
}

async fn raise_flag(st: &AppState, p: &infra::auth::Principal, flag: &safeguarding::domain::RiskFlag, text: &str) -> WebResult<()> {
    let school = p.school()?;
    let excerpt: String = text.chars().take(280).collect();
    infra::outbox::write(
        st.pool(),
        &DomainEvent::RiskFlagRaised {
            membership_id: p.membership_id,
            school_id: school,
            severity: flag.severity.as_str().into(),
            category: flag.category.into(),
            excerpt,
        },
    )
    .await?;
    Ok(())
}

#[derive(Template)]
#[template(path = "partials/helper.html")]
struct HelperView {
    concept: Uuid,
    chat: Vec<(String, String)>,
    ai_live: bool,
}

#[derive(Deserialize)]
struct AskForm {
    question: String,
}

async fn ask(State(st): State<AppState>, s: Session, Path(concept): Path<Uuid>, Form(f): Form<AskForm>) -> WebResult<Response> {
    let p = pupil(&s)?;
    let page = concept_for(&st, p, concept).await?;
    let q: String = f.question.trim().chars().take(500).collect();
    if !q.is_empty() {
        let c = ConceptId(concept);
        let flag = safeguarding::domain::screen(&q);
        read::save_message(st.pool(), p.membership_id, Some(c), "pupil", &q, flag.is_some()).await?;
        let reply = if let Some(flag) = flag {
            raise_flag(&st, p, &flag, &q).await?;
            safeguarding::domain::pupil_response(flag.severity).to_string()
        } else {
            let notes = format!("{}\n{}", page.lesson.read.join("\n"), page.lesson.steps.join("\n"));
            let system = tutoring::domain::helper_system_prompt(page.year_group, &page.title, &notes);
            match st.ai.reply(&system, &q, "helper").await {
                Ok(Some(r)) => r,
                _ => fallback_hint(&page, &q),
            }
        };
        read::save_message(st.pool(), p.membership_id, Some(c), "helper", &reply, false).await?;
    }
    let chat = read::helper_messages(st.pool(), p.membership_id, ConceptId(concept)).await?;
    Ok(render(HelperView { concept, chat, ai_live: st.ai.is_live() })?.into_response())
}

/// Offline helper: points to the right step of the method instead of answering.
fn fallback_hint(page: &read::ConceptPage, q: &str) -> String {
    let ql = q.to_lowercase();
    let words: Vec<&str> = ql.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 3).collect();
    let best = page
        .lesson
        .steps
        .iter()
        .enumerate()
        .max_by_key(|(_, s)| {
            let sl = s.to_lowercase();
            words.iter().filter(|w| sl.contains(**w)).count()
        })
        .map(|(i, s)| (i, s.clone()));
    match best {
        Some((i, step)) => format!("Look at step {} of the method: \"{}\" Try just that step, then check your working against the worked example.", i + 1, step),
        None => "Read the worked example again and try the first step on your own. Use the hint button on a question if you're stuck.".into(),
    }
}

// ------------------------------------------------------------------ practice tests
#[derive(Template)]
#[template(path = "pupil/tests.html")]
struct TestsPage {
    c: Ctx,
    mocks: Vec<read::MockView>,
}

async fn tests(State(st): State<AppState>, s: Session) -> WebResult<Response> {
    let p = pupil(&s)?;
    let mocks = read::mocks_for(st.pool(), p.membership_id).await?;
    let c = ctx(&st, p, "Practice tests", "/tests").await?;
    Ok(render(TestsPage { c, mocks })?.into_response())
}

#[derive(Template)]
#[template(path = "pupil/test.html")]
struct TestPage {
    c: Ctx,
    mock: Uuid,
    title: String,
    minutes: i32,
    started: bool,
    attempt: Uuid,
    deadline_ms: i64,
    questions: Vec<QuestionView>,
}

async fn test_intro(State(st): State<AppState>, s: Session, Path(mock): Path<Uuid>) -> WebResult<Response> {
    let p = pupil(&s)?;
    if !read::mock_allowed(st.pool(), p.membership_id, mock).await? {
        return Err(AppError::Forbidden.into());
    }
    let paper = ItemBank::mock(&st.repos, MockId(mock)).await?;
    let c = ctx(&st, p, &paper.title, "/tests").await?;
    Ok(render(TestPage { c, mock, title: paper.title, minutes: paper.minutes, started: false, attempt: Uuid::nil(), deadline_ms: 0, questions: vec![] })?.into_response())
}

async fn test_start(State(st): State<AppState>, s: Session, Path(mock): Path<Uuid>) -> WebResult<Response> {
    let p = pupil(&s)?;
    if !read::mock_allowed(st.pool(), p.membership_id, mock).await? {
        return Err(AppError::Forbidden.into());
    }
    let paper = ItemBank::mock(&st.repos, MockId(mock)).await?;
    let a = Attempts::start(&st.repos, p.membership_id, "mock", None, Some(MockId(mock))).await?;
    let deadline_ms = (time::OffsetDateTime::now_utc() + Duration::minutes(paper.minutes as i64)).unix_timestamp() * 1000;
    let questions = (0..paper.items.len()).map(|i| question_view(a.0, i, &paper.items)).collect();
    let c = ctx(&st, p, &paper.title, "/tests").await?;
    Ok(render(TestPage { c, mock, title: paper.title, minutes: paper.minutes, started: true, attempt: a.0, deadline_ms, questions })?.into_response())
}

pub struct ResultRow {
    pub n: usize,
    pub prompt: String,
    pub given: String,
    pub answer: String,
    pub correct: bool,
    pub explanation: String,
}

#[derive(Template)]
#[template(path = "pupil/test_result.html")]
struct TestResult {
    c: Ctx,
    title: String,
    score: i32,
    max: i32,
    scaled: String,
    rows: Vec<ResultRow>,
}

async fn test_finish(State(st): State<AppState>, s: Session, Path(attempt): Path<Uuid>, Form(f): Form<HashMap<String, String>>) -> WebResult<Response> {
    let p = pupil(&s)?;
    let row = attempt_row(&st, p, attempt).await?;
    let mock = row.mock.ok_or(AppError::NotFound)?;
    let paper = ItemBank::mock(&st.repos, MockId(mock)).await?;
    let mut score = 0;
    let max: i32 = paper.items.iter().map(|i| i.marks).sum();
    let mut rows = Vec::new();
    for (n, it) in paper.items.iter().enumerate() {
        let given = f.get(&format!("a_{}", it.id.0)).map(|s| s.trim().to_string()).unwrap_or_default();
        let marked = it.mark(&given);
        if !row.finished {
            Attempts::record_response(&st.repos, AttemptId(attempt), it.id, &given, marked.correct, marked.misconception.as_deref()).await?;
            if !given.is_empty() {
                infra::outbox::write(
                    st.pool(),
                    &DomainEvent::AnswerMarked { membership_id: p.membership_id, concept_id: it.concept_id, item_id: it.id, correct: marked.correct, misconception: marked.misconception.clone() },
                )
                .await?;
            }
        }
        score += marked.marks_awarded;
        rows.push(ResultRow { n: n + 1, prompt: it.prompt.clone(), given, answer: it.display_answer().into(), correct: marked.correct, explanation: it.explanation.clone() });
    }
    if !row.finished {
        Attempts::finish(&st.repos, AttemptId(attempt), score, max).await?;
        infra::outbox::write(st.pool(), &DomainEvent::MockCompleted { membership_id: p.membership_id, attempt_id: AttemptId(attempt), score, max_score: max }).await?;
        sqlx::query("UPDATE planning.tasks SET done_at = now() WHERE membership_id = $1 AND kind = 'test' AND done_at IS NULL AND on_date <= $2")
            .bind(p.membership_id.0)
            .bind(today(&st))
            .execute(st.pool())
            .await
            .map_err(|e| AppError::Infra(e.to_string()))?;
    }
    let scaled = if paper.title.to_lowercase().contains("arithmetic") || paper.items.len() >= 10 {
        let (lo, hi) = assessment::domain::ks2_scaled_range(score, max);
        format!("If this were the real paper, a score like this usually sits around {lo} to {hi} on the SATs scale (100 is the expected standard). Treat it as a rough guide.")
    } else {
        String::new()
    };
    let c = ctx(&st, p, "Test results", "/tests").await?;
    Ok(render(TestResult { c, title: paper.title, score, max, scaled, rows })?.into_response())
}

// ------------------------------------------------------------------ report a question
#[derive(Deserialize)]
struct ReportForm {
    #[serde(default)]
    note: String,
}

async fn report(State(st): State<AppState>, s: Session, Path(item): Path<Uuid>, Form(f): Form<ReportForm>) -> WebResult<Response> {
    let p = pupil(&s)?;
    let _ = ItemBank::item(&st.repos, ItemId(item)).await?;
    let note: String = f.note.chars().take(300).collect();
    read::report_item(st.pool(), p.membership_id, item, if note.trim().is_empty() { "Pupil reported a problem" } else { &note }).await?;
    Ok(Html("<p class=\"small\" role=\"status\">Thanks. A reviewer will check this question.</p>".to_string()).into_response())
}
