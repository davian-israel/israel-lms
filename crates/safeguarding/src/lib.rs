//! Safeguarding bounded context: risk flags from pupil messages, cases owned by
//! the school's Designated Safeguarding Lead (DSL), and escalation.
//!
//! The classifier here is a deterministic, conservative keyword screen. It is
//! the always-on floor; an AI safety classifier can be added behind the same
//! port, but this one never goes away.

pub mod domain {
    use kernel::{CaseId, MembershipId, SchoolId};
    use serde::{Deserialize, Serialize};
    use time::{Duration, OffsetDateTime};

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Severity {
        Medium,
        High,
    }

    impl Severity {
        pub fn as_str(self) -> &'static str {
            match self {
                Severity::Medium => "medium",
                Severity::High => "high",
            }
        }
        pub fn parse(s: &str) -> Self {
            if s == "high" { Severity::High } else { Severity::Medium }
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct RiskFlag {
        pub severity: Severity,
        pub category: &'static str,
    }

    const HIGH: &[(&str, &str)] = &[
        ("kill myself", "self-harm or suicide"),
        ("want to die", "self-harm or suicide"),
        ("hurt myself", "self-harm or suicide"),
        ("end my life", "self-harm or suicide"),
        ("cut myself", "self-harm or suicide"),
        ("hits me", "possible abuse"),
        ("hit me", "possible abuse"),
        ("touches me", "possible abuse"),
        ("don't want to go home", "distress about home"),
        ("dont want to go home", "distress about home"),
        ("scared to go home", "distress about home"),
        ("not safe at home", "distress about home"),
    ];
    const MEDIUM: &[(&str, &str)] = &[
        ("nobody cares", "low mood"),
        ("no one cares", "low mood"),
        ("hate myself", "low mood"),
        ("i'm so sad", "low mood"),
        ("being bullied", "bullying"),
        ("they bully me", "bullying"),
        ("i'm scared", "distress"),
        ("cant cope", "distress"),
        ("can't cope", "distress"),
    ];

    /// Screen one pupil message. Returns the most serious match, if any.
    pub fn screen(text: &str) -> Option<RiskFlag> {
        let t = text.to_lowercase().replace('’', "'");
        for (needle, cat) in HIGH {
            if t.contains(needle) {
                return Some(RiskFlag { severity: Severity::High, category: cat });
            }
        }
        for (needle, cat) in MEDIUM {
            if t.contains(needle) {
                return Some(RiskFlag { severity: Severity::Medium, category: cat });
            }
        }
        None
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum CaseStatus {
        Open,
        Acknowledged,
        Closed,
    }

    impl CaseStatus {
        pub fn as_str(self) -> &'static str {
            match self {
                CaseStatus::Open => "open",
                CaseStatus::Acknowledged => "acknowledged",
                CaseStatus::Closed => "closed",
            }
        }
        pub fn parse(s: &str) -> Self {
            match s {
                "acknowledged" => CaseStatus::Acknowledged,
                "closed" => CaseStatus::Closed,
                _ => CaseStatus::Open,
            }
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct SafeguardingCase {
        pub id: CaseId,
        pub school_id: SchoolId,
        pub membership_id: MembershipId,
        pub severity: Severity,
        pub category: String,
        pub excerpt: String,
        pub status: CaseStatus,
        pub opened_at: OffsetDateTime,
        pub acknowledged_at: Option<OffsetDateTime>,
        pub action: Option<String>,
        pub notes: Option<String>,
        pub closed_at: Option<OffsetDateTime>,
    }

    #[derive(Debug, thiserror::Error, PartialEq)]
    pub enum CaseError {
        #[error("record the action you took before closing the alert")]
        NoAction,
        #[error("this alert is already closed")]
        AlreadyClosed,
    }

    pub const ESCALATE_TO_DEPUTY_AFTER: Duration = Duration::hours(1);
    pub const ESCALATE_TO_PLATFORM_AFTER: Duration = Duration::hours(2);

    impl SafeguardingCase {
        pub fn open(
            school_id: SchoolId,
            membership_id: MembershipId,
            severity: Severity,
            category: &str,
            excerpt: &str,
            now: OffsetDateTime,
        ) -> Self {
            Self {
                id: CaseId::new(),
                school_id,
                membership_id,
                severity,
                category: category.to_string(),
                excerpt: excerpt.chars().take(500).collect(),
                status: CaseStatus::Open,
                opened_at: now,
                acknowledged_at: None,
                action: None,
                notes: None,
                closed_at: None,
            }
        }

        pub fn acknowledge(&mut self, now: OffsetDateTime) -> Result<(), CaseError> {
            match self.status {
                CaseStatus::Closed => Err(CaseError::AlreadyClosed),
                CaseStatus::Acknowledged => Ok(()),
                CaseStatus::Open => {
                    self.status = CaseStatus::Acknowledged;
                    self.acknowledged_at = Some(now);
                    Ok(())
                }
            }
        }

        pub fn record_action(&mut self, action: &str, notes: &str) -> Result<(), CaseError> {
            if self.status == CaseStatus::Closed {
                return Err(CaseError::AlreadyClosed);
            }
            let a = action.trim();
            self.action = (!a.is_empty()).then(|| a.to_string());
            self.notes = Some(notes.trim().to_string()).filter(|n| !n.is_empty());
            Ok(())
        }

        pub fn close(&mut self, now: OffsetDateTime) -> Result<(), CaseError> {
            if self.status == CaseStatus::Closed {
                return Err(CaseError::AlreadyClosed);
            }
            if self.action.is_none() {
                return Err(CaseError::NoAction);
            }
            if self.acknowledged_at.is_none() {
                self.acknowledged_at = Some(now);
            }
            self.status = CaseStatus::Closed;
            self.closed_at = Some(now);
            Ok(())
        }

        /// Who must be alerted now, given how long the case has been open.
        pub fn escalation_level(&self, now: OffsetDateTime) -> u8 {
            if self.status != CaseStatus::Open {
                return 0;
            }
            let age = now - self.opened_at;
            if age >= ESCALATE_TO_PLATFORM_AFTER {
                3
            } else if age >= ESCALATE_TO_DEPUTY_AFTER {
                2
            } else {
                1
            }
        }
    }

    /// What the helper says to a pupil when a message is flagged. Plain,
    /// calm, no promises of secrecy, and always a route to a person.
    pub fn pupil_response(severity: Severity) -> &'static str {
        match severity {
            Severity::High => "Thank you for telling me. We'll stop the lesson for now. A teacher at your school will check in with you. If you want to talk to someone right now, you can call Childline free on 0800 1111, any time.",
            Severity::Medium => "Thanks for telling me. That sounds hard. You can talk to a teacher you trust, and Childline is free on 0800 1111. Would you like to keep going with the lesson or take a break?",
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use time::macros::datetime;

        #[test]
        fn screening_finds_high_and_medium() {
            assert_eq!(screen("i dont want to go home later").unwrap().severity, Severity::High);
            assert_eq!(screen("nobody cares about me").unwrap().severity, Severity::Medium);
            assert!(screen("what is 3/5 of 40?").is_none());
        }

        #[test]
        fn cannot_close_without_action() {
            let now = datetime!(2026-11-11 16:42 UTC);
            let mut c = SafeguardingCase::open(SchoolId::new(), MembershipId::new(), Severity::High, "x", "y", now);
            assert_eq!(c.close(now), Err(CaseError::NoAction));
            c.record_action("Spoke with the pupil", "").unwrap();
            assert!(c.close(now).is_ok());
            assert_eq!(c.status, CaseStatus::Closed);
        }

        #[test]
        fn escalates_after_one_hour() {
            let opened = datetime!(2026-11-11 16:42 UTC);
            let c = SafeguardingCase::open(SchoolId::new(), MembershipId::new(), Severity::High, "x", "y", opened);
            assert_eq!(c.escalation_level(datetime!(2026-11-11 17:00 UTC)), 1);
            assert_eq!(c.escalation_level(datetime!(2026-11-11 17:43 UTC)), 2);
            assert_eq!(c.escalation_level(datetime!(2026-11-11 18:43 UTC)), 3);
        }
    }
}

pub mod application {
    use crate::domain::SafeguardingCase;
    use async_trait::async_trait;
    use kernel::{AppResult, CaseId, SchoolId};

    #[async_trait]
    pub trait Cases: Send + Sync {
        async fn save(&self, c: &SafeguardingCase) -> AppResult<()>;
        async fn get(&self, school: SchoolId, id: CaseId) -> AppResult<SafeguardingCase>;
        async fn list(&self, school: SchoolId) -> AppResult<Vec<SafeguardingCase>>;
    }
}
