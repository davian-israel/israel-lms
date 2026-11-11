use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

#[derive(Clone)]
pub struct Db {
    pub pool: PgPool,
}

impl Db {
    /// Connect with retries: Railway has no depends_on, so the app waits for Postgres.
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let mut last = None;
        for attempt in 1..=30 {
            match PgPoolOptions::new()
                .max_connections(10)
                .acquire_timeout(Duration::from_secs(10))
                .connect(url)
                .await
            {
                Ok(pool) => return Ok(Self { pool }),
                Err(e) => {
                    tracing::warn!(attempt, error = %e, "database not ready, retrying");
                    last = Some(e);
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
        Err(anyhow::anyhow!("could not connect to database: {:?}", last))
    }

    /// Run migrations. SQLx takes an advisory lock, so only one replica migrates.
    pub async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::migrate!("../../migrations").run(&self.pool).await?;
        Ok(())
    }
}
