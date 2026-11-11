//! `lms`: one binary, several roles.
//!
//!   lms web      HTTP server (Axum)
//!   lms worker   outbox consumer and scheduled jobs
//!   lms all      both in one process (handy for small deployments)
//!   lms migrate  run database migrations and exit
//!   lms seed     load curriculum (and demo data when DEMO_MODE=true) and exit
//!
//! Environment: DATABASE_URL (required), PORT (default 8080), DEMO_MODE,
//! SYSADMIN_EMAIL and SYSADMIN_PASSWORD, LLM_API_KEY, LLM_MODEL_FAST, SECURE_COOKIES.

use std::sync::Arc;

fn flag(name: &str) -> bool {
    matches!(std::env::var(name).unwrap_or_default().to_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

async fn prepare(db: &infra::Db) -> anyhow::Result<()> {
    db.migrate().await?;
    let n = infra::seed::curriculum(&db.pool).await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
    tracing::info!(items = n, "curriculum loaded");
    if let (Ok(email), Ok(pw)) = (std::env::var("SYSADMIN_EMAIL"), std::env::var("SYSADMIN_PASSWORD")) {
        if pw.len() >= 12 {
            infra::seed::system_admin(&db.pool, &email.to_lowercase(), &pw).await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
        } else {
            tracing::warn!("SYSADMIN_PASSWORD must be at least 12 characters; system admin not created");
        }
    }
    if flag("DEMO_MODE") {
        let today = time::OffsetDateTime::now_utc().date();
        let made = infra::seed::demo(&db.pool, today).await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
        if made {
            tracing::info!("demo schools created");
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,sqlx=warn,tower_http=info".into()))
        .init();
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    let url = std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is not set"))?;
    let db = infra::Db::connect(&url).await?;
    let notifier: Arc<dyn notifications::application::Notifier> = Arc::new(infra::notify::LogNotifier);
    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);
    let web_state = || {
        let ai = Arc::new(infra::ai::AnthropicGenerator::from_env(db.pool.clone()));
        web::AppState::new(db.clone(), ai, notifier.clone(), flag("DEMO_MODE"), flag("SECURE_COOKIES"))
    };
    match cmd.as_str() {
        "migrate" => db.migrate().await?,
        "seed" => prepare(&db).await?,
        "web" => {
            prepare(&db).await?;
            web::serve(web_state(), port).await?;
        }
        "worker" => {
            db.migrate().await?;
            worker::Worker::new(db.clone(), notifier.clone()).run().await?;
        }
        "all" => {
            prepare(&db).await?;
            let w = worker::Worker::new(db.clone(), notifier.clone());
            tokio::spawn(async move {
                if let Err(e) = w.run().await {
                    tracing::error!(error = %e, "worker stopped");
                }
            });
            web::serve(web_state(), port).await?;
        }
        other => anyhow::bail!("unknown command {other}: use web, worker, all, migrate or seed"),
    }
    Ok(())
}
