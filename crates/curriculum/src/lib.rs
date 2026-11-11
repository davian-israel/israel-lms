//! Curriculum bounded context: courses, concepts and the prerequisite graph.
//! Publishes concept ids (published language) to every other context.

pub mod domain {
    use kernel::{ConceptId, CourseId};
    use serde::{Deserialize, Serialize};
    use std::collections::{HashMap, HashSet};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Course {
        pub id: CourseId,
        pub code: String,
        pub title: String,
        pub subject: String,
        pub key_stage: String,
        pub year_group: i32,
        pub colour: String,
        pub exam: String,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Concept {
        pub id: ConceptId,
        pub course_id: CourseId,
        pub code: String,
        pub title: String,
        pub unit: String,
        pub position: i32,
        pub prerequisites: Vec<ConceptId>,
    }

    #[derive(Debug, thiserror::Error, PartialEq)]
    pub enum CurriculumError {
        #[error("the prerequisite graph has a cycle")]
        Cycle,
    }

    /// Order concepts so every prerequisite comes first; ties keep course order.
    /// Fails if the graph has a cycle (an invariant of a published curriculum).
    pub fn learning_order(concepts: &[Concept]) -> Result<Vec<ConceptId>, CurriculumError> {
        let ids: HashSet<ConceptId> = concepts.iter().map(|c| c.id).collect();
        let mut sorted: Vec<&Concept> = concepts.iter().collect();
        sorted.sort_by_key(|c| c.position);
        let mut done: HashSet<ConceptId> = HashSet::new();
        let mut out = Vec::with_capacity(concepts.len());
        let prereqs: HashMap<ConceptId, Vec<ConceptId>> = concepts
            .iter()
            .map(|c| (c.id, c.prerequisites.iter().copied().filter(|p| ids.contains(p)).collect()))
            .collect();
        while out.len() < sorted.len() {
            let next = sorted
                .iter()
                .find(|c| !done.contains(&c.id) && prereqs[&c.id].iter().all(|p| done.contains(p)));
            match next {
                Some(c) => {
                    done.insert(c.id);
                    out.push(c.id);
                }
                None => return Err(CurriculumError::Cycle),
            }
        }
        Ok(out)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn c(pos: i32, pre: Vec<ConceptId>) -> Concept {
            Concept { id: ConceptId::new(), course_id: CourseId::new(), code: String::new(), title: String::new(), unit: String::new(), position: pos, prerequisites: pre }
        }

        #[test]
        fn prerequisites_come_first() {
            let a = c(2, vec![]);
            let b = c(1, vec![a.id]);
            let order = learning_order(&[a.clone(), b.clone()]).unwrap();
            assert_eq!(order, vec![a.id, b.id]);
        }

        #[test]
        fn cycles_are_rejected() {
            let mut a = c(1, vec![]);
            let b = c(2, vec![a.id]);
            a.prerequisites.push(b.id);
            assert_eq!(learning_order(&[a, b]), Err(CurriculumError::Cycle));
        }
    }
}
