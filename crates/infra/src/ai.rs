//! AI platform anti-corruption layer. Implements the TextGenerator port with the
//! Anthropic Messages API when LLM_API_KEY is set; otherwise reports "not live"
//! and callers use their deterministic fallbacks. Every call is costed.

use async_trait::async_trait;
use kernel::AppResult;
use serde_json::json;
use sqlx::PgPool;
use std::time::Duration;
use tutoring::application::TextGenerator;

pub struct AnthropicGenerator {
    client: reqwest::Client,
    api_key: Option<String>,
    model: String,
    pool: PgPool,
}

impl AnthropicGenerator {
    pub fn from_env(pool: PgPool) -> Self {
        let api_key = std::env::var("LLM_API_KEY").ok().filter(|k| !k.trim().is_empty());
        let model = std::env::var("LLM_MODEL_FAST").unwrap_or_else(|_| "claude-haiku-5-5".to_string());
        let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().expect("http client");
        Self { client, api_key, model, pool }
    }
}

#[async_trait]
impl TextGenerator for AnthropicGenerator {
    fn is_live(&self) -> bool {
        self.api_key.is_some()
    }

    async fn reply(&self, system: &str, user: &str, purpose: &str) -> AppResult<Option<String>> {
        let Some(key) = &self.api_key else { return Ok(None) };
        let body = json!({
            "model": self.model,
            "max_tokens": 300,
            "system": system,
            "messages": [{"role": "user", "content": user}],
        });
        let resp = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await;
        let v: serde_json::Value = match resp {
            Ok(r) if r.status().is_success() => r.json().await.unwrap_or_default(),
            Ok(r) => {
                tracing::warn!(status = %r.status(), purpose, "AI provider error; using fallback");
                return Ok(None);
            }
            Err(e) => {
                tracing::warn!(error = %e, purpose, "AI provider unreachable; using fallback");
                return Ok(None);
            }
        };
        let text = v["content"][0]["text"].as_str().map(str::to_string);
        let _ = sqlx::query("INSERT INTO platform.ai_usage (purpose, model, input_tokens, output_tokens) VALUES ($1,$2,$3,$4)")
            .bind(purpose)
            .bind(&self.model)
            .bind(v["usage"]["input_tokens"].as_i64().unwrap_or(0) as i32)
            .bind(v["usage"]["output_tokens"].as_i64().unwrap_or(0) as i32)
            .execute(&self.pool)
            .await;
        Ok(text)
    }
}
