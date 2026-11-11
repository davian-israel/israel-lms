//! Notifications bounded context: email and SMS behind a port.

pub mod application {
    use async_trait::async_trait;
    use kernel::AppResult;

    #[async_trait]
    pub trait Notifier: Send + Sync {
        async fn email(&self, to: &str, subject: &str, body: &str) -> AppResult<()>;
        async fn sms(&self, to: &str, body: &str) -> AppResult<()>;
    }
}
