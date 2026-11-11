//! Seeding: curriculum content for every environment, plus an optional demo
//! school (OAKHILL) with one account per persona and two weeks of simulated
//! history produced by the real domain rules.

use crate::auth::hash_password;
use crate::db_err;
use crate::repos::Repos;
use assessment::domain::{Item, ItemKind, ItemOption};
use kernel::{AppResult, ConceptId, ItemId, MembershipId};
use learner::application::LearnerStates;
use rand::{rngs::StdRng, Rng, SeedableRng};
use serde::Deserialize;
use sqlx::PgPool;
use std::collections::HashMap;
use time::{Date, Duration, OffsetDateTime, Time};
use uuid::Uuid;

pub const DEMO_STAFF_PASSWORD: &str = "margin-demo-2026";
pub const DEMO_PUPIL_PASSWORD: &str = "maple-river-seven";

const CURRICULUM: &str = include_str!("../../../seeds/curriculum.json");

#[derive(Deserialize)]
struct Root {
    courses: Vec<CourseIn>,
}
#[derive(Deserialize)]
struct CourseIn {
    code: String,
    title: String,
    subject: String,
    key_stage: String,
    year_group: i32,
    colour: String,
    exam: String,
    concepts: Vec<ConceptIn>,
    #[serde(default)]
    mocks: Vec<MockIn>,
}
#[derive(Deserialize)]
struct ConceptIn {
    code: String,
    title: String,
    unit: String,
    lesson: serde_json::Value,
    quiz: Vec<ItemIn>,
    review: Vec<ItemIn>,
    flashcards: Vec<Card>,
    help: Card2,
    #[serde(default)]
    prerequisites: Vec<String>,
}
#[derive(Deserialize)]
struct Card {
    front: String,
    back: String,
}
#[derive(Deserialize)]
struct Card2 {
    title: String,
    body: String,
}
#[derive(Deserialize, Clone)]
struct ItemIn {
    kind: String,
    prompt: String,
    #[serde(default)]
    options: Vec<ItemOption>,
    answer: String,
    explanation: String,
    #[serde(default)]
    hints: Vec<String>,
    #[serde(default = "one")]
    marks: i32,
}
fn one() -> i32 {
    1
}
#[derive(Deserialize)]
struct MockIn {
    title: String,
    minutes: i32,
    items: Vec<ItemIn>,
    concept: String,
}

