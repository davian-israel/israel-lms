//! Shared kernel: identifiers, roles, clock helpers and the published language of
//! domain events. Every bounded context depends on this crate and nothing else
//! from the workspace (except other contexts' published ids, which live here).

use serde::{Deserialize, Serialize};
use std::fmt;
use time::{Date, Duration, OffsetDateTime, Weekday};
use uuid::Uuid;

macro_rules! id_type {
    ($($name:ident),* $(,)?) => {$(
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);
        impl $name {
            pub fn new() -> Self { Self(Uuid::now_v7()) }
        }
        impl Default for $name { fn default() -> Self { Self::new() } }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0.fmt(f) }
        }
        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> { Uuid::parse_str(s).map(Self) }
        }
    )*};
}

id_type!(
    SchoolId, AccountId, MembershipId, ClassId, CourseId, ConceptId, LessonId, ItemId,
    AttemptId, MockId, CaseId, TaskId
);

/// The five roles of the platform. Duties (safeguarding lead, deputy) attach to
/// teacher or school admin memberships and are not roles of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Student,
    Parent,
    Teacher,
    SchoolAdmin,
    SystemAdmin,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Student => "student",
            Role::Parent => "parent",
            Role::Teacher => "teacher",
            Role::SchoolAdmin => "school_admin",
            Role::SystemAdmin => "system_admin",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "student" => Role::Student,
            "parent" => Role::Parent,
            "teacher" => Role::Teacher,
            "school_admin" => Role::SchoolAdmin,
            "system_admin" => Role::SystemAdmin,
            _ => return None,
        })
    }
    pub fn label(self) -> &'static str {
        match self {
            Role::Student => "Student",
            Role::Parent => "Parent or carer",
            Role::Teacher => "Teacher",
            Role::SchoolAdmin => "School admin",
            Role::SystemAdmin => "System admin",
        }
    }
    pub fn is_staff(self) -> bool {
        matches!(self, Role::Teacher | Role::SchoolAdmin)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Duty {
    SafeguardingLead,
    DeputySafeguardingLead,
}

impl Duty {
    pub fn as_str(self) -> &'static str {
        match self {
            Duty::SafeguardingLead => "dsl",
            Duty::DeputySafeguardingLead => "deputy_dsl",
        }
    }
}

/// Clock abstraction so domain rules about dates are testable.
pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
    fn today(&self) -> Date {
        self.now().date()
    }
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// Study days are stored as ISO weekday numbers, Monday = 1 ... Sunday = 7.
pub fn weekday_number(d: Date) -> u8 {
    d.weekday().number_from_monday()
}

pub fn weekday_short(d: Date) -> &'static str {
    match d.weekday() {
        Weekday::Monday => "Mon",
        Weekday::Tuesday => "Tue",
        Weekday::Wednesday => "Wed",
        Weekday::Thursday => "Thu",
        Weekday::Friday => "Fri",
        Weekday::Saturday => "Sat",
        Weekday::Sunday => "Sun",
    }
}

pub fn long_date(d: Date) -> String {
    let wd = match d.weekday() {
        Weekday::Monday => "Monday",
        Weekday::Tuesday => "Tuesday",
        Weekday::Wednesday => "Wednesday",
        Weekday::Thursday => "Thursday",
        Weekday::Friday => "Friday",
        Weekday::Saturday => "Saturday",
        Weekday::Sunday => "Sunday",
    };
    format!("{} {} {}", wd, d.day(), month_name(d))
}

pub fn short_date(d: Date) -> String {
    format!("{} {} {}", weekday_short(d), d.day(), &month_name(d)[..3])
}

fn month_name(d: Date) -> &'static str {
    use time::Month::*;
    match d.month() {
        January => "January",
        February => "February",
        March => "March",
        April => "April",
        May => "May",
        June => "June",
        July => "July",
        August => "August",
        September => "September",
        October => "October",
        November => "November",
        December => "December",
    }
}

/// Monday of the week containing `d`.
pub fn week_start(d: Date) -> Date {
    d - Duration::days(i64::from(d.weekday().number_days_from_monday()))
}

/// Domain events: the published language contexts use to talk to each other.
/// They are written to the outbox in the same transaction as the change that
/// raised them and delivered at least once, so every handler is idempotent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DomainEvent {
    PupilEnrolled {
        membership_id: MembershipId,
        course_id: CourseId,
    },
    AnswerMarked {
        membership_id: MembershipId,
        concept_id: ConceptId,
        item_id: ItemId,
        correct: bool,
        misconception: Option<String>,
    },
    LessonCompleted {
        membership_id: MembershipId,
        concept_id: ConceptId,
        on: Date,
    },
    ReviewCompleted {
        membership_id: MembershipId,
        concept_id: ConceptId,
        passed: bool,
        on: Date,
    },
    ReflectionSubmitted {
        membership_id: MembershipId,
        concept_id: ConceptId,
        confidence: u8,
    },
    MockCompleted {
        membership_id: MembershipId,
        attempt_id: AttemptId,
        score: i32,
        max_score: i32,
    },
    RiskFlagRaised {
        membership_id: MembershipId,
        school_id: SchoolId,
        severity: String,
        category: String,
        excerpt: String,
    },
    CaseOpened {
        case_id: CaseId,
        membership_id: MembershipId,
        school_id: SchoolId,
        severity: String,
    },
    CaseClosed {
        case_id: CaseId,
        membership_id: MembershipId,
        resume_plan: bool,
    },
    StudyPreferencesChanged {
        membership_id: MembershipId,
    },
}

impl DomainEvent {
    pub fn type_name(&self) -> &'static str {
        match self {
            DomainEvent::PupilEnrolled { .. } => "pupil_enrolled",
            DomainEvent::AnswerMarked { .. } => "answer_marked",
            DomainEvent::LessonCompleted { .. } => "lesson_completed",
            DomainEvent::ReviewCompleted { .. } => "review_completed",
            DomainEvent::ReflectionSubmitted { .. } => "reflection_submitted",
            DomainEvent::MockCompleted { .. } => "mock_completed",
            DomainEvent::RiskFlagRaised { .. } => "risk_flag_raised",
            DomainEvent::CaseOpened { .. } => "case_opened",
            DomainEvent::CaseClosed { .. } => "case_closed",
            DomainEvent::StudyPreferencesChanged { .. } => "study_preferences_changed",
        }
    }
}

/// Error shared by application services. Contexts define richer domain errors
/// and convert into this at the application boundary.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,
    #[error("not allowed")]
    Forbidden,
    #[error("{0}")]
    Invalid(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("infrastructure: {0}")]
    Infra(String),
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn week_start_is_monday() {
        assert_eq!(week_start(date!(2026 - 11 - 11)), date!(2026 - 11 - 09));
        assert_eq!(weekday_short(date!(2026 - 11 - 11)), "Wed");
    }

    #[test]
    fn events_round_trip_as_json() {
        let e = DomainEvent::ReviewCompleted {
            membership_id: MembershipId::new(),
            concept_id: ConceptId::new(),
            passed: true,
            on: date!(2026 - 11 - 11),
        };
        let s = serde_json::to_string(&e).unwrap();
        let back: DomainEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(e, back);
        assert_eq!(back.type_name(), "review_completed");
    }
}
