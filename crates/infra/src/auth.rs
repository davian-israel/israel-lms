//! Sign-in for the five roles, server-side sessions and parent email codes.

use crate::db_err;
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use kernel::{AppError, AppResult, Duty, MembershipId, Role, SchoolId};
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

pub const MAX_FAILED_LOGINS: i32 = 10;

pub fn hash_password(pw: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default().hash_password(pw.as_bytes(), &salt).expect("argon2 hash").to_string()
}

pub fn verify_password(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

fn sha(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Who is signed in. Built from the session on every request.
#[derive(Debug, Clone)]
pub struct Principal {
    pub membership_id: MembershipId,
    pub account_id: Uuid,
    pub role: Role,
    pub duties: Vec<String>,
    pub school_id: Option<SchoolId>,
    pub school_name: String,
    pub school_code: String,
    pub first_name: String,
    pub last_name: String,
    pub username: Option<String>,
    pub year_group: Option<i32>,
    pub email: Option<String>,
}

impl Principal {
    pub fn display_name(&self) -> String {
        let initial: String = self.last_name.chars().take(1).collect();
        if initial.is_empty() { self.first_name.clone() } else { format!("{} {}", self.first_name, initial) }
    }
    pub fn has_duty(&self, d: Duty) -> bool {
        self.duties.iter().any(|x| x == d.as_str())
    }
    pub fn is_dsl(&self) -> bool {
        self.has_duty(Duty::SafeguardingLead) || self.has_duty(Duty::DeputySafeguardingLead)
    }
    pub fn school(&self) -> AppResult<SchoolId> {
        self.school_id.ok_or(AppError::Forbidden)
    }
}

#[derive(sqlx::FromRow)]
struct PrincipalRow {
    membership_id: Uuid,
    account_id: Uuid,
    role: String,
    duties: Vec<String>,
    school_id: Option<Uuid>,
    school_name: Option<String>,
    school_code: Option<String>,
    first_name: String,
    last_name: String,
    username: Option<String>,
    year_group: Option<i32>,
    email: Option<String>,
}

const PRINCIPAL_SQL: &str = "SELECT m.id AS membership_id, a.id AS account_id, m.role, m.duties, m.school_id,
       s.name AS school_name, s.code AS school_code, a.first_name, a.last_name, m.username, m.year_group, a.email
  FROM identity.memberships m
  JOIN identity.accounts a ON a.id = m.account_id
  LEFT JOIN identity.schools s ON s.id = m.school_id";

impl From<PrincipalRow> for Principal {
    fn from(r: PrincipalRow) -> Self {
        Principal {
            membership_id: MembershipId(r.membership_id),
            account_id: r.account_id,
            role: Role::parse(&r.role).unwrap_or(Role::Student),
            duties: r.duties,
            school_id: r.school_id.map(SchoolId),
            school_name: r.school_name.unwrap_or_else(|| "Margin platform".into()),
            school_code: r.school_code.unwrap_or_default(),
            first_name: r.first_name,
            last_name: r.last_name,
            username: r.username,
            year_group: r.year_group,
            email: r.email,
        }
    }
}

pub async fn principal(pool: &PgPool, m: MembershipId) -> AppResult<Principal> {
    let r: PrincipalRow = sqlx::query_as(&format!("{PRINCIPAL_SQL} WHERE m.id = $1 AND m.active"))
        .bind(m.0)
        .fetch_one(pool)
        .await
        .map_err(db_err)?;
    Ok(r.into())
}

pub async fn memberships_for_account(pool: &PgPool, account: Uuid) -> AppResult<Vec<Principal>> {
    let rows: Vec<PrincipalRow> = sqlx::query_as(&format!("{PRINCIPAL_SQL} WHERE a.id = $1 AND m.active ORDER BY m.created_at"))
        .bind(account)
        .fetch_all(pool)
        .await
        .map_err(db_err)?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Session lifetime by role (shared devices get short sessions).
pub fn session_length(role: Role) -> Duration {
    match role {
        Role::Student => Duration::hours(8),
        Role::Parent => Duration::days(30),
        Role::Teacher => Duration::hours(12),
        Role::SchoolAdmin => Duration::hours(8),
        Role::SystemAdmin => Duration::hours(4),
    }
}

pub async fn create_session(pool: &PgPool, m: MembershipId, role: Role) -> AppResult<String> {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    sqlx::query("INSERT INTO identity.sessions (token_hash, membership_id, expires_at) VALUES ($1, $2, $3)")
        .bind(sha(&token))
        .bind(m.0)
        .bind(OffsetDateTime::now_utc() + session_length(role))
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(token)
}

pub async fn principal_for_token(pool: &PgPool, token: &str) -> AppResult<Option<Principal>> {
    let m: Option<Uuid> = sqlx::query_scalar(
        "SELECT membership_id FROM identity.sessions WHERE token_hash = $1 AND expires_at > now()",
    )
    .bind(sha(token))
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;
    match m {
        Some(id) => principal(pool, MembershipId(id)).await.map(Some),
        None => Ok(None),
    }
}

pub async fn end_session(pool: &PgPool, token: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM identity.sessions WHERE token_hash = $1")
        .bind(sha(token))
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(())
}

/// Switch a signed-in person to another of their memberships.
pub async fn switch_session(pool: &PgPool, token: &str, to: MembershipId, account: Uuid) -> AppResult<()> {
    let ok: Option<Uuid> = sqlx::query_scalar("SELECT id FROM identity.memberships WHERE id = $1 AND account_id = $2 AND active")
        .bind(to.0)
        .bind(account)
        .fetch_optional(pool)
        .await
        .map_err(db_err)?;
    if ok.is_none() {
        return Err(AppError::Forbidden);
    }
    sqlx::query("UPDATE identity.sessions SET membership_id = $2 WHERE token_hash = $1")
        .bind(sha(token))
        .bind(to.0)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(())
}

pub struct SchoolRef {
    pub id: SchoolId,
    pub name: String,
    pub code: String,
    pub active: bool,
}

pub async fn school_by_code(pool: &PgPool, code: &str) -> AppResult<Option<SchoolRef>> {
    let r: Option<(Uuid, String, String, bool)> =
        sqlx::query_as("SELECT id, name, code, active FROM identity.schools WHERE code = $1")
            .bind(code)
            .fetch_optional(pool)
            .await
            .map_err(db_err)?;
    Ok(r.map(|(id, name, code, active)| SchoolRef { id: SchoolId(id), name, code, active }))
}

#[derive(Debug, PartialEq)]
pub enum LoginError {
    Wrong,
    Locked,
    SchoolInactive,
}

/// Pupil sign-in with school code, username and password.
pub async fn student_login(pool: &PgPool, school: SchoolId, username: &str, password: &str) -> AppResult<Result<MembershipId, LoginError>> {
    let row: Option<(Uuid, Uuid, Option<String>, i32, bool)> = sqlx::query_as(
        "SELECT m.id, a.id, a.password_hash, a.failed_logins, a.locked
           FROM identity.memberships m JOIN identity.accounts a ON a.id = m.account_id
          WHERE m.school_id = $1 AND lower(m.username) = lower($2) AND m.role = 'student' AND m.active",
    )
    .bind(school.0)
    .bind(username.trim())
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;
    let Some((mid, aid, hash, _fails, locked)) = row else { return Ok(Err(LoginError::Wrong)) };
    if locked {
        return Ok(Err(LoginError::Locked));
    }
    if hash.as_deref().map(|h| verify_password(password.trim(), h)).unwrap_or(false) {
        sqlx::query("UPDATE identity.accounts SET failed_logins = 0 WHERE id = $1").bind(aid).execute(pool).await.map_err(db_err)?;
        Ok(Ok(MembershipId(mid)))
    } else {
        sqlx::query("UPDATE identity.accounts SET failed_logins = failed_logins + 1, locked = (failed_logins + 1 >= $2) WHERE id = $1")
            .bind(aid)
            .bind(MAX_FAILED_LOGINS)
            .execute(pool)
            .await
            .map_err(db_err)?;
        Ok(Err(LoginError::Wrong))
    }
}

/// Staff and system admin sign-in with email and password. Returns the account.
pub async fn staff_login(pool: &PgPool, email: &str, password: &str) -> AppResult<Result<Uuid, LoginError>> {
    let row: Option<(Uuid, Option<String>, bool)> =
        sqlx::query_as("SELECT id, password_hash, locked FROM identity.accounts WHERE lower(email) = lower($1)")
            .bind(email.trim())
            .fetch_optional(pool)
            .await
            .map_err(db_err)?;
    let Some((aid, hash, locked)) = row else {
        // Spend similar time to avoid revealing which emails exist.
        let _ = verify_password(password, "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHQ$aGFzaGhhc2hoYXNo");
        return Ok(Err(LoginError::Wrong));
    };
    if locked {
        return Ok(Err(LoginError::Locked));
    }
    if hash.as_deref().map(|h| verify_password(password, h)).unwrap_or(false) {
        sqlx::query("UPDATE identity.accounts SET failed_logins = 0 WHERE id = $1").bind(aid).execute(pool).await.map_err(db_err)?;
        Ok(Ok(aid))
    } else {
        sqlx::query("UPDATE identity.accounts SET failed_logins = failed_logins + 1, locked = (failed_logins + 1 >= $2) WHERE id = $1")
            .bind(aid)
            .bind(MAX_FAILED_LOGINS)
            .execute(pool)
            .await
            .map_err(db_err)?;
        Ok(Err(LoginError::Wrong))
    }
}

/// Parent sign-in step 1. Returns the code to send, or None if the email has
/// no parent membership. Rate-limited to 5 codes an hour.
pub async fn issue_parent_code(pool: &PgPool, email: &str) -> AppResult<Option<String>> {
    let has_parent: Option<Uuid> = sqlx::query_scalar(
        "SELECT m.id FROM identity.memberships m JOIN identity.accounts a ON a.id = m.account_id
          WHERE lower(a.email) = lower($1) AND m.role = 'parent' AND m.active LIMIT 1",
    )
    .bind(email.trim())
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;
    if has_parent.is_none() {
        return Ok(None);
    }
    let recent: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM identity.login_codes WHERE lower(email) = lower($1) AND created_at > now() - interval '1 hour'",
    )
    .bind(email.trim())
    .fetch_one(pool)
    .await
    .map_err(db_err)?;
    if recent >= 5 {
        return Err(AppError::Invalid("Too many codes requested. Wait an hour and try again.".into()));
    }
    let code = identity::domain::one_time_code();
    sqlx::query("INSERT INTO identity.login_codes (email, code_hash, expires_at) VALUES (lower($1), $2, now() + interval '10 minutes')")
        .bind(email.trim())
        .bind(sha(&code))
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(Some(code))
}

/// Parent sign-in step 2: a valid unused code returns the parent membership.
pub async fn verify_parent_code(pool: &PgPool, email: &str, code: &str) -> AppResult<Option<MembershipId>> {
    let cleaned: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
    let ok: Option<(String, OffsetDateTime)> = sqlx::query_as(
        "UPDATE identity.login_codes SET used = true
          WHERE ctid = (SELECT ctid FROM identity.login_codes WHERE email = lower($1) AND code_hash = $2
                         AND NOT used AND expires_at > now() ORDER BY created_at DESC LIMIT 1)
          RETURNING email, expires_at",
    )
    .bind(email.trim())
    .bind(sha(&cleaned))
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;
    if ok.is_none() {
        return Ok(None);
    }
    let m: Option<Uuid> = sqlx::query_scalar(
        "SELECT m.id FROM identity.memberships m JOIN identity.accounts a ON a.id = m.account_id
          WHERE lower(a.email) = lower($1) AND m.role = 'parent' AND m.active ORDER BY m.created_at LIMIT 1",
    )
    .bind(email.trim())
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;
    Ok(m.map(MembershipId))
}

pub async fn reset_student_password(pool: &PgPool, school: SchoolId, student: MembershipId) -> AppResult<String> {
    let pw = identity::domain::passphrase();
    let n = sqlx::query(
        "UPDATE identity.accounts a SET password_hash = $3, failed_logins = 0, locked = false
           FROM identity.memberships m WHERE m.account_id = a.id AND m.id = $2 AND m.school_id = $1 AND m.role = 'student'",
    )
    .bind(school.0)
    .bind(student.0)
    .bind(hash_password(&pw))
    .execute(pool)
    .await
    .map_err(db_err)?
    .rows_affected();
    if n == 0 {
        return Err(AppError::NotFound);
    }
    Ok(pw)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn password_round_trip() {
        let h = hash_password("maple-river-seven");
        assert!(verify_password("maple-river-seven", &h));
        assert!(!verify_password("wrong", &h));
    }
}
