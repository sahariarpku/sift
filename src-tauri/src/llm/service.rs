//! Task-level calls used by the agents (Phase 2 onward).
//!
//! - A task (Screener A, Arbiter, ...) maps to one provider + model; there is
//!   no fallback model (strict mode, §6.4). A failure after retries is returned
//!   so the job pauses and the user sees the error.
//! - Per-provider concurrency limit (default 4).
//! - Schema-bound calls get one retry on invalid JSON; a second invalid
//!   response sets `needs_human` so the item goes to the human queue.
//! - Every attempt's response is returned so each can be logged with its cost.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use tokio::sync::Semaphore;

use super::cost::{self, Price};
use super::store::{self, Task};
use super::{
    build_provider, CompletionRequest, CompletionResponse, LlmError, LlmResult, ModelMatch,
    ProviderConn, RetryPolicy,
};
use crate::secrets::SecretStore;

/// Where a task's calls go, resolved from `app.db` and the keychain.
#[derive(Debug, Clone)]
pub struct Route {
    pub task: Task,
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub concurrency: u32,
    pub price: Option<Price>,
    pub conn: ProviderConn,
}

#[derive(Debug, Clone)]
pub struct Outcome {
    /// Every response received, in order. The last one is the result.
    pub responses: Vec<CompletionResponse>,
    /// Validated JSON from the last response, when a schema was requested.
    pub json: Option<serde_json::Value>,
    /// Two invalid JSON responses in a row: hand the item to a person (§6.4).
    pub needs_human: bool,
    /// Summed estimate over all responses; `None` if any could not be priced.
    pub cost_usd: Option<f64>,
    /// Set when a response came from a different model than requested.
    pub model_warning: Option<String>,
}

pub struct LlmService {
    client: reqwest::Client,
    limits: Mutex<HashMap<String, Arc<Semaphore>>>,
    retry: RetryPolicy,
}

