//! Planning bounded context: study preferences and the 7-day study plan.
//!
//! The plan is rebuilt nightly and whenever evidence changes. Invariants:
//! each day stays within its minute budget, rest days get no tasks, due reviews
//! come before fixes and new lessons, and nothing is planned while paused.

pub mod domain {
    use kernel::{weekday_number, ConceptId};
    use serde::{Deserialize, Serialize};
    use time::{Date, Duration};

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum TaskKind {
        Review,
        Fix,
        Learn,
        Test,
    }

    impl TaskKind {
        pub fn as_str(self) -> &'static str {
            match self {
                TaskKind::Review => "review",
                TaskKind::Fix => "fix",
                TaskKind::Learn => "learn",
                TaskKind::Test => "test",
            }
        }
        pub fn parse(s: &str) -> Self {
            match s {
                "review" => TaskKind::Review,
                "fix" => TaskKind::Fix,
                "test" => TaskKind::Test,
                _ => TaskKind::Learn,
            }
        }
        pub fn minutes(self) -> i32 {
            match self {
                TaskKind::Review => 4,
                TaskKind::Fix => 5,
                TaskKind::Learn => 12,
                TaskKind::Test => 30,
            }
        }
        pub fn label(self) -> &'static str {
            match self {
                TaskKind::Review => "Review",
                TaskKind::Fix => "Fix a mistake",
                TaskKind::Learn => "New lesson",
                TaskKind::Test => "Practice test",
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct StudyPreferences {
        /// ISO weekday numbers (Mon = 1).
        pub days: Vec<u8>,
        pub minutes: i32,
    }

    impl Default for StudyPreferences {
        fn default() -> Self {
            Self { days: vec![1, 2, 3, 4, 5], minutes: 25 }
        }
    }

    impl StudyPreferences {
        pub fn is_study_day(&self, d: Date) -> bool {
            self.days.contains(&weekday_number(d))
        }
        /// The first study day on or after `d` (searches two weeks).
        pub fn next_study_day(&self, d: Date) -> Date {
            (0..14)
                .map(|i| d + Duration::days(i))
                .find(|x| self.is_study_day(*x))
                .unwrap_or(d)
        }
        pub fn validate(&self) -> Result<(), PlanError> {
            if self.days.is_empty() {
                return Err(PlanError::NoStudyDays);
            }
            if !(10..=120).contains(&self.minutes) {
                return Err(PlanError::BadBudget);
            }
            Ok(())
        }
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct PlannedTask {
        pub on: Date,
        pub kind: TaskKind,
        pub concept_id: ConceptId,
        pub minutes: i32,
        pub order: i32,
    }

    /// What the planner knows about a pupil when it builds the week.
    #[derive(Debug, Clone, Default)]
    pub struct PlanningInput {
        /// (concept, due date) for every concept in the review ladder.
        pub reviews: Vec<(ConceptId, Date)>,
        /// Concepts with a recent mistake that needs a fix.
        pub fixes: Vec<ConceptId>,
        /// Concepts not yet learned, in course order (prerequisites first).
        pub new_concepts: Vec<ConceptId>,
        pub paused: bool,
    }

    #[derive(Debug, thiserror::Error, PartialEq)]
    pub enum PlanError {
        #[error("choose at least one study day")]
        NoStudyDays,
        #[error("minutes a day must be between 10 and 120")]
        BadBudget,
    }

    /// Build the next 7 days from `today`. Pure: no IO, fully testable.
    pub fn build_week(today: Date, prefs: &StudyPreferences, input: &PlanningInput) -> Vec<PlannedTask> {
        let mut out = Vec::new();
        if input.paused {
            return out;
        }
        // Place each review on its due date, moved forward to a study day.
        let mut reviews: Vec<(ConceptId, Date)> = input
            .reviews
            .iter()
            .map(|(c, d)| (*c, prefs.next_study_day((*d).max(today))))
            .collect();
        reviews.sort_by_key(|(_, d)| *d);
        let mut fixes = input.fixes.clone();
        fixes.dedup();
        let mut new_iter = input.new_concepts.iter().copied();

        for i in 0..7 {
            let day = today + Duration::days(i);
            if !prefs.is_study_day(day) {
                continue;
            }
            let mut used = 0;
            let mut order = 0;
            let push = |kind: TaskKind, c: ConceptId, used: &mut i32, order: &mut i32, out: &mut Vec<PlannedTask>| {
                out.push(PlannedTask { on: day, kind, concept_id: c, minutes: kind.minutes(), order: *order });
                *used += kind.minutes();
                *order += 1;
            };
            // 1. reviews due today (always placed; the ladder must not slip)
            for (c, _) in reviews.iter().filter(|(_, d)| *d == day) {
                push(TaskKind::Review, *c, &mut used, &mut order, &mut out);
            }
            // 2. fixes, first study day only gets up to two
            if i == 0 || out.iter().all(|t| t.kind != TaskKind::Fix) {
                let mut placed = 0;
                while placed < 2 && used + TaskKind::Fix.minutes() <= prefs.minutes {
                    match fixes.pop() {
                        Some(c) => {
                            push(TaskKind::Fix, c, &mut used, &mut order, &mut out);
                            placed += 1;
                        }
                        None => break,
                    }
                }
            }
            // 3. new lessons while budget allows
            while used + TaskKind::Learn.minutes() <= prefs.minutes {
                match new_iter.next() {
                    Some(c) => push(TaskKind::Learn, c, &mut used, &mut order, &mut out),
                    None => break,
                }
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use time::macros::date;

        #[test]
        fn reviews_skip_weekends_and_come_first() {
            let prefs = StudyPreferences::default();
            let c = ConceptId::new();
            let n = ConceptId::new();
            let input = PlanningInput {
                reviews: vec![(c, date!(2026 - 11 - 14))], // Saturday
                new_concepts: vec![n],
                ..Default::default()
            };
            let week = build_week(date!(2026 - 11 - 11), &prefs, &input);
            let review = week.iter().find(|t| t.kind == TaskKind::Review).unwrap();
            assert_eq!(review.on, date!(2026 - 11 - 16)); // moved to Monday
            assert!(week.iter().all(|t| prefs.is_study_day(t.on)));
            let first_day: Vec<_> = week.iter().filter(|t| t.on == date!(2026 - 11 - 11)).collect();
            assert_eq!(first_day[0].kind, TaskKind::Learn);
        }

        #[test]
        fn new_lessons_respect_budget() {
            let prefs = StudyPreferences { days: vec![1, 2, 3, 4, 5, 6, 7], minutes: 25 };
            let input = PlanningInput { new_concepts: (0..20).map(|_| ConceptId::new()).collect(), ..Default::default() };
            let week = build_week(date!(2026 - 11 - 11), &prefs, &input);
            for i in 0..7 {
                let d = date!(2026 - 11 - 11) + Duration::days(i);
                let mins: i32 = week.iter().filter(|t| t.on == d).map(|t| t.minutes).sum();
                assert!(mins <= 25, "{d}: {mins}");
            }
        }

        #[test]
        fn paused_plan_is_empty() {
            let input = PlanningInput { new_concepts: vec![ConceptId::new()], paused: true, ..Default::default() };
            assert!(build_week(date!(2026 - 11 - 11), &StudyPreferences::default(), &input).is_empty());
        }

        #[test]
        fn preferences_need_a_day() {
            let p = StudyPreferences { days: vec![], minutes: 25 };
            assert_eq!(p.validate(), Err(PlanError::NoStudyDays));
        }
    }
}

pub mod application {
    use crate::domain::{PlannedTask, PlanningInput, StudyPreferences};
    use async_trait::async_trait;
    use kernel::{AppResult, MembershipId};
    use time::Date;

    #[async_trait]
    pub trait Plans: Send + Sync {
        async fn preferences(&self, m: MembershipId) -> AppResult<StudyPreferences>;
        async fn save_preferences(&self, m: MembershipId, p: &StudyPreferences) -> AppResult<()>;
        async fn planning_input(&self, m: MembershipId, today: Date) -> AppResult<PlanningInput>;
        /// Replace open tasks from `from` onwards; finished tasks are kept.
        async fn replace_week(&self, m: MembershipId, from: Date, tasks: &[PlannedTask]) -> AppResult<()>;
        async fn set_paused(&self, m: MembershipId, paused: bool, reason: &str) -> AppResult<()>;
    }

    /// Application service: rebuild one pupil's week.
    pub async fn rebuild(plans: &dyn Plans, m: MembershipId, today: Date) -> AppResult<usize> {
        let prefs = plans.preferences(m).await?;
        let input = plans.planning_input(m, today).await?;
        let week = crate::domain::build_week(today, &prefs, &input);
        plans.replace_week(m, today, &week).await?;
        Ok(week.len())
    }
}
