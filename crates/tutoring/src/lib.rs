//! Tutoring bounded context: the hints-first helper and reflection feedback.

pub mod domain {
    /// Hints are given one step at a time; the full method is only shown after
    /// the pupil has made at least one attempt and used the hint ladder.
    #[derive(Debug, Clone, PartialEq)]
    pub struct HintLadder {
        pub hints: Vec<String>,
        pub shown: usize,
        pub attempts: u32,
    }

    impl HintLadder {
        pub fn new(hints: Vec<String>) -> Self {
            Self { hints, shown: 0, attempts: 0 }
        }
        pub fn next_hint(&mut self) -> Option<&str> {
            if self.shown < self.hints.len() {
                self.shown += 1;
                self.hints.get(self.shown - 1).map(String::as_str)
            } else {
                None
            }
        }
        pub fn may_show_solution(&self) -> bool {
            self.attempts >= 1 && self.shown >= self.hints.len()
        }
    }

    /// Feedback on a pupil's "explain it to a friend" reflection. Checks that the
    /// key ideas are present; one line of praise for effort and one thing to add.
    pub fn reflection_feedback(text: &str, key_ideas: &[(&str, &str)]) -> String {
        let t = text.to_lowercase();
        if t.trim().split_whitespace().count() < 4 {
            return "Try writing a full sentence or two, as if you were explaining it to a friend.".into();
        }
        let missing: Vec<&str> = key_ideas
            .iter()
            .filter(|(needle, _)| !needle.split('|').any(|n| t.contains(n)))
            .map(|(_, advice)| *advice)
            .collect();
        match missing.first() {
            None => "Clear explanation. You covered the key steps.".into(),
            Some(advice) => format!("Good start. One thing to add next time: {advice}"),
        }
    }

    /// System prompt for the AI helper: lesson-scoped, hints first, no persona,
    /// no flattery, never the final answer by default (DfE standards 10, 11, 13).
    pub fn helper_system_prompt(year_group: i32, lesson_title: &str, lesson_text: &str) -> String {
        format!(
            "You are the lesson helper in a UK school learning platform. The pupil is in Year {year_group}. \
             The lesson is \"{lesson_title}\". Only discuss this lesson. Give one short hint at a time, \
             in plain words for this age. Never give the final answer unless the pupil has already tried \
             and asks again. Do not claim to have feelings, do not act as a friend, do not flatter, and do not \
             use guilt or pressure. If the pupil seems upset or unsafe, say a teacher will check in and that \
             Childline is free on 0800 1111. Keep replies under 60 words.\n\nLesson notes:\n{lesson_text}"
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn hints_come_one_at_a_time() {
            let mut l = HintLadder::new(vec!["a".into(), "b".into()]);
            assert_eq!(l.next_hint(), Some("a"));
            assert!(!l.may_show_solution());
            assert_eq!(l.next_hint(), Some("b"));
            assert_eq!(l.next_hint(), None);
            l.attempts = 1;
            assert!(l.may_show_solution());
        }

        #[test]
        fn reflection_names_one_missing_idea() {
            let ideas = [("divide|÷", "say you divide by the bottom number."), ("check|smaller", "add a check.")];
            let f = reflection_feedback("Divide 30 by 5 to get 6 then times by 2", &ideas);
            assert!(f.contains("add a check"));
            let g = reflection_feedback("divide by 5 then times 2 and check it is smaller", &ideas);
            assert!(g.starts_with("Clear"));
        }
    }
}

pub mod application {
    use async_trait::async_trait;
    use kernel::AppResult;

    /// Port to a text-generating model. Implemented in infra (Anthropic API) with
    /// a deterministic fallback when no key is configured.
    #[async_trait]
    pub trait TextGenerator: Send + Sync {
        fn is_live(&self) -> bool;
        async fn reply(&self, system: &str, user: &str, purpose: &str) -> AppResult<Option<String>>;
    }
}
