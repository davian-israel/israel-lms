//! Outbound adapters: PostgreSQL repositories (SQLx), the outbox, sessions,
//! the AI provider, notifications and demo seeding. Implements the ports the
//! bounded contexts declare; contexts never depend on this crate.

pub mod ai;
pub mod auth;
pub mod db;
pub mod notify;
pub mod outbox;
pub mod read;
pub mod repos;
pub mod seed;

pub use db::Db;

use kernel::AppError;

pub(crate) fn db_err(e: sqlx::Error) -> AppError {
    match e {
        sqlx::Error::RowNotFound => AppError::NotFound,
        other => AppError::Infra(other.to_string()),
    }
}