/// Load the curriculum. Idempotent: courses that already exist are skipped.
pub async fn curriculum(pool: &PgPool) -> AppResult<usize> {
    let root: Root = serde_json::from_str(CURRICULUM).map_err(|e| kernel::AppError::Infra(e.to_string()))?;
    let mut added = 0;
    for c in root.courses {
        let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM curriculum.courses WHERE code = $1")
            .bind(&c.code)
            .fetch_optional(pool)
            .await
            .map_err(db_err)?;
        if exists.is_some() {
            continue;
        }
        let mut tx = pool.begin().await.map_err(db_err)?;
        let course_id = Uuid::now_v7();
        sqlx::query("INSERT INTO curriculum.courses (id, code, title, subject, key_stage, year_group, colour, exam) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(course_id).bind(&c.code).bind(&c.title).bind(&c.subject).bind(&c.key_stage).bind(c.year_group).bind(&c.colour).bind(&c.exam)
            .execute(&mut *tx).await.map_err(db_err)?;
        let mut ids: HashMap<String, Uuid> = HashMap::new();
        for (pos, k) in c.concepts.iter().enumerate() {
            let cid = Uuid::now_v7();
            ids.insert(k.code.clone(), cid);
            sqlx::query("INSERT INTO curriculum.concepts (id, course_id, code, title, unit, position) VALUES ($1,$2,$3,$4,$5,$6)")
                .bind(cid).bind(course_id).bind(&k.code).bind(&k.title).bind(&k.unit).bind(pos as i32 + 1)
                .execute(&mut *tx).await.map_err(db_err)?;
            sqlx::query("INSERT INTO content.lessons (id, concept_id, title, body, status, ai_generated) VALUES ($1,$2,$3,$4,'live',false)")
                .bind(Uuid::now_v7()).bind(cid).bind(&k.title).bind(&k.lesson)
                .execute(&mut *tx).await.map_err(db_err)?;
            for (usage, list) in [("quiz", &k.quiz), ("review", &k.review)] {
                for (i, it) in list.iter().enumerate() {
                    insert_item(&mut tx, cid, it, usage, i as i32).await?;
                }
            }
            for f in &k.flashcards {
                sqlx::query("INSERT INTO content.flashcards (id, concept_id, front, back) VALUES ($1,$2,$3,$4)")
                    .bind(Uuid::now_v7()).bind(cid).bind(&f.front).bind(&f.back)
                    .execute(&mut *tx).await.map_err(db_err)?;
            }
            sqlx::query("INSERT INTO content.help_ideas (id, concept_id, title, body) VALUES ($1,$2,$3,$4)")
                .bind(Uuid::now_v7()).bind(cid).bind(&k.help.title).bind(&k.help.body)
                .execute(&mut *tx).await.map_err(db_err)?;
        }
        for k in &c.concepts {
            for p in &k.prerequisites {
                if let (Some(pre), Some(cur)) = (ids.get(p), ids.get(&k.code)) {
                    sqlx::query("INSERT INTO curriculum.concept_edges (prerequisite_id, concept_id) VALUES ($1,$2) ON CONFLICT DO NOTHING")
                        .bind(pre).bind(cur).execute(&mut *tx).await.map_err(db_err)?;
                }
            }
        }
        for mk in &c.mocks {
            let cid = ids.get(&mk.concept).copied().unwrap_or_else(|| *ids.values().next().unwrap());
            let mut item_ids = Vec::new();
            for (i, it) in mk.items.iter().enumerate() {
                item_ids.push(insert_item(&mut tx, cid, it, "mock", i as i32).await?);
            }
            sqlx::query("INSERT INTO assessment.mocks (id, course_id, title, minutes, item_ids) VALUES ($1,$2,$3,$4,$5)")
                .bind(Uuid::now_v7()).bind(course_id).bind(&mk.title).bind(mk.minutes).bind(&item_ids)
                .execute(&mut *tx).await.map_err(db_err)?;
        }
        tx.commit().await.map_err(db_err)?;
        added += 1;
    }
    Ok(added)
}

async fn insert_item(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, concept: Uuid, it: &ItemIn, usage: &str, pos: i32) -> AppResult<Uuid> {
    let id = Uuid::now_v7();
    // Invariant check before anything goes live.
    let item = Item {
        id: ItemId(id),
        concept_id: ConceptId(concept),
        kind: ItemKind::parse(&it.kind),
        prompt: it.prompt.clone(),
        options: it.options.clone(),
        answer: it.answer.clone(),
        explanation: it.explanation.clone(),
        hints: it.hints.clone(),
        marks: it.marks,
    };
    let status = if item.validate().is_ok() { "live" } else { "quarantined" };
    if status != "live" {
        tracing::warn!(prompt = %it.prompt, "seed item failed validation; quarantined");
    }
    sqlx::query("INSERT INTO assessment.items (id, concept_id, kind, prompt, options, answer, explanation, hints, marks, usage, status, position)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
        .bind(id).bind(concept).bind(item.kind.as_str()).bind(&it.prompt)
        .bind(serde_json::to_value(&it.options).unwrap_or_default()).bind(&it.answer).bind(&it.explanation)
        .bind(serde_json::to_value(&it.hints).unwrap_or_default()).bind(it.marks).bind(usage).bind(status).bind(pos)
        .execute(&mut **tx).await.map_err(db_err)?;
    Ok(id)
}

/// Create (or update the password of) the platform's system admin account.
pub async fn system_admin(pool: &PgPool, email: &str, password: &str) -> AppResult<()> {
    let aid: Option<Uuid> = sqlx::query_scalar("SELECT id FROM identity.accounts WHERE lower(email) = lower($1)")
        .bind(email).fetch_optional(pool).await.map_err(db_err)?;
    let aid = match aid {
        Some(a) => {
            sqlx::query("UPDATE identity.accounts SET password_hash = $2, locked = false, failed_logins = 0 WHERE id = $1")
                .bind(a).bind(hash_password(password)).execute(pool).await.map_err(db_err)?;
            a
        }
        None => {
            let a = Uuid::now_v7();
            sqlx::query("INSERT INTO identity.accounts (id, first_name, last_name, email, password_hash) VALUES ($1,'Dev','Operations',lower($2),$3)")
                .bind(a).bind(email).bind(hash_password(password)).execute(pool).await.map_err(db_err)?;
            a
        }
    };
    let has: Option<Uuid> = sqlx::query_scalar("SELECT id FROM identity.memberships WHERE account_id = $1 AND role = 'system_admin'")
        .bind(aid).fetch_optional(pool).await.map_err(db_err)?;
    if has.is_none() {
        sqlx::query("INSERT INTO identity.memberships (id, account_id, role) VALUES ($1,$2,'system_admin')")
            .bind(Uuid::now_v7()).bind(aid).execute(pool).await.map_err(db_err)?;
    }
    Ok(())
}

struct Persona {
    first: &'static str,
    last: &'static str,
    class: &'static str,
    year: i32,
    accuracy: f64,
    diligence: f64,
    stopped_days_ago: i64,
}

/// Demo school with personas. Skipped if OAKHILL already exists.
pub async fn demo(pool: &PgPool, today: Date) -> AppResult<bool> {
    let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM identity.schools WHERE code = 'OAKHILL'")
        .fetch_optional(pool).await.map_err(db_err)?;
    if exists.is_some() {
        return Ok(false);
    }
    let school = Uuid::now_v7();
    sqlx::query("INSERT INTO identity.schools (id, code, name, phase, active) VALUES ($1,'OAKHILL','Oakhill Academy','all-through',true)")
        .bind(school).execute(pool).await.map_err(db_err)?;
    let other = Uuid::now_v7();
    sqlx::query("INSERT INTO identity.schools (id, code, name, phase, active) VALUES ($1,'RIVERSIDE','Riverside Primary','primary',false)")
        .bind(other).execute(pool).await.map_err(db_err)?;

    let class6 = Uuid::now_v7();
    let class10 = Uuid::now_v7();
    for (id, name, year) in [(class6, "6B", 6), (class10, "10A", 10)] {
        sqlx::query("INSERT INTO identity.classes (id, school_id, name, year_group) VALUES ($1,$2,$3,$4)")
            .bind(id).bind(school).bind(name).bind(year).execute(pool).await.map_err(db_err)?;
    }

    let staff_hash = hash_password(DEMO_STAFF_PASSWORD);
    let pupil_hash = hash_password(DEMO_PUPIL_PASSWORD);

    // Staff: (first, last, email, role, duties, classes)
    let staff = [
        ("Rachel", "Hughes", "rachel.hughes@oakhill.example", "teacher", vec![], vec![class6]),
        ("Owen", "Clarke", "owen.clarke@oakhill.example", "teacher", vec!["dsl"], vec![class10]),
        ("Sandra", "Patel", "sandra.patel@oakhill.example", "school_admin", vec!["deputy_dsl"], vec![]),
    ];
    for (f, l, e, role, duties, classes) in staff {
        let aid = Uuid::now_v7();
        let mid = Uuid::now_v7();
        sqlx::query("INSERT INTO identity.accounts (id, first_name, last_name, email, password_hash, phone) VALUES ($1,$2,$3,$4,$5,'+447700900000')")
            .bind(aid).bind(f).bind(l).bind(e).bind(&staff_hash).execute(pool).await.map_err(db_err)?;
        let duties: Vec<String> = duties.into_iter().map(String::from).collect();
        sqlx::query("INSERT INTO identity.memberships (id, account_id, school_id, role, duties) VALUES ($1,$2,$3,$4,$5)")
            .bind(mid).bind(aid).bind(school).bind(role).bind(&duties).execute(pool).await.map_err(db_err)?;
        for c in classes {
            sqlx::query("INSERT INTO identity.class_members (class_id, membership_id) VALUES ($1,$2)")
                .bind(c).bind(mid).execute(pool).await.map_err(db_err)?;
        }
    }
    // Riverside has an admin only (shows tenancy and the activation rule).
    let raid = Uuid::now_v7();
    sqlx::query("INSERT INTO identity.accounts (id, first_name, last_name, email, password_hash) VALUES ($1,'Grace','Mensah','grace.mensah@riverside.example',$2)")
        .bind(raid).bind(&staff_hash).execute(pool).await.map_err(db_err)?;
    sqlx::query("INSERT INTO identity.memberships (id, account_id, school_id, role) VALUES ($1,$2,$3,'school_admin')")
        .bind(Uuid::now_v7()).bind(raid).bind(other).execute(pool).await.map_err(db_err)?;

    let personas = [
        Persona { first: "Amara", last: "Kofi", class: "6B", year: 6, accuracy: 0.8, diligence: 0.9, stopped_days_ago: 0 },
        Persona { first: "Ben", last: "Okafor", class: "6B", year: 6, accuracy: 0.75, diligence: 0.8, stopped_days_ago: 0 },
        Persona { first: "Chloe", last: "Davies", class: "6B", year: 6, accuracy: 0.85, diligence: 0.7, stopped_days_ago: 0 },
        Persona { first: "Leo", last: "Martin", class: "6B", year: 6, accuracy: 0.6, diligence: 0.8, stopped_days_ago: 6 },
        Persona { first: "Mia", last: "Rossi", class: "6B", year: 6, accuracy: 0.9, diligence: 0.95, stopped_days_ago: 0 },
        Persona { first: "Priya", last: "Shah", class: "6B", year: 6, accuracy: 0.45, diligence: 0.85, stopped_days_ago: 0 },
        Persona { first: "Sam", last: "Taylor", class: "6B", year: 6, accuracy: 0.7, diligence: 0.6, stopped_days_ago: 0 },
        Persona { first: "Tom", last: "Walker", class: "6B", year: 6, accuracy: 0.55, diligence: 0.75, stopped_days_ago: 0 },
        Persona { first: "Zainab", last: "Ali", class: "10A", year: 10, accuracy: 0.8, diligence: 0.85, stopped_days_ago: 0 },
        Persona { first: "Daniel", last: "Evans", class: "10A", year: 10, accuracy: 0.65, diligence: 0.6, stopped_days_ago: 0 },
    ];
    let repos = Repos::new(pool.clone());
    let mut rng = StdRng::seed_from_u64(20261111);
    let mut amara = None;
    for p in &personas {
        let aid = Uuid::now_v7();
        let mid = Uuid::now_v7();
        let username = format!("{}.{}", p.first.to_lowercase(), p.last[..1].to_lowercase());
        sqlx::query("INSERT INTO identity.accounts (id, first_name, last_name, password_hash) VALUES ($1,$2,$3,$4)")
            .bind(aid).bind(p.first).bind(p.last).bind(&pupil_hash).execute(pool).await.map_err(db_err)?;
        sqlx::query("INSERT INTO identity.memberships (id, account_id, school_id, role, username, year_group) VALUES ($1,$2,$3,'student',$4,$5)")
            .bind(mid).bind(aid).bind(school).bind(&username).bind(p.year).execute(pool).await.map_err(db_err)?;
        let class = if p.class == "6B" { class6 } else { class10 };
        sqlx::query("INSERT INTO identity.class_members (class_id, membership_id) VALUES ($1,$2)")
            .bind(class).bind(mid).execute(pool).await.map_err(db_err)?;
        let courses: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM curriculum.courses WHERE year_group = $1")
            .bind(p.year).fetch_all(pool).await.map_err(db_err)?;
        for c in &courses {
            sqlx::query("INSERT INTO planning.enrolments (membership_id, course_id) VALUES ($1,$2)")
                .bind(mid).bind(c).execute(pool).await.map_err(db_err)?;
        }
        sqlx::query("INSERT INTO planning.preferences (membership_id) VALUES ($1)").bind(mid).execute(pool).await.map_err(db_err)?;
        simulate(pool, &repos, MembershipId(mid), &courses, p, today, &mut rng).await?;
        if p.first == "Amara" {
            amara = Some(mid);
        }
    }

    // Parent Joyce, linked to Amara.
    let jid = Uuid::now_v7();
    let jm = Uuid::now_v7();
    sqlx::query("INSERT INTO identity.accounts (id, first_name, last_name, email) VALUES ($1,'Joyce','Kofi','joyce.kofi@example.com')")
        .bind(jid).execute(pool).await.map_err(db_err)?;
    sqlx::query("INSERT INTO identity.memberships (id, account_id, school_id, role) VALUES ($1,$2,$3,'parent')")
        .bind(jm).bind(jid).bind(school).execute(pool).await.map_err(db_err)?;
    if let Some(a) = amara {
        sqlx::query("INSERT INTO identity.parent_links (id, school_id, student_membership_id, parent_membership_id, invite_code, verified_at) VALUES ($1,$2,$3,$4,$5, now())")
            .bind(Uuid::now_v7()).bind(school).bind(a).bind(jm).bind(identity::domain::invite_code())
            .execute(pool).await.map_err(db_err)?;
    }

    // Build every pupil's plan for this week.
    let students: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM identity.memberships WHERE role = 'student'")
        .fetch_all(pool).await.map_err(db_err)?;
    for s in students {
        planning::application::rebuild(&repos, MembershipId(s), today).await?;
    }
    Ok(true)
}

/// Two weeks of study, generated by the real Learner Model rules.
async fn simulate(pool: &PgPool, repos: &Repos, m: MembershipId, courses: &[Uuid], p: &Persona, today: Date, rng: &mut StdRng) -> AppResult<()> {
    let concepts: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT c.id, c.course_id FROM curriculum.concepts c WHERE c.course_id = ANY($1) ORDER BY c.position, c.course_id",
    )
    .bind(courses).fetch_all(pool).await.map_err(db_err)?;
    let mut next_new = concepts.iter().map(|(c, _)| *c);
    let start = today - Duration::days(14);
    let mut tries = 0;
    for d in 0..14 {
        let day = start + Duration::days(d);
        let wd = kernel::weekday_number(day);
        if wd > 5 || (p.stopped_days_ago > 0 && (today - day).whole_days() <= p.stopped_days_ago) {
            continue;
        }
        if rng.gen::<f64>() > p.diligence {
            continue;
        }
        let at = OffsetDateTime::new_utc(day, Time::from_hms(16, 10, 0).unwrap());
        let mut minutes = 0;
        // Due reviews first.
        let due: Vec<Uuid> = sqlx::query_scalar(
            "SELECT concept_id FROM learner.concept_states WHERE membership_id = $1 AND stage BETWEEN 1 AND 5 AND due_on <= $2",
        )
        .bind(m.0).bind(day).fetch_all(pool).await.map_err(db_err)?;
        for c in due {
            let items = repos.items(ConceptId(c), "review").await?;
            let (right, total) = answer_items(pool, repos, m, &items, "review", c, p.accuracy + 0.05, at, rng).await?;
            let mut st = repos.get_or_new(m, ConceptId(c)).await?;
            st.complete_review(assessment::domain::review_passed(right, total), day);
            repos.save(&st).await?;
            done_task(pool, m, c, "review", day, 4).await?;
            minutes += 4;
        }
        // One or two new lessons.
        let lessons = if rng.gen::<f64>() < 0.5 { 2 } else { 1 };
        for _ in 0..lessons {
            if minutes + 12 > 30 {
                break;
            }
            let Some(c) = next_new.next() else { break };
            let items = repos.items(ConceptId(c), "quiz").await?;
            answer_items(pool, repos, m, &items, "lesson", c, p.accuracy, at, rng).await?;
            let mut st = repos.get_or_new(m, ConceptId(c)).await?;
            st.complete_lesson(day);
            repos.save(&st).await?;
            done_task(pool, m, c, "learn", day, 12).await?;
            minutes += 12;
            if p.first == "Tom" {
                crate::read::save_reflection(pool, m, ConceptId(c), "I just know how to do it, it's easy", 3, "Good start. One thing to add next time: say each step.").await?;
            }
        }
        tries += 1;
    }
    let _ = tries;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn answer_items(
    pool: &PgPool, repos: &Repos, m: MembershipId, items: &[Item], kind: &str, concept: Uuid, accuracy: f64, at: OffsetDateTime, rng: &mut StdRng,
) -> AppResult<(usize, usize)> {
    let attempt = Uuid::now_v7();
    sqlx::query("INSERT INTO assessment.attempts (id, membership_id, kind, concept_id, started_at, finished_at) VALUES ($1,$2,$3,$4,$5,$5)")
        .bind(attempt).bind(m.0).bind(kind).bind(concept).bind(at).execute(pool).await.map_err(db_err)?;
    let mut right = 0;
    let mut st = repos.get_or_new(m, ConceptId(concept)).await?;
    for it in items {
        let correct = rng.gen::<f64>() < accuracy;
        let given = if correct {
            it.display_answer().to_string()
        } else {
            it.options.iter().find(|o| o.misconception.is_some()).map(|o| o.label.clone()).unwrap_or_else(|| "?".into())
        };
        let marked = it.mark(&given);
        if marked.correct {
            right += 1;
        }
        st.observe_answer(marked.correct);
        sqlx::query("INSERT INTO assessment.responses (id, attempt_id, item_id, answer, correct, misconception, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7)")
            .bind(Uuid::now_v7()).bind(attempt).bind(it.id.0).bind(&given).bind(marked.correct).bind(&marked.misconception).bind(at)
            .execute(pool).await.map_err(db_err)?;
        if let Some(mis) = &marked.misconception {
            repos.add_misconception(m, ConceptId(concept), mis).await?;
        }
    }
    repos.save(&st).await?;
    sqlx::query("UPDATE assessment.attempts SET score = $2, max_score = $3 WHERE id = $1")
        .bind(attempt).bind(right as i32).bind(items.len() as i32).execute(pool).await.map_err(db_err)?;
    Ok((right, items.len()))
}

async fn done_task(pool: &PgPool, m: MembershipId, concept: Uuid, kind: &str, day: Date, minutes: i32) -> AppResult<()> {
    sqlx::query("INSERT INTO planning.tasks (id, membership_id, on_date, kind, concept_id, minutes, position, done_at) VALUES ($1,$2,$3,$4,$5,$6,0,$7)")
        .bind(Uuid::now_v7()).bind(m.0).bind(day).bind(kind).bind(concept).bind(minutes)
        .bind(OffsetDateTime::new_utc(day, Time::from_hms(16, 30, 0).unwrap()))
        .execute(pool).await.map_err(db_err)?;
    Ok(())
}
