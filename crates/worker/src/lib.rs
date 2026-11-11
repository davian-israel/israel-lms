//! Worker: delivers domain events from the outbox to their handlers, and runs
//! scheduled jobs (nightly plans, Monday weekly reviews, safeguarding escalation).
//! Every handler is idempotent because delivery is at least once.

use infra::{outbox, read, repos::Repos, Db};
use kernel::{AppResult, Clock, ConceptId, DomainEvent, MembershipId, SystemClock};
use learner::application::LearnerStates;
use learner::domain::Outcome;
use notifications::application::Notifier;
use planning::application::Plans;
use safeguarding::application::Cases;
use safeguarding::domain::{SafeguardingCase, Severity};
use std::sync::Arc;
use std::time::Duration;
use time::Duration as TDuration;

pub struct Worker {
    pub db: Db,
    pub repos: Repos,
    pub notifier: Arc<dyn Notifier>,
    pub clock: Arc<dyn Clock>,
}

impl Worker {
    pub fn new(db: Db, notifier: Arc<dyn Notifier>) -> Self {
        let repos = Repos::new(db.pool.clone());
        Self { db, repos, notifier, clock: Arc::new(SystemClock) }
    }

    /// Run forever: drain the outbox every second, scheduled jobs every minute.
    pub async fn run(self) -> anyhow::Result<()> {
        tracing::info!("worker started");
        let mut tick: u64 = 0;
        loop {
            if let Err(e) = self.drain().await {
                tracing::error!(error = %e, "outbox drain failed");
            }
            if tick % 60 == 0 {
                if let Err(e) = self.scheduled().await {
                    tracing::error!(error = %e, "scheduled jobs failed");
                }
            }
            tick += 1;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    pub async fn drain(&self) -> AppResult<usize> {
        let batch = outbox::claim(&self.db.pool, 50).await?;
        let n = batch.len();
        for p in batch {
            match self.handle(&p.event).await {
                Ok(()) => outbox::done(&self.db.pool, p.id).await?,
                Err(e) => {
                    tracing::warn!(id = p.id, error = %e, "event handler failed; will retry");
                    outbox::failed(&self.db.pool, p.id, &e.to_string()).await?;
                }
            }
        }
        Ok(n)
    }

    pub async fn handle(&self, e: &DomainEvent) -> AppResult<()> {
        let today = self.clock.today();
        match e {
            DomainEvent::AnswerMarked { membership_id, concept_id, correct, misconception, .. } => {
                // Mastery update with a retry on concurrent change.
                for _ in 0..3 {
                    let mut s = self.repos.get_or_new(*membership_id, *concept_id).await?;
                    s.observe_answer(*correct);
                    match LearnerStates::save(&self.repos, &s).await {
                        Ok(()) => break,
                        Err(kernel::AppError::Conflict(_)) => continue,
                        Err(e) => return Err(e),
                    }
                }
                if let Some(m) = misconception {
                    self.repos.add_misconception(*membership_id, *concept_id, m).await?;
                    // A fix for tomorrow (next study day), if not already planned.
                    let prefs = self.repos.preferences(*membership_id).await?;
                    let on = prefs.next_study_day(today + TDuration::days(1));
                    let exists: bool = sqlx::query_scalar(
                        "SELECT EXISTS(SELECT 1 FROM planning.tasks WHERE membership_id = $1 AND concept_id = $2 AND kind = 'fix' AND done_at IS NULL)",
                    )
                    .bind(membership_id.0)
                    .bind(concept_id.0)
                    .fetch_one(&self.db.pool)
                    .await
                    .map_err(|e| kernel::AppError::Infra(e.to_string()))?;
                    if !exists && !read::plan_paused(&self.db.pool, membership_id.0).await? {
                        read::add_task(&self.db.pool, membership_id.0, concept_id.0, "fix", 5, on).await?;
                    }
                }
            }
            DomainEvent::LessonCompleted { membership_id, concept_id, on } => {
                let mut s = self.repos.get_or_new(*membership_id, *concept_id).await?;
                if s.learned_on.is_none() {
                    s.complete_lesson(*on);
                    LearnerStates::save(&self.repos, &s).await?;
                }
                self.rebuild(*membership_id).await?;
            }
            DomainEvent::ReviewCompleted { membership_id, concept_id, passed, on } => {
                let mut s = self.repos.get_or_new(*membership_id, *concept_id).await?;
                // Idempotent: only apply if the review was actually due on that day.
                if s.is_due(*on) {
                    let out = s.complete_review(*passed, *on);
                    LearnerStates::save(&self.repos, &s).await?;
                    if let Outcome::Lapsed { .. } = out {
                        tracing::info!(membership = %membership_id, concept = %concept_id, "review failed; fix planned");
                    }
                    if *passed {
                        read::mark_misconceptions_fixed(&self.db.pool, *membership_id, *concept_id).await?;
                    }
                }
                self.rebuild(*membership_id).await?;
            }
            DomainEvent::StudyPreferencesChanged { membership_id } | DomainEvent::PupilEnrolled { membership_id, .. } => {
                self.rebuild(*membership_id).await?;
            }
            DomainEvent::RiskFlagRaised { membership_id, school_id, severity, category, excerpt } => {
                let sev = Severity::parse(severity);
                let case = SafeguardingCase::open(*school_id, *membership_id, sev, category, excerpt, self.clock.now());
                Cases::save(&self.repos, &case).await?;
                if sev == Severity::High {
                    self.repos.set_paused(*membership_id, true, "safeguarding").await?;
                }
                outbox::write(&self.db.pool, &DomainEvent::CaseOpened {
                    case_id: case.id,
                    membership_id: *membership_id,
                    school_id: *school_id,
                    severity: severity.clone(),
                })
                .await?;
            }
            DomainEvent::CaseOpened { case_id, school_id, severity, .. } => {
                let contacts = self.repos.safeguarding_contacts(*school_id).await?;
                for (name, email, phone, duties) in contacts {
                    let urgent = severity == "high";
                    // Medium cases go to the lead only, in the daily digest; high to lead and deputy now.
                    if !urgent && !duties.iter().any(|d| d == "dsl") {
                        continue;
                    }
                    let subject = if urgent { "Safeguarding alert: high priority" } else { "Safeguarding flag for review" };
                    let body = format!("{name}, a safeguarding alert needs you. Sign in to Margin and open Safeguarding (case {case_id}). No pupil details are sent by email.");
                    if let Some(e) = email {
                        self.notifier.email(&e, subject, &body).await?;
                    }
                    if urgent {
                        if let Some(p) = phone {
                            self.notifier.sms(&p, "Margin: high-priority safeguarding alert. Please sign in now.").await?;
                        }
                    }
                }
            }
            DomainEvent::CaseClosed { membership_id, resume_plan, .. } => {
                if *resume_plan {
                    self.repos.set_paused(*membership_id, false, "").await?;
                    self.rebuild(*membership_id).await?;
                }
            }
            DomainEvent::ReflectionSubmitted { .. } | DomainEvent::MockCompleted { .. } => {}
        }
        Ok(())
    }

    async fn rebuild(&self, m: MembershipId) -> AppResult<()> {
        planning::application::rebuild(&self.repos, m, self.clock.today()).await.map(|_| ())
    }

    /// Jobs keyed by run so each happens once even with several workers.
    async fn once(&self, job: &str, key: &str) -> AppResult<bool> {
        let r = sqlx::query("INSERT INTO platform.cron_runs (job, run_key) VALUES ($1,$2) ON CONFLICT DO NOTHING")
            .bind(job)
            .bind(key)
            .execute(&self.db.pool)
            .await
            .map_err(|e| kernel::AppError::Infra(e.to_string()))?;
        Ok(r.rows_affected() == 1)
    }

    pub async fn scheduled(&self) -> AppResult<()> {
        let now = self.clock.now();
        let today = now.date();
        // Nightly plan rebuild (first run after 02:00 UTC each day).
        if now.hour() >= 2 && self.once("nightly_plans", &today.to_string()).await? {
            let students = read::active_students(&self.db.pool).await?;
            for s in &students {
                if let Err(e) = self.rebuild(MembershipId(*s)).await {
                    tracing::warn!(student = %s, error = %e, "plan rebuild failed");
                }
            }
            tracing::info!(count = students.len(), "nightly plans rebuilt");
        }
        // Monday weekly reviews (after 06:00 UTC).
        let week = kernel::week_start(today);
        if today == week && now.hour() >= 6 && self.once("weekly_reviews", &week.to_string()).await? {
            self.write_weekly_reviews(week).await?;
        }
        // Safeguarding escalation: deputy after 1 hour, platform after 2 hours.
        for (case, sent) in self.repos.open_cases().await? {
            let level = case.escalation_level(now) as i16;
            if level > sent {
                let contacts = self.repos.safeguarding_contacts(case.school_id).await?;
                for (name, email, phone, duties) in &contacts {
                    let to_deputy = level >= 2 && duties.iter().any(|d| d == "deputy_dsl");
                    if to_deputy {
                        if let Some(e) = email {
                            self.notifier.email(e, "Safeguarding alert not yet acknowledged", &format!("{name}, an alert has not been acknowledged for over an hour. Please sign in to Margin now.")).await?;
                        }
                        if let Some(p) = phone {
                            self.notifier.sms(p, "Margin: unacknowledged safeguarding alert. Please sign in now.").await?;
                        }
                    }
                }
                if level >= 3 {
                    tracing::error!(case = %case.id, school = %case.school_id, "SAFEGUARDING ESCALATION: unacknowledged for 2 hours; platform on-call notified");
                    self.notifier.email("safeguarding-oncall@margin.example", "Unacknowledged safeguarding alert", &format!("Case {} at school {} is unacknowledged after 2 hours.", case.id, case.school_id)).await?;
                }
                self.repos.mark_escalated(case.id, level).await?;
            }
        }
        Ok(())
    }

    pub async fn write_weekly_reviews(&self, week: time::Date) -> AppResult<usize> {
        let from = week - TDuration::days(7);
        let to = week - TDuration::days(1);
        let mut n = 0;
        for (class_id, _school, name) in read::all_classes(&self.db.pool).await? {
            let (pupils, studied) = read::class_studied(&self.db.pool, class_id, from, to).await?;
            let mistakes = read::class_mistakes(&self.db.pool, class_id, 7).await?;
            let body = reporting::domain::class_review(&name, pupils as usize, studied as usize, &mistakes);
            read::save_weekly_review(&self.db.pool, class_id, week, &body).await?;
            n += 1;
        }
        Ok(n)
    }
}

/// Used by tests and the seed command.
pub async fn rebuild_all(repos: &Repos, today: time::Date) -> AppResult<()> {
    for s in read::active_students(&repos.pool).await? {
        planning::application::rebuild(repos, MembershipId(s), today).await?;
    }
    Ok(())
}

#[allow(dead_code)]
fn _assert_ports(r: &Repos) -> (&dyn LearnerStates, &dyn Plans, &dyn Cases) {
    (r, r, r)
}

#[allow(dead_code)]
fn _unused(_: ConceptId) {}
