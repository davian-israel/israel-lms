//! Repository adapters: map aggregates to tables by hand with SQLx.

use crate::db_err;
use assessment::application::{Attempts, ItemBank, MockPaper};
use assessment::domain::{Item, ItemKind, ItemOption};
use async_trait::async_trait;
use kernel::{AppError, AppResult, AttemptId, CaseId, ConceptId, ItemId, MembershipId, MockId, SchoolId};
use learner::application::LearnerStates;
use learner::domain::LearnerConceptState;
use planning::application::Plans;
use planning::domain::{PlannedTask, PlanningInput, StudyPreferences};
use safeguarding::application::Cases;
use safeguarding::domain::{CaseStatus, SafeguardingCase, Severity};
use sqlx::PgPool;
use time::Date;
use uuid::Uuid;

#[derive(Clone)]
pub struct Repos {
    pub pool: PgPool,
}

impl Repos {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

// ---------------------------------------------------------------- learner
#[derive(sqlx::FromRow)]
struct StateRow {
    mastery: f64,
    stage: i16,
    learned_on: Option<Date>,
    due_on: Option<Date>,
    secured_on: Option<Date>,
    lapses: i32,
    version: i32,
}

#[async_trait]
impl LearnerStates for Repos {
    async fn get_or_new(&self, m: MembershipId, c: ConceptId) -> AppResult<LearnerConceptState> {
        let r: Option<StateRow> = sqlx::query_as(
            "SELECT mastery, stage, learned_on, due_on, secured_on, lapses, version
               FROM learner.concept_states WHERE membership_id = $1 AND concept_id = $2",
        )
        .bind(m.0)
        .bind(c.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(match r {
            None => LearnerConceptState::new(m, c),
            Some(r) => LearnerConceptState {
                membership_id: m,
                concept_id: c,
                mastery: r.mastery,
                stage: r.stage as u8,
                learned_on: r.learned_on,
                due_on: r.due_on,
                secured_on: r.secured_on,
                lapses: r.lapses as u32,
                version: r.version,
            },
        })
    }

    /// Optimistic concurrency: the update only applies if nobody saved in between.
    async fn save(&self, s: &LearnerConceptState) -> AppResult<()> {
        let n = sqlx::query(
            "INSERT INTO learner.concept_states (membership_id, concept_id, mastery, stage, learned_on, due_on, secured_on, lapses, version)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,1)
             ON CONFLICT (membership_id, concept_id) DO UPDATE SET
               mastery = EXCLUDED.mastery, stage = EXCLUDED.stage, learned_on = EXCLUDED.learned_on,
               due_on = EXCLUDED.due_on, secured_on = EXCLUDED.secured_on, lapses = EXCLUDED.lapses,
               version = learner.concept_states.version + 1, updated_at = now()
             WHERE learner.concept_states.version = $9",
        )
        .bind(s.membership_id.0)
        .bind(s.concept_id.0)
        .bind(s.mastery)
        .bind(s.stage as i16)
        .bind(s.learned_on)
        .bind(s.due_on)
        .bind(s.secured_on)
        .bind(s.lapses as i32)
        .bind(s.version)
        .execute(&self.pool)
        .await
        .map_err(db_err)?
        .rows_affected();
        if n == 0 {
            return Err(AppError::Conflict("learner state changed concurrently".into()));
        }
        Ok(())
    }

    async fn add_misconception(&self, m: MembershipId, c: ConceptId, code: &str) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO learner.misconceptions (membership_id, concept_id, misconception) VALUES ($1,$2,$3)
             ON CONFLICT (membership_id, concept_id, misconception)
             DO UPDATE SET seen = learner.misconceptions.seen + 1, fixed = false, last_seen_at = now()",
        )
        .bind(m.0)
        .bind(c.0)
        .bind(code)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }
}

