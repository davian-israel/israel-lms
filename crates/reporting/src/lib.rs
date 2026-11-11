//! Reporting bounded context (read side): dashboards, weekly reviews and the
//! parent's weekly page. Read models only; it never changes other contexts.

pub mod domain {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    pub struct PupilWeek {
        pub minutes_by_day: Vec<(String, i32)>,
        pub days_studied: i32,
        pub total_minutes: i32,
        pub secured: Vec<String>,
        pub learned: Vec<String>,
        pub quiz_right: i64,
        pub quiz_total: i64,
        pub reviews_passed: i64,
        pub reviews_total: i64,
        pub mock: Option<(String, i32, i32)>,
        /// (concept title, misconception text, count)
        pub mistakes: Vec<(String, String, i64)>,
    }

    impl PupilWeek {
        pub fn headline(&self) -> &'static str {
            match (self.days_studied, self.reviews_total) {
                (0, _) => "a quiet one",
                (d, t) if d >= 4 && (t == 0 || self.reviews_passed * 10 >= t * 8) => "a strong one",
                (d, _) if d >= 3 => "a steady one",
                _ => "a light one",
            }
        }
    }

    /// The Monday weekly review for a teacher, written from the class data.
    /// Deterministic so it works without an AI provider; an AI rewrite can be
    /// layered on top through the TextGenerator port.
    pub fn class_review(
        class_name: &str,
        pupils: usize,
        studied: usize,
        top_mistakes: &[(String, String, i64)],
    ) -> String {
        let mut s = format!("{studied} of {pupils} pupils in {class_name} studied last week.");
        if let Some((concept, misconception, n)) = top_mistakes.first() {
            s.push_str(&format!(
                " The most common mistake was in {concept}: {n} answers showed \"{}\". Suggested: a 10-minute whole-class re-teach of {concept}, and the plans already add extra reviews for the pupils affected.",
                misconception.trim_end_matches('.')
            ));
        } else {
            s.push_str(" No repeated mistakes stood out. Plans continue as set.");
        }
        s
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn headline_reflects_effort() {
            let w = PupilWeek { days_studied: 4, reviews_total: 10, reviews_passed: 9, ..Default::default() };
            assert_eq!(w.headline(), "a strong one");
            assert_eq!(PupilWeek::default().headline(), "a quiet one");
        }

        #[test]
        fn review_mentions_top_mistake() {
            let r = class_review("6B", 28, 24, &[("Fractions of amounts".into(), "Divided by the numerator.".into(), 11)]);
            assert!(r.contains("24 of 28"));
            assert!(r.contains("Fractions of amounts"));
        }
    }
}
