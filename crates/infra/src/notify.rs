//! Notification adapter. Without a provider configured, messages are written
//! to the log (and kept visible in demo mode), so nothing is silently lost.

use async_trait::async_trait;
use kernel::AppResult;
use notifications::application::Notifier;

pub struct LogNotifier;

#[async_trait]
impl Notifier for LogNotifier {
    async fn email(&self, to: &str, subject: &str, body: &str) -> AppResult<()> {
        tracing::info!(target: "notify", channel = "email", to, subject, body, "notification");
        Ok(())
    }
    async fn sms(&self, to: &str, body: &str) -> AppResult<()> {
        tracing::info!(target: "notify", channel = "sms", to, body, "notification");
        Ok(())
    }
}