// ---------------------------------------------------------------- planning
#[async_trait]
impl Plans for Repos {
    async fn preferences(&self, m: MembershipId) -> AppResult<StudyPreferences> {
        let r: Option<(Vec<i16>, i32)> = sqlx::query_as("SELECT days, minutes FROM planning.preferences WHERE membership_id = $1")
            .bind(m.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(r.map(|(d, minutes)| StudyPreferences { days: d.into_iter().map(|x| x as u8).collect(), minutes })
            .unwrap_or_default())
    }

    async fn save_preferences(&self, m: MembershipId, p: &StudyPreferences) -> AppResult<()> {
        p.validate().map_err(|e| AppError::Invalid(e.to_string()))?;
        let days: Vec<i16> = p.days.iter().map(|d| *d as i16).collect();
        sqlx::query(
            "INSERT INTO planning.preferences (membership_id, days, minutes) VALUES ($1,$2,$3)
             ON CONFLICT (membership_id) DO UPDATE SET days = EXCLUDED.days, minutes = EXCLUDED.minutes",
        )
        .bind(m.0)
        .bind(days)
        .bind(p.minutes)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn planning_input(&self, m: MembershipId, today: Date) -> AppResult<PlanningInput> {
        let paused: bool = sqlx::query_scalar("SELECT COALESCE((SELECT paused FROM planning.preferences WHERE membership_id = $1), false)")
            .bind(m.0)
            .fetch_one(&self.pool)
            .await
            .map_err(db_err)?;
        let reviews: Vec<(Uuid, Date)> = sqlx::query_as(
            "SELECT concept_id, due_on FROM learner.concept_states
              WHERE membership_id = $1 AND stage BETWEEN 1 AND 6 AND due_on IS NOT NULL AND due_on <= $2 + 6",
        )
        .bind(m.0)
        .bind(today)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        let fixes: Vec<Uuid> = sqlx::query_scalar(
            "SELECT concept_id FROM learner.misconceptions WHERE membership_id = $1 AND NOT fixed
              GROUP BY concept_id ORDER BY max(last_seen_at) LIMIT 4",
        )
        .bind(m.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        // New concepts: enrolled courses, not started, prerequisites already started, course order.
        let new_concepts: Vec<Uuid> = sqlx::query_scalar(
            "SELECT c.id FROM planning.enrolments e
               JOIN curriculum.concepts c ON c.course_id = e.course_id
               JOIN curriculum.courses co ON co.id = c.course_id
              WHERE e.membership_id = $1
                AND NOT EXISTS (SELECT 1 FROM learner.concept_states s WHERE s.membership_id = $1 AND s.concept_id = c.id AND s.stage > 0)
                AND NOT EXISTS (SELECT 1 FROM curriculum.concept_edges ed
                                 WHERE ed.concept_id = c.id
                                   AND NOT EXISTS (SELECT 1 FROM learner.concept_states s2
                                                    WHERE s2.membership_id = $1 AND s2.concept_id = ed.prerequisite_id AND s2.stage > 0))
              ORDER BY c.position, co.code
              LIMIT 12",
        )
        .bind(m.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        // Interleave subjects: round-robin by course so a week mixes topics.
        Ok(PlanningInput {
            reviews: reviews.into_iter().map(|(c, d)| (ConceptId(c), d)).collect(),
            fixes: fixes.into_iter().map(ConceptId).collect(),
            new_concepts: new_concepts.into_iter().map(ConceptId).collect(),
            paused,
        })
    }

    async fn replace_week(&self, m: MembershipId, from: Date, tasks: &[PlannedTask]) -> AppResult<()> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        sqlx::query("DELETE FROM planning.tasks WHERE membership_id = $1 AND on_date >= $2 AND done_at IS NULL")
            .bind(m.0)
            .bind(from)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        for t in tasks {
            // Don't re-add a task already done today for the same concept and kind.
            let done: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM planning.tasks WHERE membership_id = $1 AND on_date = $2 AND concept_id = $3 AND kind = $4 AND done_at IS NOT NULL",
            )
            .bind(m.0)
            .bind(t.on)
            .bind(t.concept_id.0)
            .bind(t.kind.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_err)?;
            if done.is_some() {
                continue;
            }
            sqlx::query(
                "INSERT INTO planning.tasks (id, membership_id, on_date, kind, concept_id, minutes, position) VALUES ($1,$2,$3,$4,$5,$6,$7)",
            )
            .bind(Uuid::now_v7())
            .bind(m.0)
            .bind(t.on)
            .bind(t.kind.as_str())
            .bind(t.concept_id.0)
            .bind(t.minutes)
            .bind(t.order)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }
        tx.commit().await.map_err(db_err)
    }

    async fn set_paused(&self, m: MembershipId, paused: bool, reason: &str) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO planning.preferences (membership_id, paused, pause_reason) VALUES ($1,$2,$3)
             ON CONFLICT (membership_id) DO UPDATE SET paused = EXCLUDED.paused, pause_reason = EXCLUDED.pause_reason",
        )
        .bind(m.0)
        .bind(paused)
        .bind(reason)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        if paused {
            sqlx::query("DELETE FROM planning.tasks WHERE membership_id = $1 AND done_at IS NULL")
                .bind(m.0)
                .execute(&self.pool)
                .await
                .map_err(db_err)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- assessment
#[derive(sqlx::FromRow)]
pub struct ItemRow {
    pub id: Uuid,
    pub concept_id: Uuid,
    pub kind: String,
    pub prompt: String,
    pub options: serde_json::Value,
    pub answer: String,
    pub explanation: String,
    pub hints: serde_json::Value,
    pub marks: i32,
}

impl From<ItemRow> for Item {
    fn from(r: ItemRow) -> Self {
        Item {
            id: ItemId(r.id),
            concept_id: ConceptId(r.concept_id),
            kind: ItemKind::parse(&r.kind),
            prompt: r.prompt,
            options: serde_json::from_value::<Vec<ItemOption>>(r.options).unwrap_or_default(),
            answer: r.answer,
            explanation: r.explanation,
            hints: serde_json::from_value(r.hints).unwrap_or_default(),
            marks: r.marks,
        }
    }
}

pub const ITEM_COLS: &str = "id, concept_id, kind, prompt, options, answer, explanation, hints, marks";

impl Repos {
    pub async fn items(&self, c: ConceptId, usage: &str) -> AppResult<Vec<Item>> {
        let rows: Vec<ItemRow> = sqlx::query_as(&format!(
            "SELECT {ITEM_COLS} FROM assessment.items WHERE concept_id = $1 AND usage = $2 AND status = 'live' ORDER BY position"
        ))
        .bind(c.0)
        .bind(usage)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }
}

#[async_trait]
impl ItemBank for Repos {
    async fn items_for_concept(&self, c: ConceptId) -> AppResult<Vec<Item>> {
        self.items(c, "quiz").await
    }
    async fn item(&self, id: ItemId) -> AppResult<Item> {
        let r: ItemRow = sqlx::query_as(&format!("SELECT {ITEM_COLS} FROM assessment.items WHERE id = $1"))
            .bind(id.0)
            .fetch_one(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(r.into())
    }
    async fn mock(&self, id: MockId) -> AppResult<MockPaper> {
        let (title, minutes, ids): (String, i32, Vec<Uuid>) =
            sqlx::query_as("SELECT title, minutes, item_ids FROM assessment.mocks WHERE id = $1")
                .bind(id.0)
                .fetch_one(&self.pool)
                .await
                .map_err(db_err)?;
        let rows: Vec<ItemRow> = sqlx::query_as(&format!(
            "SELECT {ITEM_COLS} FROM assessment.items WHERE id = ANY($1) ORDER BY array_position($1, id)"
        ))
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(MockPaper { id, title, minutes, items: rows.into_iter().map(Into::into).collect() })
    }
}

#[async_trait]
impl Attempts for Repos {
    async fn start(&self, m: MembershipId, kind: &str, concept: Option<ConceptId>, mock: Option<MockId>) -> AppResult<AttemptId> {
        let id = AttemptId::new();
        sqlx::query("INSERT INTO assessment.attempts (id, membership_id, kind, concept_id, mock_id) VALUES ($1,$2,$3,$4,$5)")
            .bind(id.0)
            .bind(m.0)
            .bind(kind)
            .bind(concept.map(|c| c.0))
            .bind(mock.map(|x| x.0))
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(id)
    }
    async fn record_response(&self, a: AttemptId, item: ItemId, answer: &str, correct: bool, misconception: Option<&str>) -> AppResult<()> {
        // An attempt keeps the first answer to each item (invariant: one response per item).
        sqlx::query(
            "INSERT INTO assessment.responses (id, attempt_id, item_id, answer, correct, misconception) VALUES ($1,$2,$3,$4,$5,$6)
             ON CONFLICT (attempt_id, item_id) DO NOTHING",
        )
        .bind(Uuid::now_v7())
        .bind(a.0)
        .bind(item.0)
        .bind(answer)
        .bind(correct)
        .bind(misconception)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }
    async fn finish(&self, a: AttemptId, score: i32, max: i32) -> AppResult<()> {
        sqlx::query("UPDATE assessment.attempts SET finished_at = now(), score = $2, max_score = $3 WHERE id = $1 AND finished_at IS NULL")
            .bind(a.0)
            .bind(score)
            .bind(max)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }
}

// ---------------------------------------------------------------- safeguarding
#[derive(sqlx::FromRow)]
struct CaseRow {
    id: Uuid,
    school_id: Uuid,
    membership_id: Uuid,
    severity: String,
    category: String,
    excerpt: String,
    status: String,
    opened_at: time::OffsetDateTime,
    acknowledged_at: Option<time::OffsetDateTime>,
    action: Option<String>,
    notes: Option<String>,
    closed_at: Option<time::OffsetDateTime>,
}

impl From<CaseRow> for SafeguardingCase {
    fn from(r: CaseRow) -> Self {
        SafeguardingCase {
            id: CaseId(r.id),
            school_id: SchoolId(r.school_id),
            membership_id: MembershipId(r.membership_id),
            severity: Severity::parse(&r.severity),
            category: r.category,
            excerpt: r.excerpt,
            status: CaseStatus::parse(&r.status),
            opened_at: r.opened_at,
            acknowledged_at: r.acknowledged_at,
            action: r.action,
            notes: r.notes,
            closed_at: r.closed_at,
        }
    }
}

const CASE_COLS: &str = "id, school_id, membership_id, severity, category, excerpt, status, opened_at, acknowledged_at, action, notes, closed_at";

#[async_trait]
impl Cases for Repos {
    async fn save(&self, c: &SafeguardingCase) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO safeguarding.cases (id, school_id, membership_id, severity, category, excerpt, status, opened_at, acknowledged_at, action, notes, closed_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
             ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status, acknowledged_at = EXCLUDED.acknowledged_at,
               action = EXCLUDED.action, notes = EXCLUDED.notes, closed_at = EXCLUDED.closed_at",
        )
        .bind(c.id.0)
        .bind(c.school_id.0)
        .bind(c.membership_id.0)
        .bind(c.severity.as_str())
        .bind(&c.category)
        .bind(&c.excerpt)
        .bind(c.status.as_str())
        .bind(c.opened_at)
        .bind(c.acknowledged_at)
        .bind(&c.action)
        .bind(&c.notes)
        .bind(c.closed_at)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }
    async fn get(&self, school: SchoolId, id: CaseId) -> AppResult<SafeguardingCase> {
        let r: CaseRow = sqlx::query_as(&format!("SELECT {CASE_COLS} FROM safeguarding.cases WHERE id = $1 AND school_id = $2"))
            .bind(id.0)
            .bind(school.0)
            .fetch_one(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(r.into())
    }
    async fn list(&self, school: SchoolId) -> AppResult<Vec<SafeguardingCase>> {
        let rows: Vec<CaseRow> = sqlx::query_as(&format!(
            "SELECT {CASE_COLS} FROM safeguarding.cases WHERE school_id = $1 ORDER BY (status = 'closed'), opened_at DESC LIMIT 100"
        ))
        .bind(school.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }
}

impl Repos {
    /// All open cases across schools, for escalation by the worker.
    pub async fn open_cases(&self) -> AppResult<Vec<(SafeguardingCase, i16)>> {
        let rows: Vec<(Uuid, i16)> = sqlx::query_as("SELECT id, escalation_sent FROM safeguarding.cases WHERE status = 'open'")
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;
        let mut out = Vec::new();
        for (id, sent) in rows {
            let r: CaseRow = sqlx::query_as(&format!("SELECT {CASE_COLS} FROM safeguarding.cases WHERE id = $1"))
                .bind(id)
                .fetch_one(&self.pool)
                .await
                .map_err(db_err)?;
            out.push((r.into(), sent));
        }
        Ok(out)
    }

    pub async fn mark_escalated(&self, id: CaseId, level: i16) -> AppResult<()> {
        sqlx::query("UPDATE safeguarding.cases SET escalation_sent = $2 WHERE id = $1")
            .bind(id.0)
            .bind(level)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    /// Contacts to alert for a school: (name, email, phone, duty).
    pub async fn safeguarding_contacts(&self, school: SchoolId) -> AppResult<Vec<(String, Option<String>, Option<String>, Vec<String>)>> {
        sqlx::query_as(
            "SELECT a.first_name || ' ' || a.last_name, a.email, a.phone, m.duties
               FROM identity.memberships m JOIN identity.accounts a ON a.id = m.account_id
              WHERE m.school_id = $1 AND m.active AND (m.duties && ARRAY['dsl','deputy_dsl'])",
        )
        .bind(school.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)
    }
}
