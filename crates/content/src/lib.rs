//! Content Generation bounded context: lessons, flashcards and help ideas, with
//! a publishing lifecycle that keeps unevaluated content away from pupils.

pub mod domain {
    use kernel::{ConceptId, LessonId};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum ContentStatus {
        Draft,
        Evaluating,
        Live,
        Quarantined,
        Retired,
    }

    impl ContentStatus {
        pub fn as_str(self) -> &'static str {
            match self {
                ContentStatus::Draft => "draft",
                ContentStatus::Evaluating => "evaluating",
                ContentStatus::Live => "live",
                ContentStatus::Quarantined => "quarantined",
                ContentStatus::Retired => "retired",
            }
        }
        pub fn parse(s: &str) -> Self {
            match s {
                "draft" => ContentStatus::Draft,
                "evaluating" => ContentStatus::Evaluating,
                "quarantined" => ContentStatus::Quarantined,
                "retired" => ContentStatus::Retired,
                _ => ContentStatus::Live,
            }
        }
        /// Allowed transitions of the content lifecycle.
        pub fn can_move_to(self, next: ContentStatus) -> bool {
            use ContentStatus::*;
            matches!(
                (self, next),
                (Draft, Evaluating)
                    | (Evaluating, Live)
                    | (Evaluating, Quarantined)
                    | (Quarantined, Evaluating)
                    | (Quarantined, Retired)
                    | (Live, Retired)
                    | (Live, Quarantined)
            )
        }
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Lesson {
        pub id: LessonId,
        pub concept_id: ConceptId,
        pub title: String,
        /// Paragraphs of the Read step (plain text; rendered safely).
        pub read: Vec<String>,
        pub steps: Vec<String>,
        pub check: String,
        pub worked_example: Vec<(String, String)>,
        pub reflect_prompt: String,
        /// (keywords separated by |, advice if missing)
        pub reflect_ideas: Vec<(String, String)>,
        pub status: ContentStatus,
        pub ai_generated: bool,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct HelpIdea {
        pub concept_id: ConceptId,
        pub title: String,
        pub body: String,
    }

    #[cfg(test)]
    mod tests {
        use super::ContentStatus::*;

        #[test]
        fn live_requires_evaluation() {
            assert!(!Draft.can_move_to(Live));
            assert!(Draft.can_move_to(Evaluating));
            assert!(Evaluating.can_move_to(Live));
            assert!(!Retired.can_move_to(Live));
        }
    }
}
