//! Learner Model bounded context.
//!
//! Owns `LearnerConceptState`: how well one pupil knows one concept (mastery, by
//! Bayesian Knowledge Tracing) and when they should next review it (the 21-day
//! review ladder). The state changes only by applying evidence.

pub mod domain {
    use kernel::{ConceptId, MembershipId};
    use serde::{Deserialize, Serialize};
    use time::{Date, Duration};

    /// Bayesian Knowledge Tracing parameters (conservative defaults).
    pub const P_INIT: f64 = 0.2;
    pub const P_LEARN: f64 = 0.15;
    pub const P_SLIP: f64 = 0.1;
    pub const P_GUESS: f64 = 0.2;
    pub const MASTERED: f64 = 0.85;

    /// Days after first learning at which each review falls (R1..R5).
    pub const LADDER_DAYS: [i64; 5] = [1, 3, 7, 14, 21];
    /// After a concept is secured, keep it fresh with a long-term check.
    pub const LONG_TERM_GAP_DAYS: i64 = 24;

    /// Stage: 0 = not started, 1 = learned (R1 due), 2..=5 = Rn due, 6 = secured.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct LearnerConceptState {
        pub membership_id: MembershipId,
        pub concept_id: ConceptId,
        pub mastery: f64,
        pub stage: u8,
        pub learned_on: Option<Date>,
        pub due_on: Option<Date>,
        pub secured_on: Option<Date>,
        pub lapses: u32,
        pub version: i32,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub enum Outcome {
        /// The pupil passed; next review date given.
        Advanced { next_due: Date },
        /// Passed the day-21 review.
        Secured { next_due: Date },
        /// Failed; dropped back. A fix should be planned.
        Lapsed { next_due: Date, back_to_stage: u8 },
    }

    impl LearnerConceptState {
        pub fn new(membership_id: MembershipId, concept_id: ConceptId) -> Self {
            Self {
                membership_id,
                concept_id,
                mastery: P_INIT,
                stage: 0,
                learned_on: None,
                due_on: None,
                secured_on: None,
                lapses: 0,
                version: 0,
            }
        }

        pub fn is_mastered(&self) -> bool {
            self.mastery >= MASTERED
        }
        pub fn is_secured(&self) -> bool {
            self.stage >= 6
        }
        pub fn is_started(&self) -> bool {
            self.stage > 0
        }
        /// Review number (1..=5) that is currently due, if any.
        pub fn review_number(&self) -> Option<u8> {
            (1..=5).contains(&self.stage).then_some(self.stage)
        }

        /// Update mastery from one answer (BKT posterior then learning transition).
        pub fn observe_answer(&mut self, correct: bool) {
            let p = self.mastery;
            let posterior = if correct {
                p * (1.0 - P_SLIP) / (p * (1.0 - P_SLIP) + (1.0 - p) * P_GUESS)
            } else {
                p * P_SLIP / (p * P_SLIP + (1.0 - p) * (1.0 - P_GUESS))
            };
            self.mastery = (posterior + (1.0 - posterior) * P_LEARN).clamp(0.01, 0.99);
        }

        /// The pupil finished the lesson for this concept (day 0).
        pub fn complete_lesson(&mut self, on: Date) -> Date {
            if self.stage == 0 {
                self.stage = 1;
                self.learned_on = Some(on);
            }
            let due = on + Duration::days(LADDER_DAYS[0]);
            if self.stage == 1 {
                self.due_on = Some(due);
            }
            self.due_on.unwrap_or(due)
        }

