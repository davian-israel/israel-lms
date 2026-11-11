//! Read side (CQRS-lite): queries that build the page view models. These read
//! across context schemas on purpose; they never change aggregate state except
//! the few small commands grouped at the bottom.

use crate::db_err;
use content::domain::{ContentStatus, Lesson};
use kernel::{AppError, AppResult, ConceptId, CourseId, LessonId, MembershipId, SchoolId};
use reporting::domain::PupilWeek;
use serde::Deserialize;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use time::{Date, Duration};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TaskView {
    pub id: Uuid,
    pub on_date: Date,
    pub kind: String,
    pub concept_id: Uuid,
    pub concept_title: String,
    pub course_title: String,
    pub colour: String,
    pub minutes: i32,
    pub done: bool,
    pub stage: Option<i16>,
}

const TASK_SQL: &str = "SELECT t.id, t.on_date, t.kind, t.concept_id, c.title AS concept_title, co.title AS course_title,
        co.colour, t.minutes, (t.done_at IS NOT NULL) AS done, s.stage
   FROM planning.tasks t
   JOIN curriculum.concepts c ON c.id = t.concept_id
   JOIN curriculum.courses co ON co.id = c.course_id
   LEFT JOIN learner.concept_states s ON s.membership_id = t.membership_id AND s.concept_id = t.concept_id";

pub async fn tasks_between(pool: &PgPool, m: MembershipId, from: Date, to: Date) -> AppResult<Vec<TaskView>> {
    sqlx::query_as(&format!("{TASK_SQL} WHERE t.membership_id = $1 AND t.on_date BETWEEN $2 AND $3 ORDER BY t.on_date, t.position"))
        .bind(m.0)
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await
        .map_err(db_err)
}

pub async fn task(pool: &PgPool, m: MembershipId, id: Uuid) -> AppResult<TaskView> {
    sqlx::query_as(&format!("{TASK_SQL} WHERE t.membership_id = $1 AND t.id = $2"))
        .bind(m.0)
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(db_err)
}

pub async fn plan_exists_from(pool: &PgPool, m: MembershipId, day: Date) -> AppResult<bool> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM planning.tasks WHERE membership_id = $1 AND on_date >= $2)")
        .bind(m.0)
        .bind(day)
        .fetch_one(pool)
        .await
        .map_err(db_err)
}

pub async fn complete_task(pool: &PgPool, m: MembershipId, id: Uuid) -> AppResult<()> {
    sqlx::query("UPDATE planning.tasks SET done_at = now() WHERE id = $1 AND membership_id = $2 AND done_at IS NULL")
        .bind(id)
        .bind(m.0)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(())
}

