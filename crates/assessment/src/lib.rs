//! Assessment bounded context: items, marking, attempts and practice tests.

pub mod domain {
    use kernel::{ConceptId, ItemId};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum ItemKind {
        /// Pick one option.
        Choice,
        /// A number, optionally with units, commas or a pound sign.
        Numeric,
        /// Short text compared against accepted answers.
        Text,
    }

    impl ItemKind {
        pub fn parse(s: &str) -> Self {
            match s {
                "choice" | "mcq" => ItemKind::Choice,
                "numeric" => ItemKind::Numeric,
                _ => ItemKind::Text,
            }
        }
        pub fn as_str(self) -> &'static str {
            match self {
                ItemKind::Choice => "choice",
                ItemKind::Numeric => "numeric",
                ItemKind::Text => "text",
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct ItemOption {
        pub label: String,
        /// Explanation of the wrong idea this option reveals (None for the key).
        #[serde(default)]
        pub misconception: Option<String>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Item {
        pub id: ItemId,
        pub concept_id: ConceptId,
        pub kind: ItemKind,
        pub prompt: String,
        #[serde(default)]
        pub options: Vec<ItemOption>,
        /// Accepted answers separated by `|`.
        pub answer: String,
        /// The method shown after answering.
        pub explanation: String,
        #[serde(default)]
        pub hints: Vec<String>,
        #[serde(default = "one")]
        pub marks: i32,
    }

    fn one() -> i32 {
        1
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Marked {
        pub correct: bool,
        pub misconception: Option<String>,
        pub marks_awarded: i32,
    }

    #[derive(Debug, thiserror::Error, PartialEq)]
    pub enum ItemError {
        #[error("a choice item needs exactly one correct option")]
        NoSingleKey,
        #[error("every wrong option needs a misconception explanation")]
        UntaggedDistractor,
    }

    impl Item {
        /// Invariants checked before an item can go live.
        pub fn validate(&self) -> Result<(), ItemError> {
            if self.kind == ItemKind::Choice {
                let keys = self.options.iter().filter(|o| same_text(&o.label, &self.answer)).count();
                if keys != 1 {
                    return Err(ItemError::NoSingleKey);
                }
                if self
                    .options
                    .iter()
                    .any(|o| !same_text(&o.label, &self.answer) && o.misconception.is_none())
                {
                    return Err(ItemError::UntaggedDistractor);
                }
            }
            Ok(())
        }

        pub fn accepted(&self) -> impl Iterator<Item = &str> {
            self.answer.split('|').map(str::trim).filter(|s| !s.is_empty())
        }

        pub fn display_answer(&self) -> &str {
            self.accepted().next().unwrap_or("")
        }

        pub fn mark(&self, given: &str) -> Marked {
            let correct = match self.kind {
                ItemKind::Choice => self.accepted().any(|a| same_text(a, given)),
                ItemKind::Numeric => match parse_number(given) {
                    Some(n) => self
                        .accepted()
                        .filter_map(parse_number)
                        .any(|a| (a - n).abs() <= 1e-6_f64.max(a.abs() * 1e-9)),
                    None => false,
                },
                ItemKind::Text => self.accepted().any(|a| normalise(a) == normalise(given)),
            };
            let misconception = if correct {
                None
            } else {
                self.options
                    .iter()
                    .find(|o| same_text(&o.label, given))
                    .and_then(|o| o.misconception.clone())
            };
            Marked { correct, misconception, marks_awarded: if correct { self.marks } else { 0 } }
        }
    }

    fn same_text(a: &str, b: &str) -> bool {
        normalise(a) == normalise(b)
    }

    /// Lower-case, collapse spaces, drop trailing punctuation.
    pub fn normalise(s: &str) -> String {
        let lower = s.trim().to_lowercase();
        let collapsed: String = lower.split_whitespace().collect::<Vec<_>>().join(" ");
        collapsed.trim_end_matches(['.', '!', '?']).to_string()
    }

    /// Accept "24", "£27", "1,000", "20 m/s", "5400m", "-12%".
    pub fn parse_number(s: &str) -> Option<f64> {
        let cleaned: String = s
            .trim()
            .trim_start_matches('£')
            .chars()
            .filter(|c| *c != ',')
            .collect();
        let mut end = 0;
        for (i, c) in cleaned.char_indices() {
            if c.is_ascii_digit() || c == '.' || ((c == '-' || c == '+') && i == 0) {
                end = i + c.len_utf8();
            } else {
                break;
            }
        }
        cleaned[..end].parse::<f64>().ok()
    }

    /// Review sessions are passed with at least two thirds correct.
    pub fn review_passed(correct: usize, total: usize) -> bool {
        total > 0 && correct * 3 >= total * 2
    }

    /// Estimated KS2 scaled-score range for a raw arithmetic score out of 40.
    /// Illustrative linear mapping; replace with the published conversion table
    /// for the relevant year before pilot use.
    pub fn ks2_scaled_range(raw: i32, max: i32) -> (i32, i32) {
        if max <= 0 {
            return (80, 80);
        }
        let pct = raw as f64 / max as f64;
        let mid = 80.0 + pct * 40.0;
        ((mid - 2.0).round().clamp(80.0, 120.0) as i32, (mid + 2.0).round().clamp(80.0, 120.0) as i32)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn choice() -> Item {
            Item {
                id: ItemId::new(),
                concept_id: ConceptId::new(),
                kind: ItemKind::Choice,
                prompt: "What is 1/4 of 36?".into(),
                options: vec![
                    ItemOption { label: "9".into(), misconception: None },
                    ItemOption { label: "32".into(), misconception: Some("You subtracted 4.".into()) },
                ],
                answer: "9".into(),
                explanation: "36 ÷ 4 = 9".into(),
                hints: vec![],
                marks: 1,
            }
        }

        #[test]
        fn choice_marking_reports_misconception() {
            let i = choice();
            assert!(i.validate().is_ok());
            assert!(i.mark("9").correct);
            let m = i.mark("32");
            assert!(!m.correct);
            assert_eq!(m.misconception.as_deref(), Some("You subtracted 4."));
        }

        #[test]
        fn untagged_distractor_is_rejected() {
            let mut i = choice();
            i.options[1].misconception = None;
            assert_eq!(i.validate(), Err(ItemError::UntaggedDistractor));
        }

        #[test]
        fn numbers_with_units_and_symbols() {
            assert_eq!(parse_number("£27"), Some(27.0));
            assert_eq!(parse_number("1,000"), Some(1000.0));
            assert_eq!(parse_number("20 m/s"), Some(20.0));
            assert_eq!(parse_number("-12%"), Some(-12.0));
            assert_eq!(parse_number("abc"), None);
        }

        #[test]
        fn text_answers_ignore_case_and_full_stop() {
            let i = Item { kind: ItemKind::Text, answer: "are delivered".into(), options: vec![], ..choice() };
            assert!(i.mark("Are delivered.").correct);
        }

        #[test]
        fn two_thirds_passes_a_review() {
            assert!(review_passed(2, 3));
            assert!(!review_passed(1, 3));
        }
    }
}

pub mod application {
    use crate::domain::Item;
    use async_trait::async_trait;
    use kernel::{AppResult, AttemptId, ConceptId, ItemId, MembershipId, MockId};

    #[derive(Debug, Clone)]
    pub struct MockPaper {
        pub id: MockId,
        pub title: String,
        pub minutes: i32,
        pub items: Vec<Item>,
    }

    #[async_trait]
    pub trait ItemBank: Send + Sync {
        async fn items_for_concept(&self, c: ConceptId) -> AppResult<Vec<Item>>;
        async fn item(&self, id: ItemId) -> AppResult<Item>;
        async fn mock(&self, id: MockId) -> AppResult<MockPaper>;
    }

    #[async_trait]
    pub trait Attempts: Send + Sync {
        async fn start(&self, m: MembershipId, kind: &str, concept: Option<ConceptId>, mock: Option<MockId>) -> AppResult<AttemptId>;
        async fn record_response(&self, a: AttemptId, item: ItemId, answer: &str, correct: bool, misconception: Option<&str>) -> AppResult<()>;
        async fn finish(&self, a: AttemptId, score: i32, max: i32) -> AppResult<()>;
    }
}
