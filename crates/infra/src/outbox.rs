//! Transactional outbox: events are stored with the change that raised them.

use crate::db_err;
use kernel::{AppResult, DomainEvent};
use sqlx::{PgPool, Postgres, Transaction};

pub async fn write_tx(tx: &mut Transaction<'_, Postgres>, e: &DomainEvent) -> AppResult<()> {
    sqlx::query("INSERT INTO platform.outbox (event_type, payload) VALUES ($1, $2)")
        .bind(e.type_name())
        .bind(serde_json::to_value(e).unwrap_or_default())
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    Ok(())
}

pub async fn write(pool: &PgPool, e: &DomainEvent) -> AppResult<()> {
    let mut tx = pool.begin().await.map_err(db_err)?;
    write_tx(&mut tx, e).await?;
    tx.commit().await.map_err(db_err)
}

pub struct Pending {
    pub id: i64,
    pub event: DomainEvent,
}

/// Claim a batch of pending events (SKIP LOCKED lets several workers share the queue).
pub async fn claim(pool: &PgPool, limit: i64) -> AppResult<Vec<Pending>> {
    let rows: Vec<(i64, serde_json::Value)> = sqlx::query_as(
        "UPDATE platform.outbox SET attempts = attempts + 1
         WHERE id IN (SELECT id FROM platform.outbox WHERE processed_at IS NULL AND attempts < 5
                      ORDER BY id LIMIT $1 FOR UPDATE SKIP LOCKED)
         RETURNING id, payload",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(db_err)?;
    let mut out: Vec<Pending> = rows
        .into_iter()
        .filter_map(|(id, v)| serde_json::from_value(v).ok().map(|event| Pending { id, event }))
        .collect();
    out.sort_by_key(|p| p.id);
    Ok(out)
}

pub async fn done(pool: &PgPool, id: i64) -> AppResult<()> {
    sqlx::query("UPDATE platform.outbox SET processed_at = now(), last_error = NULL WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(())
}

pub async fn failed(pool: &PgPool, id: i64, err: &str) -> AppResult<()> {
    sqlx::query("UPDATE platform.outbox SET last_error = $2 WHERE id = $1")
        .bind(id)
        .bind(err)
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(())
}

pub async fn lag_seconds(pool: &PgPool) -> AppResult<f64> {
    let v: Option<f64> = sqlx::query_scalar(
        "SELECT EXTRACT(EPOCH FROM now() - min(created_at))::float8 FROM platform.outbox WHERE processed_at IS NULL",
    )
    .fetch_one(pool)
    .await
    .map_err(db_err)?;
    Ok(v.unwrap_or(0.0))
}

pub async fn audit(pool: &PgPool, actor: Option<uuid::Uuid>, school: Option<uuid::Uuid>, action: &str, detail: serde_json::Value) {
    let _ = sqlx::query("INSERT INTO platform.audit_log (actor, school_id, action, detail) VALUES ($1, $2, $3, $4)")
        .bind(actor)
        .bind(school)
        .bind(action)
        .bind(detail)
        .execute(pool)
        .await;
}