        /// Apply the result of a scheduled review. Passing moves up the ladder;
        /// failing drops back as in the specification's 21-day table.
        pub fn complete_review(&mut self, passed: bool, on: Date) -> Outcome {
            let stage = self.stage.max(1);
            if passed {
                if stage >= 5 {
                    self.stage = 6;
                    self.secured_on.get_or_insert(on);
                    let next = on + Duration::days(LONG_TERM_GAP_DAYS);
                    self.due_on = Some(next);
                    return Outcome::Secured { next_due: next };
                }
                self.stage = stage + 1;
                let gap = LADDER_DAYS[self.stage as usize - 1] - LADDER_DAYS[self.stage as usize - 2];
                let next = on + Duration::days(gap.max(1));
                self.due_on = Some(next);
                Outcome::Advanced { next_due: next }
            } else {
                self.lapses += 1;
                // R1 -> R1, R2 -> R1, R3 -> R2, R4 -> R2, R5 -> R3, secured -> R3
                let back = match stage {
                    1 | 2 => 1,
                    3 => 2,
                    4 => 2,
                    _ => 3,
                };
                self.stage = back;
                self.secured_on = None;
                let next = on + Duration::days(1);
                self.due_on = Some(next);
                Outcome::Lapsed { next_due: next, back_to_stage: back }
            }
        }

        pub fn is_due(&self, on: Date) -> bool {
            self.stage >= 1 && self.due_on.map(|d| d <= on).unwrap_or(false)
        }

        pub fn level_label(&self) -> &'static str {
            if self.is_secured() {
                "Secured"
            } else if self.is_mastered() {
                "Mastered"
            } else if self.mastery >= 0.6 {
                "Getting there"
            } else if self.is_started() {
                "Learning"
            } else {
                "Not started"
            }
        }

        /// 0..=4 bucket for heatmaps (matches the design system mastery scale).
        pub fn level(&self) -> u8 {
            if self.is_secured() {
                4
            } else if self.is_mastered() {
                3
            } else if self.mastery >= 0.6 {
                2
            } else if self.is_started() {
                1
            } else {
                0
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use time::macros::date;

        fn state() -> LearnerConceptState {
            LearnerConceptState::new(MembershipId::new(), ConceptId::new())
        }

        #[test]
        fn ladder_follows_1_3_7_14_21_when_all_passed() {
            let mut s = state();
            let d0 = date!(2026 - 11 - 04);
            assert_eq!(s.complete_lesson(d0), date!(2026 - 11 - 05));
            let mut on = date!(2026 - 11 - 05);
            let expected = [date!(2026 - 11 - 07), date!(2026 - 11 - 11), date!(2026 - 11 - 18), date!(2026 - 11 - 25)];
            for exp in expected {
                match s.complete_review(true, on) {
                    Outcome::Advanced { next_due } => {
                        assert_eq!(next_due, exp);
                        on = next_due;
                    }
                    other => panic!("unexpected {other:?}"),
                }
            }
            assert!(matches!(s.complete_review(true, on), Outcome::Secured { .. }));
            assert!(s.is_secured());
        }

        #[test]
        fn failing_review_4_drops_to_review_2() {
            let mut s = state();
            s.complete_lesson(date!(2026 - 11 - 01));
            s.stage = 4;
            let out = s.complete_review(false, date!(2026 - 11 - 15));
            assert_eq!(out, Outcome::Lapsed { next_due: date!(2026 - 11 - 16), back_to_stage: 2 });
            assert_eq!(s.lapses, 1);
        }

        #[test]
        fn mastery_rises_with_correct_answers_and_falls_with_wrong() {
            let mut s = state();
            for _ in 0..5 {
                s.observe_answer(true);
            }
            assert!(s.is_mastered(), "mastery {}", s.mastery);
            let before = s.mastery;
            s.observe_answer(false);
            assert!(s.mastery < before);
        }
    }
}

pub mod application {
    use crate::domain::LearnerConceptState;
    use async_trait::async_trait;
    use kernel::{AppResult, ConceptId, MembershipId};

    /// Repository port, implemented in the infra crate.
    #[async_trait]
    pub trait LearnerStates: Send + Sync {
        async fn get_or_new(&self, m: MembershipId, c: ConceptId) -> AppResult<LearnerConceptState>;
        async fn save(&self, s: &LearnerConceptState) -> AppResult<()>;
        async fn add_misconception(&self, m: MembershipId, c: ConceptId, code: &str) -> AppResult<()>;
    }
}