impl LlmService {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            limits: Mutex::new(HashMap::new()),
            retry: RetryPolicy::default(),
        }
    }

    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Resolves the provider, model, key and price for a task.
    pub fn route(
        &self,
        app: &Connection,
        secrets: &dyn SecretStore,
        task: Task,
    ) -> crate::error::Result<Route> {
        let (provider_id, model_id) = store::assignment(app, task)?.ok_or_else(|| {
            crate::error::Error::Invalid(format!(
                "No model is set for {}. Choose one in Settings.",
                task.label()
            ))
        })?;
        let rec = store::get(app, &provider_id)?.ok_or_else(|| {
            crate::error::Error::Invalid(format!("The provider for {} was removed.", task.label()))
        })?;
        let bridge = crate::mcp::is_bridge_url(&rec.base_url);
        let api_key = match &rec.api_key_ref {
            Some(r) => secrets.get(r)?,
            // The app's own MCP bridge takes the local token from app.db.
            None if bridge => Some(crate::mcp::token(app)?),
            None => None,
        };
        let price = store::price(app, &provider_id, &model_id)?;
        Ok(Route {
            task,
            provider_id,
            provider_name: rec.display_name,
            concurrency: rec.concurrency,
            price,
            conn: ProviderConn {
                protocol: rec.protocol,
                base_url: rec.base_url,
                api_key,
                options: rec.options,
                test_model: Some(model_id.clone()),
                retry: self.retry,
                // A bridge call waits for an assistant; the bridge gives up
                // first (`mcp::http::BRIDGE_WAIT`) with a clear message.
                timeout: if bridge {
                    crate::mcp::http::BRIDGE_WAIT + std::time::Duration::from_secs(60)
                } else {
                    std::time::Duration::from_secs(300)
                },
            },
            model_id,
        })
    }

    fn limit(&self, route: &Route) -> Arc<Semaphore> {
        let mut limits = self.limits.lock().unwrap_or_else(|p| p.into_inner());
        limits
            .entry(route.provider_id.clone())
            .or_insert_with(|| Arc::new(Semaphore::new(route.concurrency.max(1) as usize)))
            .clone()
    }

    /// Sends `req` (its `model` is replaced by the route's model).
    pub async fn complete(&self, route: &Route, mut req: CompletionRequest) -> LlmResult<Outcome> {
        req.model = route.model_id.clone();
        let permit = self
            .limit(route)
            .acquire_owned()
            .await
            .map_err(|_| LlmError::Config("provider limiter closed".into()))?;
        let provider = build_provider(route.conn.clone(), self.client.clone());

        let mut responses = Vec::new();
        let tries = if req.json_schema.is_some() { 2 } else { 1 };
        for _ in 0..tries {
            let resp = provider.complete(req.clone()).await?;
            let ok = req.json_schema.is_none() || resp.json.is_some();
            responses.push(resp);
            if ok {
                break;
            }
        }
        drop(permit);

        let last = responses
            .last()
            .ok_or_else(|| LlmError::Parse("no response".into()))?;
        let json = last.json.clone();
        let needs_human = req.json_schema.is_some() && json.is_none();
        let cost_usd = responses
            .iter()
            .map(|r| cost::estimate(&r.usage, route.price))
            .try_fold(0.0, |acc, c| c.map(|c| acc + c));
        let model_warning = responses
            .iter()
            .find(|r| r.model_match() == ModelMatch::Different)
            .map(|r| {
                format!(
                    "{} requested {} but the response came from {}.",
                    route.task.label(),
                    route.model_id,
                    r.effective_model().unwrap_or("an unnamed model")
                )
            });
        if let Some(w) = &model_warning {
            eprintln!("model mismatch: {w}");
        }
        Ok(Outcome {
            responses,
            json,
            needs_human,
            cost_usd,
            model_warning,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::llm::store::{upsert, ProviderRecord};
    use crate::llm::{ChatMessage, JsonSchema, Protocol, ProviderOptions};
    use crate::secrets::MemoryStore;
    use crate::test_http::{MockServer, Reply};
    use serde_json::json;

    fn chat(content: &str, model: &str) -> Reply {
        Reply::json(
            200,
            json!({
                "model": model,
                "choices": [{"finish_reason": "stop", "message": {"role": "assistant", "content": content}}],
                "usage": {"prompt_tokens": 1000, "completion_tokens": 100}
            })
            .to_string(),
        )
    }

    fn setup(base: &str) -> (tempfile::TempDir, Connection, MemoryStore) {
        let dir = tempfile::tempdir().unwrap();
        let app = db::open_app(&dir.path().join("app.db")).unwrap();
        upsert(
            &app,
            &ProviderRecord {
                id: "p1".into(),
                preset: "custom".into(),
                display_name: "Gateway".into(),
                protocol: Protocol::OpenaiCompat,
                base_url: format!("{base}/v1"),
                api_key_ref: Some("provider:p1".into()),
                models: vec![],
                data_policy_warning: false,
                concurrency: 2,
                options: ProviderOptions::default(),
                last_test_status: None,
                last_test_message: None,
                last_test_at: None,
            },
        )
        .unwrap();
        store::set_assignment(&app, Task::ScreenerA, Some(("p1", "model-a"))).unwrap();
        store::set_price(
            &app,
            "p1",
            "model-a",
            Some(Price {
                input_per_mtok: 3.0,
                output_per_mtok: 15.0,
            }),
        )
        .unwrap();
        let secrets = MemoryStore::default();
        secrets.set("provider:p1", "gateway-key-000000").unwrap();
        (dir, app, secrets)
    }

    fn schema_request() -> CompletionRequest {
        let mut req = CompletionRequest::new("ignored", vec![ChatMessage::user("screen")]);
        req.json_schema = Some(JsonSchema {
            name: "s".into(),
            schema: json!({"type": "object", "properties": {"decision": {"type": "string"}}, "required": ["decision"]}),
        });
        req
    }

    #[tokio::test]
    async fn routes_task_to_its_model_with_key_and_cost() {
        let server = MockServer::start(vec![chat(r#"{"decision":"include"}"#, "model-a")]).await;
        let (_d, app, secrets) = setup(server.base());
        let svc = LlmService::new(reqwest::Client::new()).with_retry(RetryPolicy::fast());
        let route = svc.route(&app, &secrets, Task::ScreenerA).unwrap();
        let out = svc.complete(&route, schema_request()).await.unwrap();
        assert_eq!(out.json.unwrap()["decision"], "include");
        assert!(!out.needs_human);
        assert!((out.cost_usd.unwrap() - 0.0045).abs() < 1e-12);
        assert!(out.model_warning.is_none());
        let r = &server.requests()[0];
        assert_eq!(r.json()["model"], "model-a");
        assert_eq!(r.header("authorization"), Some("Bearer gateway-key-000000"));
    }

    #[tokio::test]
    async fn one_retry_on_invalid_json_then_human_queue() {
        let server = MockServer::start(vec![
            chat("include, probably", "model-a"),
            chat("still prose", "model-a"),
        ])
        .await;
        let (_d, app, secrets) = setup(server.base());
        let svc = LlmService::new(reqwest::Client::new()).with_retry(RetryPolicy::fast());
        let route = svc.route(&app, &secrets, Task::ScreenerA).unwrap();
        let out = svc.complete(&route, schema_request()).await.unwrap();
        assert!(out.needs_human);
        assert_eq!(out.responses.len(), 2);
        assert!(
            (out.cost_usd.unwrap() - 0.009).abs() < 1e-12,
            "both attempts are costed"
        );
    }

    #[tokio::test]
    async fn recovers_when_second_attempt_is_valid() {
        let server = MockServer::start(vec![
            chat("oops", "model-a"),
            chat(r#"{"decision":"exclude"}"#, "model-a"),
        ])
        .await;
        let (_d, app, secrets) = setup(server.base());
        let svc = LlmService::new(reqwest::Client::new()).with_retry(RetryPolicy::fast());
        let route = svc.route(&app, &secrets, Task::ScreenerA).unwrap();
        let out = svc.complete(&route, schema_request()).await.unwrap();
        assert!(!out.needs_human);
        assert_eq!(out.json.unwrap()["decision"], "exclude");
    }

    #[tokio::test]
    async fn warns_when_another_model_answers() {
        let server = MockServer::start(vec![chat("OK", "some-other-model")]).await;
        let (_d, app, secrets) = setup(server.base());
        let svc = LlmService::new(reqwest::Client::new()).with_retry(RetryPolicy::fast());
        let route = svc.route(&app, &secrets, Task::ScreenerA).unwrap();
        let out = svc
            .complete(
                &route,
                CompletionRequest::new("x", vec![ChatMessage::user("hi")]),
            )
            .await
            .unwrap();
        assert!(out.model_warning.unwrap().contains("some-other-model"));
    }

    #[tokio::test]
    async fn failure_after_retries_is_returned_not_substituted() {
        let server = MockServer::start(vec![Reply::json(
            503,
            r#"{"error":{"message":"upstream down"}}"#,
        )])
        .await;
        let (_d, app, secrets) = setup(server.base());
        let svc = LlmService::new(reqwest::Client::new()).with_retry(RetryPolicy::fast());
        let route = svc.route(&app, &secrets, Task::ScreenerA).unwrap();
        let err = svc
            .complete(
                &route,
                CompletionRequest::new("x", vec![ChatMessage::user("hi")]),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Http { status: 503, .. }));
        assert!(server
            .requests()
            .iter()
            .all(|r| r.json()["model"] == "model-a"));
    }

    #[test]
    fn unassigned_task_has_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let app = db::open_app(&dir.path().join("app.db")).unwrap();
        let svc = LlmService::new(reqwest::Client::new());
        let err = svc
            .route(&app, &MemoryStore::default(), Task::Arbiter)
            .unwrap_err();
        assert!(err.to_string().contains("No model is set for Arbiter"));
    }

    #[tokio::test]
    async fn concurrency_is_limited_per_provider() {
        let server = MockServer::start(vec![chat("OK", "model-a")]).await;
        let (_d, app, secrets) = setup(server.base());
        let svc = Arc::new(LlmService::new(reqwest::Client::new()).with_retry(RetryPolicy::fast()));
        let route = svc.route(&app, &secrets, Task::ScreenerA).unwrap();
        assert_eq!(svc.limit(&route).available_permits(), 2);
        let handles: Vec<_> = (0..6)
            .map(|_| {
                let (svc, route) = (svc.clone(), route.clone());
                tokio::spawn(async move {
                    svc.complete(
                        &route,
                        CompletionRequest::new("x", vec![ChatMessage::user("hi")]),
                    )
                    .await
                })
            })
            .collect();
        for h in handles {
            h.await.unwrap().unwrap();
        }
        assert_eq!(
            svc.limit(&route).available_permits(),
            2,
            "all permits returned"
        );
        assert_eq!(server.requests().len(), 6);
    }
}