pub async fn complete_tasks_for(pool: &PgPool, m: MembershipId, concept: ConceptId, kinds: &[&str], day: Date) -> AppResult<()> {
    let kinds: Vec<String> = kinds.iter().map(|s| s.to_string()).collect();
    sqlx::query(
        "UPDATE planning.tasks SET done_at = now()
          WHERE membership_id = $1 AND concept_id = $2 AND kind = ANY($3) AND on_date <= $4 AND done_at IS NULL",
    )
    .bind(m.0)
    .bind(concept.0)
    .bind(&kinds)
    .bind(day)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(())
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BookView {
    pub course_id: Uuid,
    pub title: String,
    pub subtitle: String,
    pub colour: String,
    pub year_group: i32,
    pub secured: i64,
    pub started: i64,
    pub total: i64,
}

impl BookView {
    pub fn pct(&self) -> i64 {
        if self.total == 0 { 0 } else { self.secured * 100 / self.total }
    }
}

pub async fn books(pool: &PgPool, m: MembershipId) -> AppResult<Vec<BookView>> {
    sqlx::query_as(
        "SELECT co.id AS course_id, co.title, co.exam AS subtitle, co.colour, co.year_group,
                count(s.concept_id) FILTER (WHERE s.stage >= 6) AS secured,
                count(s.concept_id) FILTER (WHERE s.stage >= 1) AS started,
                count(c.id) AS total
           FROM planning.enrolments e
           JOIN curriculum.courses co ON co.id = e.course_id
           JOIN curriculum.concepts c ON c.course_id = co.id
           LEFT JOIN learner.concept_states s ON s.concept_id = c.id AND s.membership_id = e.membership_id
          WHERE e.membership_id = $1
          GROUP BY co.id ORDER BY co.title",
    )
    .bind(m.0)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn addable_courses(pool: &PgPool, m: MembershipId, year: i32) -> AppResult<Vec<(Uuid, String, String)>> {
    sqlx::query_as(
        "SELECT co.id, co.title, co.exam FROM curriculum.courses co
          WHERE abs(co.year_group - $2) <= 1
            AND NOT EXISTS (SELECT 1 FROM planning.enrolments e WHERE e.membership_id = $1 AND e.course_id = co.id)
          ORDER BY co.title",
    )
    .bind(m.0)
    .bind(year)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn enrol(pool: &PgPool, m: MembershipId, course: CourseId) -> AppResult<()> {
    sqlx::query("INSERT INTO planning.enrolments (membership_id, course_id) VALUES ($1,$2) ON CONFLICT DO NOTHING")
        .bind(m.0)
        .bind(course.0)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
struct LessonBody {
    read: Vec<String>,
    steps: Vec<String>,
    check: String,
    worked_example: Vec<(String, String)>,
    reflect_prompt: String,
    reflect_ideas: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ConceptPage {
    pub concept_id: Uuid,
    pub title: String,
    pub unit: String,
    pub course_title: String,
    pub colour: String,
    pub year_group: i32,
    pub lesson: Lesson,
    pub flashcards: Vec<(String, String)>,
}

pub async fn concept_page(pool: &PgPool, concept: ConceptId) -> AppResult<ConceptPage> {
    let (title, unit, course_title, colour, year_group, lesson_id, ltitle, body, status, ai): (
        String, String, String, String, i32, Uuid, String, serde_json::Value, String, bool,
    ) = sqlx::query_as(
        "SELECT c.title, c.unit, co.title, co.colour, co.year_group, l.id, l.title, l.body, l.status, l.ai_generated
           FROM curriculum.concepts c JOIN curriculum.courses co ON co.id = c.course_id
           JOIN content.lessons l ON l.concept_id = c.id
          WHERE c.id = $1",
    )
    .bind(concept.0)
    .fetch_one(pool)
    .await
    .map_err(db_err)?;
    let b: LessonBody = serde_json::from_value(body).map_err(|e| AppError::Infra(e.to_string()))?;
    let status = ContentStatus::parse(&status);
    if status != ContentStatus::Live {
        return Err(AppError::NotFound);
    }
    let flashcards: Vec<(String, String)> =
        sqlx::query_as("SELECT front, back FROM content.flashcards WHERE concept_id = $1 ORDER BY front")
            .bind(concept.0)
            .fetch_all(pool)
            .await
            .map_err(db_err)?;
    Ok(ConceptPage {
        concept_id: concept.0,
        title,
        unit,
        course_title,
        colour,
        year_group,
        lesson: Lesson {
            id: LessonId(lesson_id),
            concept_id: concept,
            title: ltitle,
            read: b.read,
            steps: b.steps,
            check: b.check,
            worked_example: b.worked_example,
            reflect_prompt: b.reflect_prompt,
            reflect_ideas: b.reflect_ideas,
            status,
            ai_generated: ai,
        },
        flashcards,
    })
}

/// Is this pupil enrolled in the course that owns this concept?
pub async fn can_study(pool: &PgPool, m: MembershipId, concept: ConceptId) -> AppResult<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM planning.enrolments e JOIN curriculum.concepts c ON c.course_id = e.course_id
                        WHERE e.membership_id = $1 AND c.id = $2)",
    )
    .bind(m.0)
    .bind(concept.0)
    .fetch_one(pool)
    .await
    .map_err(db_err)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MockView {
    pub id: Uuid,
    pub title: String,
    pub minutes: i32,
    pub course_title: String,
    pub items: i32,
    pub best: Option<i32>,
    pub max_score: Option<i32>,
}

pub async fn mocks_for(pool: &PgPool, m: MembershipId) -> AppResult<Vec<MockView>> {
    sqlx::query_as(
        "SELECT mo.id, mo.title, mo.minutes, co.title AS course_title, cardinality(mo.item_ids) AS items,
                (SELECT max(score) FROM assessment.attempts a WHERE a.mock_id = mo.id AND a.membership_id = $1 AND a.finished_at IS NOT NULL) AS best,
                (SELECT max(max_score) FROM assessment.attempts a WHERE a.mock_id = mo.id AND a.membership_id = $1) AS max_score
           FROM assessment.mocks mo
           JOIN curriculum.courses co ON co.id = mo.course_id
           JOIN planning.enrolments e ON e.course_id = co.id AND e.membership_id = $1
          ORDER BY co.title",
    )
    .bind(m.0)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn mock_allowed(pool: &PgPool, m: MembershipId, mock: Uuid) -> AppResult<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM assessment.mocks mo JOIN planning.enrolments e ON e.course_id = mo.course_id
                        WHERE mo.id = $2 AND e.membership_id = $1)",
    )
    .bind(m.0)
    .bind(mock)
    .fetch_one(pool)
    .await
    .map_err(db_err)
}

// ------------------------------------------------------------------ staff
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ClassView {
    pub id: Uuid,
    pub name: String,
    pub year_group: i32,
    pub pupils: i64,
}

pub async fn classes_for(pool: &PgPool, school: SchoolId, staff: MembershipId, all: bool) -> AppResult<Vec<ClassView>> {
    sqlx::query_as(
        "SELECT cl.id, cl.name, cl.year_group,
                (SELECT count(*) FROM identity.class_members cm JOIN identity.memberships m ON m.id = cm.membership_id
                  WHERE cm.class_id = cl.id AND m.role = 'student' AND m.active) AS pupils
           FROM identity.classes cl
          WHERE cl.school_id = $1
            AND ($3 OR EXISTS (SELECT 1 FROM identity.class_members cm WHERE cm.class_id = cl.id AND cm.membership_id = $2))
          ORDER BY cl.year_group, cl.name",
    )
    .bind(school.0)
    .bind(staff.0)
    .bind(all)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn class_in_school(pool: &PgPool, school: SchoolId, class: Uuid) -> AppResult<ClassView> {
    let all = classes_for(pool, school, MembershipId(Uuid::nil()), true).await?;
    all.into_iter().find(|c| c.id == class).ok_or(AppError::NotFound)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PupilRow {
    pub id: Uuid,
    pub first_name: String,
    pub last_name: String,
    pub username: Option<String>,
    pub year_group: Option<i32>,
}

impl PupilRow {
    pub fn name(&self) -> String {
        let i: String = self.last_name.chars().take(1).collect();
        format!("{} {}", self.first_name, i).trim().to_string()
    }
}

pub async fn class_pupils(pool: &PgPool, class: Uuid) -> AppResult<Vec<PupilRow>> {
    sqlx::query_as(
        "SELECT m.id, a.first_name, a.last_name, m.username, m.year_group
           FROM identity.class_members cm JOIN identity.memberships m ON m.id = cm.membership_id
           JOIN identity.accounts a ON a.id = m.account_id
          WHERE cm.class_id = $1 AND m.role = 'student' AND m.active ORDER BY a.first_name, a.last_name",
    )
    .bind(class)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn courses_for_year(pool: &PgPool, year: i32) -> AppResult<Vec<(Uuid, String, String)>> {
    sqlx::query_as("SELECT id, title, colour FROM curriculum.courses WHERE year_group = $1 ORDER BY title")
        .bind(year)
        .fetch_all(pool)
        .await
        .map_err(db_err)
}

pub async fn course_concepts(pool: &PgPool, course: Uuid) -> AppResult<Vec<(Uuid, String)>> {
    sqlx::query_as("SELECT id, title FROM curriculum.concepts WHERE course_id = $1 ORDER BY position")
        .bind(course)
        .fetch_all(pool)
        .await
        .map_err(db_err)
}

pub struct Heatmap {
    pub concepts: Vec<(Uuid, String)>,
    pub rows: Vec<(PupilRow, Vec<(u8, f64)>)>,
}

pub async fn heatmap(pool: &PgPool, class: Uuid, course: Uuid) -> AppResult<Heatmap> {
    let concepts = course_concepts(pool, course).await?;
    let pupils = class_pupils(pool, class).await?;
    let states: Vec<(Uuid, Uuid, f64, i16)> = sqlx::query_as(
        "SELECT s.membership_id, s.concept_id, s.mastery, s.stage FROM learner.concept_states s
           JOIN curriculum.concepts c ON c.id = s.concept_id
          WHERE c.course_id = $1 AND s.membership_id IN (SELECT membership_id FROM identity.class_members WHERE class_id = $2)",
    )
    .bind(course)
    .bind(class)
    .fetch_all(pool)
    .await
    .map_err(db_err)?;
    let map: HashMap<(Uuid, Uuid), (f64, i16)> = states.into_iter().map(|(m, c, ma, st)| ((m, c), (ma, st))).collect();
    let rows = pupils
        .into_iter()
        .map(|p| {
            let cells = concepts
                .iter()
                .map(|(cid, _)| match map.get(&(p.id, *cid)) {
                    None => (0u8, 0.0),
                    Some((mastery, stage)) => {
                        let mut s = learner::domain::LearnerConceptState::new(MembershipId(p.id), ConceptId(*cid));
                        s.mastery = *mastery;
                        s.stage = *stage as u8;
                        (s.level(), *mastery)
                    }
                })
                .collect();
            (p, cells)
        })
        .collect();
    Ok(Heatmap { concepts, rows })
}

/// Pupils who need a teacher's attention, each with a one-line reason.
pub async fn needs_attention(pool: &PgPool, class: Uuid, today: Date) -> AppResult<Vec<(Uuid, String, String)>> {
    let pupils = class_pupils(pool, class).await?;
    let mut out = Vec::new();
    for p in pupils {
        let last: Option<time::OffsetDateTime> =
            sqlx::query_scalar("SELECT max(started_at) FROM assessment.attempts WHERE membership_id = $1")
                .bind(p.id)
                .fetch_one(pool)
                .await
                .map_err(db_err)?;
        let overdue: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM learner.concept_states WHERE membership_id = $1 AND stage BETWEEN 1 AND 5 AND due_on < $2",
        )
        .bind(p.id)
        .bind(today)
        .fetch_one(pool)
        .await
        .map_err(db_err)?;
        let lapsed: Option<(String, i32)> = sqlx::query_as(
            "SELECT c.title, s.lapses FROM learner.concept_states s JOIN curriculum.concepts c ON c.id = s.concept_id
              WHERE s.membership_id = $1 AND s.lapses >= 2 ORDER BY s.lapses DESC LIMIT 1",
        )
        .bind(p.id)
        .fetch_optional(pool)
        .await
        .map_err(db_err)?;
        let overconfident: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM tutoring.reflections r
              WHERE r.membership_id = $1 AND r.confidence >= 3 AND r.created_at > now() - interval '14 days'
                AND EXISTS (SELECT 1 FROM learner.misconceptions mi WHERE mi.membership_id = r.membership_id
                             AND mi.concept_id = r.concept_id AND NOT mi.fixed)",
        )
        .bind(p.id)
        .fetch_one(pool)
        .await
        .map_err(db_err)?;
        let days_since = last.map(|l| (today - l.date()).whole_days());
        let reason = if days_since.map(|d| d >= 4).unwrap_or(true) && overdue > 0 {
            Some(match last {
                Some(l) => format!("No study since {}. {} review{} overdue.", kernel::long_date(l.date()), overdue, if overdue == 1 { "" } else { "s" }),
                None => "Hasn't started yet.".to_string(),
            })
        } else if let Some((title, n)) = lapsed {
            Some(format!("Failed reviews of {title} {n} times. The plan is re-teaching what comes before it first."))
        } else if overconfident > 0 {
            Some("Very confident but still making the same mistake. Extra worked examples added.".to_string())
        } else {
            None
        };
        if let Some(r) = reason {
            out.push((p.id, p.name(), r));
        }
        if out.len() >= 5 {
            break;
        }
    }
    Ok(out)
}

pub async fn class_mistakes(pool: &PgPool, class: Uuid, since_days: i32) -> AppResult<Vec<(String, String, i64)>> {
    sqlx::query_as(
        "SELECT c.title, r.misconception, count(*) AS n
           FROM assessment.responses r
           JOIN assessment.attempts a ON a.id = r.attempt_id
           JOIN assessment.items i ON i.id = r.item_id
           JOIN curriculum.concepts c ON c.id = i.concept_id
          WHERE r.misconception IS NOT NULL AND r.created_at > now() - make_interval(days => $2)
            AND a.membership_id IN (SELECT membership_id FROM identity.class_members WHERE class_id = $1)
          GROUP BY c.title, r.misconception ORDER BY n DESC LIMIT 5",
    )
    .bind(class)
    .bind(since_days)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn class_studied(pool: &PgPool, class: Uuid, from: Date, to: Date) -> AppResult<(i64, i64)> {
    sqlx::query_as(
        "SELECT count(DISTINCT cm.membership_id) FILTER (WHERE m.role = 'student'),
                count(DISTINCT a.membership_id)
           FROM identity.class_members cm JOIN identity.memberships m ON m.id = cm.membership_id
           LEFT JOIN assessment.attempts a ON a.membership_id = cm.membership_id AND a.started_at::date BETWEEN $2 AND $3
          WHERE cm.class_id = $1",
    )
    .bind(class)
    .bind(from)
    .bind(to)
    .fetch_one(pool)
    .await
    .map_err(db_err)
}

pub async fn pupil_in_school(pool: &PgPool, school: SchoolId, m: Uuid) -> AppResult<PupilRow> {
    sqlx::query_as(
        "SELECT m.id, a.first_name, a.last_name, m.username, m.year_group
           FROM identity.memberships m JOIN identity.accounts a ON a.id = m.account_id
          WHERE m.id = $1 AND m.school_id = $2 AND m.role = 'student'",
    )
    .bind(m)
    .bind(school.0)
    .fetch_one(pool)
    .await
    .map_err(db_err)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StateView {
    pub concept_title: String,
    pub course_title: String,
    pub mastery: f64,
    pub stage: i16,
    pub due_on: Option<Date>,
    pub lapses: i32,
}

pub async fn pupil_states(pool: &PgPool, m: Uuid) -> AppResult<Vec<StateView>> {
    sqlx::query_as(
        "SELECT c.title AS concept_title, co.title AS course_title, s.mastery, s.stage, s.due_on, s.lapses
           FROM learner.concept_states s JOIN curriculum.concepts c ON c.id = s.concept_id
           JOIN curriculum.courses co ON co.id = c.course_id
          WHERE s.membership_id = $1 ORDER BY co.title, c.position",
    )
    .bind(m)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ResponseView {
    pub prompt: String,
    pub answer: String,
    pub correct: bool,
    pub misconception: Option<String>,
    pub created_at: time::OffsetDateTime,
}

pub async fn recent_responses(pool: &PgPool, m: Uuid, limit: i64) -> AppResult<Vec<ResponseView>> {
    sqlx::query_as(
        "SELECT i.prompt, r.answer, r.correct, r.misconception, r.created_at
           FROM assessment.responses r JOIN assessment.attempts a ON a.id = r.attempt_id
           JOIN assessment.items i ON i.id = r.item_id
          WHERE a.membership_id = $1 ORDER BY r.created_at DESC LIMIT $2",
    )
    .bind(m)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn recent_reflections(pool: &PgPool, m: Uuid) -> AppResult<Vec<(String, String, i16, String)>> {
    sqlx::query_as(
        "SELECT c.title, r.body, r.confidence, r.feedback FROM tutoring.reflections r JOIN curriculum.concepts c ON c.id = r.concept_id
          WHERE r.membership_id = $1 ORDER BY r.created_at DESC LIMIT 5",
    )
    .bind(m)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn weekly_review(pool: &PgPool, class: Uuid, week: Date) -> AppResult<Option<String>> {
    sqlx::query_scalar("SELECT body FROM platform.weekly_reviews WHERE class_id = $1 AND week_start = $2")
        .bind(class)
        .bind(week)
        .fetch_optional(pool)
        .await
        .map_err(db_err)
}

pub async fn save_weekly_review(pool: &PgPool, class: Uuid, week: Date, body: &str) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO platform.weekly_reviews (id, class_id, week_start, body) VALUES ($1,$2,$3,$4)
         ON CONFLICT (class_id, week_start) DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(class)
    .bind(week)
    .bind(body)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(())
}

pub async fn all_classes(pool: &PgPool) -> AppResult<Vec<(Uuid, Uuid, String)>> {
    sqlx::query_as("SELECT id, school_id, name FROM identity.classes").fetch_all(pool).await.map_err(db_err)
}

pub async fn active_students(pool: &PgPool) -> AppResult<Vec<Uuid>> {
    sqlx::query_scalar("SELECT id FROM identity.memberships WHERE role = 'student' AND active")
        .fetch_all(pool)
        .await
        .map_err(db_err)
}

// ------------------------------------------------------------------ admin
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StaffView {
    pub membership_id: Uuid,
    pub first_name: String,
    pub last_name: String,
    pub email: Option<String>,
    pub role: String,
    pub duties: Vec<String>,
}

pub async fn staff(pool: &PgPool, school: SchoolId) -> AppResult<Vec<StaffView>> {
    sqlx::query_as(
        "SELECT m.id AS membership_id, a.first_name, a.last_name, a.email, m.role, m.duties
           FROM identity.memberships m JOIN identity.accounts a ON a.id = m.account_id
          WHERE m.school_id = $1 AND m.role IN ('teacher','school_admin') AND m.active ORDER BY a.last_name",
    )
    .bind(school.0)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn school_counts(pool: &PgPool, school: SchoolId) -> AppResult<(i64, i64, i64, i64)> {
    sqlx::query_as(
        "SELECT count(*) FILTER (WHERE role = 'student'), count(*) FILTER (WHERE role IN ('teacher','school_admin')),
                count(*) FILTER (WHERE role = 'parent'),
                (SELECT count(*) FROM safeguarding.cases WHERE school_id = $1 AND status <> 'closed')
           FROM identity.memberships WHERE school_id = $1 AND active",
    )
    .bind(school.0)
    .fetch_one(pool)
    .await
    .map_err(db_err)
}

pub async fn set_duty(pool: &PgPool, school: SchoolId, staff: Uuid, duty: &str, on: bool) -> AppResult<()> {
    if on {
        // Only one lead and one deputy per school.
        sqlx::query("UPDATE identity.memberships SET duties = array_remove(duties, $2) WHERE school_id = $1")
            .bind(school.0)
            .bind(duty)
            .execute(pool)
            .await
            .map_err(db_err)?;
        sqlx::query(
            "UPDATE identity.memberships SET duties = array_append(array_remove(duties, $3), $3)
              WHERE id = $2 AND school_id = $1 AND role IN ('teacher','school_admin')",
        )
        .bind(school.0)
        .bind(staff)
        .bind(duty)
        .execute(pool)
        .await
        .map_err(db_err)?;
    } else {
        sqlx::query("UPDATE identity.memberships SET duties = array_remove(duties, $3) WHERE id = $2 AND school_id = $1")
            .bind(school.0)
            .bind(staff)
            .bind(duty)
            .execute(pool)
            .await
            .map_err(db_err)?;
    }
    Ok(())
}

pub async fn school_duties(pool: &PgPool, school: SchoolId) -> AppResult<Vec<kernel::Duty>> {
    let d: Vec<String> = sqlx::query_scalar("SELECT unnest(duties) FROM identity.memberships WHERE school_id = $1 AND active")
        .bind(school.0)
        .fetch_all(pool)
        .await
        .map_err(db_err)?;
    Ok(d.iter()
        .filter_map(|s| match s.as_str() {
            "dsl" => Some(kernel::Duty::SafeguardingLead),
            "deputy_dsl" => Some(kernel::Duty::DeputySafeguardingLead),
            _ => None,
        })
        .collect())
}

pub async fn set_school_active(pool: &PgPool, school: SchoolId, active: bool) -> AppResult<()> {
    sqlx::query("UPDATE identity.schools SET active = $2 WHERE id = $1")
        .bind(school.0)
        .bind(active)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(())
}

pub async fn usernames(pool: &PgPool, school: SchoolId) -> AppResult<HashSet<String>> {
    let v: Vec<String> = sqlx::query_scalar("SELECT username FROM identity.memberships WHERE school_id = $1 AND username IS NOT NULL")
        .bind(school.0)
        .fetch_all(pool)
        .await
        .map_err(db_err)?;
    Ok(v.into_iter().collect())
}

/// Create pupils from checked import rows. Returns (name, class, username, password).
pub async fn create_pupils(
    pool: &PgPool,
    school: SchoolId,
    rows: &[identity::domain::ImportRow],
) -> AppResult<Vec<(String, String, String, String)>> {
    let mut tx = pool.begin().await.map_err(db_err)?;
    let mut out = Vec::new();
    for r in rows {
        let Some(year) = r.year_group else { return Err(AppError::Invalid(format!("Line {} has no year group", r.line))) };
        let class_id: Uuid = match sqlx::query_scalar::<_, Uuid>("SELECT id FROM identity.classes WHERE school_id = $1 AND name = $2")
            .bind(school.0)
            .bind(&r.class_name)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_err)?
        {
            Some(id) => id,
            None => {
                let id = Uuid::now_v7();
                sqlx::query("INSERT INTO identity.classes (id, school_id, name, year_group) VALUES ($1,$2,$3,$4)")
                    .bind(id)
                    .bind(school.0)
                    .bind(&r.class_name)
                    .bind(year)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
                id
            }
        };
        let pw = identity::domain::passphrase();
        let aid = Uuid::now_v7();
        let mid = Uuid::now_v7();
        sqlx::query("INSERT INTO identity.accounts (id, first_name, last_name, password_hash) VALUES ($1,$2,$3,$4)")
            .bind(aid)
            .bind(&r.first_name)
            .bind(&r.last_name)
            .bind(crate::auth::hash_password(&pw))
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query("INSERT INTO identity.memberships (id, account_id, school_id, role, username, year_group) VALUES ($1,$2,$3,'student',$4,$5)")
            .bind(mid)
            .bind(aid)
            .bind(school.0)
            .bind(&r.username)
            .bind(year)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query("INSERT INTO identity.class_members (class_id, membership_id) VALUES ($1,$2)")
            .bind(class_id)
            .bind(mid)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        // Core courses for the year group are attached automatically.
        let courses: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM curriculum.courses WHERE year_group = $1")
            .bind(year)
            .fetch_all(&mut *tx)
            .await
            .map_err(db_err)?;
        for c in courses {
            sqlx::query("INSERT INTO planning.enrolments (membership_id, course_id) VALUES ($1,$2) ON CONFLICT DO NOTHING")
                .bind(mid)
                .bind(c)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
            crate::outbox::write_tx(&mut tx, &kernel::DomainEvent::PupilEnrolled { membership_id: MembershipId(mid), course_id: CourseId(c) }).await?;
        }
        out.push((format!("{} {}", r.first_name, r.last_name), r.class_name.clone(), r.username.clone(), pw));
    }
    tx.commit().await.map_err(db_err)?;
    Ok(out)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct InviteView {
    pub pupil: String,
    pub class_name: String,
    pub invite_code: String,
    pub linked: bool,
}

/// Ensure every pupil in the school has a parent invite code; list them.
pub async fn parent_invites(pool: &PgPool, school: SchoolId) -> AppResult<Vec<InviteView>> {
    let missing: Vec<Uuid> = sqlx::query_scalar(
        "SELECT m.id FROM identity.memberships m WHERE m.school_id = $1 AND m.role = 'student' AND m.active
            AND NOT EXISTS (SELECT 1 FROM identity.parent_links p WHERE p.student_membership_id = m.id)",
    )
    .bind(school.0)
    .fetch_all(pool)
    .await
    .map_err(db_err)?;
    for m in missing {
        sqlx::query("INSERT INTO identity.parent_links (id, school_id, student_membership_id, invite_code) VALUES ($1,$2,$3,$4) ON CONFLICT DO NOTHING")
            .bind(Uuid::now_v7())
            .bind(school.0)
            .bind(m)
            .bind(identity::domain::invite_code())
            .execute(pool)
            .await
            .map_err(db_err)?;
    }
    sqlx::query_as(
        "SELECT a.first_name || ' ' || a.last_name AS pupil, COALESCE(cl.name, '') AS class_name, p.invite_code,
                (p.parent_membership_id IS NOT NULL) AS linked
           FROM identity.parent_links p JOIN identity.memberships m ON m.id = p.student_membership_id
           JOIN identity.accounts a ON a.id = m.account_id
           LEFT JOIN identity.class_members cm ON cm.membership_id = m.id
           LEFT JOIN identity.classes cl ON cl.id = cm.class_id
          WHERE p.school_id = $1 ORDER BY cl.name, a.first_name",
    )
    .bind(school.0)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

// ------------------------------------------------------------------ parent
pub async fn children(pool: &PgPool, parent: MembershipId) -> AppResult<Vec<PupilRow>> {
    sqlx::query_as(
        "SELECT m.id, a.first_name, a.last_name, m.username, m.year_group
           FROM identity.parent_links p JOIN identity.memberships m ON m.id = p.student_membership_id
           JOIN identity.accounts a ON a.id = m.account_id
          WHERE p.parent_membership_id = $1 AND p.verified_at IS NOT NULL ORDER BY a.first_name",
    )
    .bind(parent.0)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn link_child(pool: &PgPool, parent: MembershipId, code: &str) -> AppResult<bool> {
    let n = sqlx::query(
        "UPDATE identity.parent_links SET parent_membership_id = $1, verified_at = now()
          WHERE upper(invite_code) = upper($2) AND parent_membership_id IS NULL",
    )
    .bind(parent.0)
    .bind(code.trim())
    .execute(pool)
    .await
    .map_err(db_err)?
    .rows_affected();
    Ok(n > 0)
}

pub async fn plan_paused(pool: &PgPool, m: Uuid) -> AppResult<bool> {
    sqlx::query_scalar("SELECT COALESCE((SELECT paused FROM planning.preferences WHERE membership_id = $1), false)")
        .bind(m)
        .fetch_one(pool)
        .await
        .map_err(db_err)
}

pub async fn pupil_week(pool: &PgPool, m: Uuid, from: Date) -> AppResult<PupilWeek> {
    let to = from + Duration::days(6);
    let mut w = PupilWeek::default();
    let mins: Vec<(Date, i64)> = sqlx::query_as(
        "SELECT on_date, sum(minutes)::bigint FROM planning.tasks
          WHERE membership_id = $1 AND done_at IS NOT NULL AND on_date BETWEEN $2 AND $3 GROUP BY on_date",
    )
    .bind(m)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .map_err(db_err)?;
    let by_day: HashMap<Date, i64> = mins.into_iter().collect();
    for i in 0..7 {
        let d = from + Duration::days(i);
        let v = *by_day.get(&d).unwrap_or(&0) as i32;
        if i < 5 || v > 0 {
            w.minutes_by_day.push((kernel::weekday_short(d).to_string(), v));
        }
        if v > 0 {
            w.days_studied += 1;
            w.total_minutes += v;
        }
    }
    w.secured = sqlx::query_scalar(
        "SELECT c.title FROM learner.concept_states s JOIN curriculum.concepts c ON c.id = s.concept_id
          WHERE s.membership_id = $1 AND s.secured_on BETWEEN $2 AND $3",
    )
    .bind(m)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .map_err(db_err)?;
    w.learned = sqlx::query_scalar(
        "SELECT c.title FROM learner.concept_states s JOIN curriculum.concepts c ON c.id = s.concept_id
          WHERE s.membership_id = $1 AND s.learned_on BETWEEN $2 AND $3",
    )
    .bind(m)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .map_err(db_err)?;
    let (qr, qt): (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE r.correct), count(*) FROM assessment.responses r JOIN assessment.attempts a ON a.id = r.attempt_id
          WHERE a.membership_id = $1 AND a.kind = 'lesson' AND r.created_at::date BETWEEN $2 AND $3",
    )
    .bind(m)
    .bind(from)
    .bind(to)
    .fetch_one(pool)
    .await
    .map_err(db_err)?;
    w.quiz_right = qr.unwrap_or(0);
    w.quiz_total = qt.unwrap_or(0);
    let (rp, rt): (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE score * 3 >= max_score * 2), count(*) FROM assessment.attempts
          WHERE membership_id = $1 AND kind = 'review' AND finished_at IS NOT NULL AND started_at::date BETWEEN $2 AND $3",
    )
    .bind(m)
    .bind(from)
    .bind(to)
    .fetch_one(pool)
    .await
    .map_err(db_err)?;
    w.reviews_passed = rp.unwrap_or(0);
    w.reviews_total = rt.unwrap_or(0);
    w.mock = sqlx::query_as(
        "SELECT mo.title, a.score, a.max_score FROM assessment.attempts a JOIN assessment.mocks mo ON mo.id = a.mock_id
          WHERE a.membership_id = $1 AND a.kind = 'mock' AND a.finished_at IS NOT NULL AND a.started_at::date BETWEEN $2 AND $3
          ORDER BY a.finished_at DESC LIMIT 1",
    )
    .bind(m)
    .bind(from)
    .bind(to)
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;
    w.mistakes = sqlx::query_as(
        "SELECT c.title, mi.misconception, mi.seen::bigint FROM learner.misconceptions mi JOIN curriculum.concepts c ON c.id = mi.concept_id
          WHERE mi.membership_id = $1 AND NOT mi.fixed ORDER BY mi.last_seen_at DESC LIMIT 3",
    )
    .bind(m)
    .fetch_all(pool)
    .await
    .map_err(db_err)?;
    Ok(w)
}

/// Help ideas for parents: from the concepts with open mistakes first, then
/// the concepts learned most recently. Always three if available.
pub async fn help_ideas(pool: &PgPool, m: Uuid) -> AppResult<Vec<(String, String, String)>> {
    sqlx::query_as(
        "WITH ranked AS (
            SELECT h.title, h.body, c.title AS concept, 0 AS pri, mi.last_seen_at AS t
              FROM learner.misconceptions mi JOIN content.help_ideas h ON h.concept_id = mi.concept_id
              JOIN curriculum.concepts c ON c.id = mi.concept_id
             WHERE mi.membership_id = $1 AND NOT mi.fixed
            UNION ALL
            SELECT h.title, h.body, c.title, 1, s.updated_at
              FROM learner.concept_states s JOIN content.help_ideas h ON h.concept_id = s.concept_id
              JOIN curriculum.concepts c ON c.id = s.concept_id
             WHERE s.membership_id = $1 AND s.stage >= 1)
         SELECT title, body, concept FROM (
            SELECT DISTINCT ON (title) title, body, concept, pri, t FROM ranked ORDER BY title, pri, t DESC) x
          ORDER BY pri, t DESC LIMIT 3",
    )
    .bind(m)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn teacher_contact(pool: &PgPool, pupil: Uuid) -> AppResult<Option<(String, Option<String>)>> {
    sqlx::query_as(
        "SELECT a.first_name || ' ' || a.last_name, a.email FROM identity.class_members cm
           JOIN identity.class_members tm ON tm.class_id = cm.class_id
           JOIN identity.memberships m ON m.id = tm.membership_id AND m.role = 'teacher'
           JOIN identity.accounts a ON a.id = m.account_id
          WHERE cm.membership_id = $1 LIMIT 1",
    )
    .bind(pupil)
    .fetch_optional(pool)
    .await
    .map_err(db_err)
}

// ------------------------------------------------------------------ system admin
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SchoolSummary {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub active: bool,
    pub pupils: i64,
    pub staff: i64,
    pub open_cases: i64,
}

pub async fn schools(pool: &PgPool) -> AppResult<Vec<SchoolSummary>> {
    sqlx::query_as(
        "SELECT s.id, s.code, s.name, s.active,
                (SELECT count(*) FROM identity.memberships m WHERE m.school_id = s.id AND m.role = 'student') AS pupils,
                (SELECT count(*) FROM identity.memberships m WHERE m.school_id = s.id AND m.role IN ('teacher','school_admin')) AS staff,
                (SELECT count(*) FROM safeguarding.cases c WHERE c.school_id = s.id AND c.status <> 'closed') AS open_cases
           FROM identity.schools s ORDER BY s.name",
    )
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn create_school(pool: &PgPool, code: &str, name: &str, admin_first: &str, admin_last: &str, admin_email: &str) -> AppResult<String> {
    let code = identity::domain::normalise_school_code(code).ok_or_else(|| AppError::Invalid("School code must be 3 to 12 letters or numbers.".into()))?;
    if name.trim().is_empty() || admin_email.trim().is_empty() || !admin_email.contains('@') {
        return Err(AppError::Invalid("Enter the school name and the school admin's email address.".into()));
    }
    let pw = identity::domain::passphrase();
    let mut tx = pool.begin().await.map_err(db_err)?;
    let sid = Uuid::now_v7();
    sqlx::query("INSERT INTO identity.schools (id, code, name) VALUES ($1,$2,$3)")
        .bind(sid)
        .bind(&code)
        .bind(name.trim())
        .execute(&mut *tx)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_unique_violation() => AppError::Invalid("That school code is already used.".into()),
            other => db_err(other),
        })?;
    let existing: Option<Uuid> = sqlx::query_scalar("SELECT id FROM identity.accounts WHERE lower(email) = lower($1)")
        .bind(admin_email.trim())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
    let aid = match existing {
        Some(a) => a,
        None => {
            let a = Uuid::now_v7();
            sqlx::query("INSERT INTO identity.accounts (id, first_name, last_name, email, password_hash) VALUES ($1,$2,$3,lower($4),$5)")
                .bind(a)
                .bind(admin_first.trim())
                .bind(admin_last.trim())
                .bind(admin_email.trim())
                .bind(crate::auth::hash_password(&pw))
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
            a
        }
    };
    sqlx::query("INSERT INTO identity.memberships (id, account_id, school_id, role) VALUES ($1,$2,$3,'school_admin')")
        .bind(Uuid::now_v7())
        .bind(aid)
        .bind(sid)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
    tx.commit().await.map_err(db_err)?;
    Ok(pw)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ContentSummary {
    pub course: String,
    pub year_group: i32,
    pub concepts: i64,
    pub lessons_live: i64,
    pub items: i64,
    pub open_reports: i64,
}

pub async fn content_summary(pool: &PgPool) -> AppResult<Vec<ContentSummary>> {
    sqlx::query_as(
        "SELECT co.title AS course, co.year_group,
                (SELECT count(*) FROM curriculum.concepts c WHERE c.course_id = co.id) AS concepts,
                (SELECT count(*) FROM content.lessons l JOIN curriculum.concepts c ON c.id = l.concept_id WHERE c.course_id = co.id AND l.status = 'live') AS lessons_live,
                (SELECT count(*) FROM assessment.items i JOIN curriculum.concepts c ON c.id = i.concept_id WHERE c.course_id = co.id) AS items,
                (SELECT count(*) FROM content.reports r JOIN assessment.items i ON i.id = r.item_id JOIN curriculum.concepts c ON c.id = i.concept_id
                  WHERE c.course_id = co.id AND r.status = 'open') AS open_reports
           FROM curriculum.courses co ORDER BY co.year_group, co.title",
    )
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn open_reports(pool: &PgPool) -> AppResult<Vec<(Uuid, String, String, time::OffsetDateTime)>> {
    sqlx::query_as(
        "SELECT r.id, COALESCE(i.prompt, ''), r.note, r.created_at FROM content.reports r
           LEFT JOIN assessment.items i ON i.id = r.item_id WHERE r.status = 'open' ORDER BY r.created_at DESC LIMIT 50",
    )
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn report_item(pool: &PgPool, m: MembershipId, item: Uuid, note: &str) -> AppResult<()> {
    sqlx::query("INSERT INTO content.reports (id, item_id, membership_id, note) VALUES ($1,$2,$3,$4)")
        .bind(Uuid::now_v7())
        .bind(item)
        .bind(m.0)
        .bind(note.chars().take(500).collect::<String>())
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(())
}

pub async fn resolve_report(pool: &PgPool, id: Uuid, retire_item: bool) -> AppResult<()> {
    if retire_item {
        sqlx::query("UPDATE assessment.items SET status = 'retired' WHERE id = (SELECT item_id FROM content.reports WHERE id = $1)")
            .bind(id)
            .execute(pool)
            .await
            .map_err(db_err)?;
    }
    sqlx::query("UPDATE content.reports SET status = 'resolved' WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(())
}

pub async fn ai_usage_summary(pool: &PgPool) -> AppResult<(i64, i64, i64)> {
    sqlx::query_as(
        "SELECT count(*), COALESCE(sum(input_tokens),0)::bigint, COALESCE(sum(output_tokens),0)::bigint
           FROM platform.ai_usage WHERE at > now() - interval '30 days'",
    )
    .fetch_one(pool)
    .await
    .map_err(db_err)
}

pub async fn membership_name(pool: &PgPool, m: Uuid) -> AppResult<String> {
    let (f, l): (String, String) = sqlx::query_as(
        "SELECT a.first_name, a.last_name FROM identity.memberships m JOIN identity.accounts a ON a.id = m.account_id WHERE m.id = $1",
    )
    .bind(m)
    .fetch_one(pool)
    .await
    .map_err(db_err)?;
    Ok(format!("{f} {}", l.chars().take(1).collect::<String>()).trim().to_string())
}

pub async fn school_of(pool: &PgPool, m: Uuid) -> AppResult<Option<SchoolId>> {
    let s: Option<Option<Uuid>> = sqlx::query_scalar("SELECT school_id FROM identity.memberships WHERE id = $1")
        .bind(m)
        .fetch_optional(pool)
        .await
        .map_err(db_err)?;
    Ok(s.flatten().map(SchoolId))
}

pub async fn helper_messages(pool: &PgPool, m: MembershipId, concept: ConceptId) -> AppResult<Vec<(String, String)>> {
    sqlx::query_as(
        "SELECT author, body FROM tutoring.messages WHERE membership_id = $1 AND concept_id = $2
          AND created_at > now() - interval '1 day' ORDER BY created_at",
    )
    .bind(m.0)
    .bind(concept.0)
    .fetch_all(pool)
    .await
    .map_err(db_err)
}

pub async fn save_message(pool: &PgPool, m: MembershipId, concept: Option<ConceptId>, author: &str, body: &str, flagged: bool) -> AppResult<()> {
    sqlx::query("INSERT INTO tutoring.messages (id, membership_id, concept_id, author, body, flagged) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(Uuid::now_v7())
        .bind(m.0)
        .bind(concept.map(|c| c.0))
        .bind(author)
        .bind(body)
        .bind(flagged)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(())
}

pub async fn save_reflection(pool: &PgPool, m: MembershipId, concept: ConceptId, body: &str, confidence: i16, feedback: &str) -> AppResult<()> {
    sqlx::query("INSERT INTO tutoring.reflections (id, membership_id, concept_id, body, confidence, feedback) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(Uuid::now_v7())
        .bind(m.0)
        .bind(concept.0)
        .bind(body)
        .bind(confidence)
        .bind(feedback)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(())
}

pub async fn mark_misconceptions_fixed(pool: &PgPool, m: MembershipId, concept: ConceptId) -> AppResult<()> {
    sqlx::query("UPDATE learner.misconceptions SET fixed = true WHERE membership_id = $1 AND concept_id = $2")
        .bind(m.0)
        .bind(concept.0)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(())
}

pub async fn add_task(pool: &PgPool, m: Uuid, concept: Uuid, kind: &str, minutes: i32, on: Date) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO planning.tasks (id, membership_id, on_date, kind, concept_id, minutes, position)
         SELECT $1,$2,$3,$4,$5,$6, COALESCE(max(position),0) + 1 FROM planning.tasks WHERE membership_id = $2 AND on_date = $3",
    )
    .bind(Uuid::now_v7())
    .bind(m)
    .bind(on)
    .bind(kind)
    .bind(concept)
    .bind(minutes)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(())
}
