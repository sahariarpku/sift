//! Anthropic Messages API.
//!
//! Sources (checked 2026-09-24, Claude API reference and the claude-api skill):
//! - `POST {base}/v1/messages` with headers `x-api-key`, `anthropic-version: 2023-06-01`.
//!   Response: `content[]` blocks (`type: "text"`), `model`, `stop_reason`,
//!   `usage.input_tokens` / `usage.output_tokens`.
//! - JSON output: `output_config: {format: {type: "json_schema", schema}}`; the
//!   first text block then holds the JSON. Forced `tool_choice` is rejected by
//!   some current models, so it is not used.
//! - `temperature` is rejected (400) by several current models; it is dropped
//!   on that error and the response records it as not applied.
//! - `GET {base}/v1/models?limit=&after_id=` -> `data[]`, `has_more`, `last_id`;
//!   each model carries `capabilities.structured_outputs.supported`.
//! - Errors: `{"type":"error","error":{"type","message"}}`; 429, 500 and 529
//!   (overloaded) are retryable, with `retry-after` in seconds.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::http::{self, HttpReply};
use super::{
    json_instruction, parse_and_validate, rejects_param, run_test, CompletionRequest,
    CompletionResponse, LlmError, LlmProvider, LlmResult, ModelInfo, ProviderConn, Role,
    TestResult, Usage,
};

pub const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct Anthropic {
    conn: ProviderConn,
    client: reqwest::Client,
}

impl Anthropic {
    pub fn new(conn: ProviderConn, client: reqwest::Client) -> Self {
        Self { conn, client }
    }

    fn url(&self, path: &str) -> String {
        let base = self.conn.base_url.trim_end_matches('/');
        let base = base.strip_suffix("/v1").unwrap_or(base);
        format!("{base}/v1/{path}")
    }

    fn headers(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let rb = rb.header("anthropic-version", ANTHROPIC_VERSION);
        match self.conn.api_key.as_deref().filter(|k| !k.is_empty()) {
            Some(key) => rb.header("x-api-key", key),
            None => rb,
        }
    }

    fn body(&self, req: &CompletionRequest, structured: bool, temperature: Option<f64>) -> Value {
        let mut system = req.system.clone().unwrap_or_default();
        if let (Some(schema), false) = (&req.json_schema, structured) {
            if !system.is_empty() {
                system.push_str("\n\n");
            }
            system.push_str(&json_instruction(schema));
        }
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                };
                json!({"role": role, "content": m.content})
            })
            .collect();
        let mut body =
            json!({"model": req.model, "max_tokens": req.max_tokens, "messages": messages});
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        if let Some(t) = temperature {
            body["temperature"] = json!(t);
        }
        if let (Some(schema), true) = (&req.json_schema, structured) {
            body["output_config"] =
                json!({"format": {"type": "json_schema", "schema": schema.schema}});
        }
        body
    }

    fn parse(
        &self,
        req: &CompletionRequest,
        reply: &HttpReply,
        structured: bool,
        temperature: Option<f64>,
        attempts: u32,
    ) -> LlmResult<CompletionResponse> {
        let v: Value = serde_json::from_str(&reply.body)
            .map_err(|e| LlmError::Parse(format!("response is not JSON: {e}")))?;
        let stop_reason = v["stop_reason"].as_str().map(str::to_owned);
        if stop_reason.as_deref() == Some("refusal") {
            let why = v
                .pointer("/stop_details/category")
                .and_then(Value::as_str)
                .unwrap_or("no category given");
            return Err(LlmError::Refusal(why.to_owned()));
        }
        let blocks = v["content"]
            .as_array()
            .ok_or_else(|| LlmError::Parse("response has no content array".into()))?;
        let text: String = blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("");
        let (json, json_error) = match &req.json_schema {
            Some(schema) => match parse_and_validate(&text, schema) {
                Ok(j) => (Some(j), None),
                Err(e) => (None, Some(e.to_string())),
            },
            None => (None, None),
        };
        Ok(CompletionResponse {
            text,
            json,
            json_error,
            requested_model: req.model.clone(),
            reported_model: v["model"].as_str().map(str::to_owned),
            routed_via: reply.header("x-routed-via"),
            usage: Usage {
                input_tokens: v.pointer("/usage/input_tokens").and_then(Value::as_u64),
                output_tokens: v.pointer("/usage/output_tokens").and_then(Value::as_u64),
            },
            temperature_applied: temperature,
            structured_output: structured && req.json_schema.is_some(),
            stop_reason,
            raw_body: reply.body.clone(),
            attempts,
        })
    }
}

#[async_trait]
impl LlmProvider for Anthropic {
    async fn list_models(&self) -> LlmResult<Vec<ModelInfo>> {
        let mut models = Vec::new();
        let mut after: Option<String> = None;
        // The API pages with after_id; 20 pages of 1000 is far beyond any real list.
        for _ in 0..20 {
            let mut url = self.url("models?limit=1000");
            if let Some(a) = &after {
                url.push_str(&format!("&after_id={a}"));
            }
            let reply = http::send(&self.conn.retry, &url, || {
                self.headers(self.client.get(&url).timeout(self.conn.timeout))
            })
            .await?;
            if !reply.is_success() {
                return Err(reply.into_error());
            }
            let v: Value = serde_json::from_str(&reply.body)
                .map_err(|e| LlmError::Parse(format!("model list is not JSON: {e}")))?;
            let data = v["data"]
                .as_array()
                .ok_or_else(|| LlmError::Parse("model list has no data".into()))?;
            models.extend(data.iter().filter_map(|m| {
                Some(ModelInfo {
                    id: m["id"].as_str()?.to_owned(),
                    display_name: m["display_name"].as_str().map(str::to_owned),
                    structured_output: m
                        .pointer("/capabilities/structured_outputs/supported")
                        .and_then(Value::as_bool),
                })
            }));
            match (v["has_more"].as_bool(), v["last_id"].as_str()) {
                (Some(true), Some(last)) => after = Some(last.to_owned()),
                _ => break,
            }
        }
        Ok(models)
    }

    async fn complete(&self, req: CompletionRequest) -> LlmResult<CompletionResponse> {
        let url = self.url("messages");
        let mut structured = req.json_schema.is_some() && self.conn.options.structured_output;
        let mut temperature = req.temperature;
        let mut attempts = 0;
        for _ in 0..3 {
            let body = self.body(&req, structured, temperature);
            let reply = http::send(&self.conn.retry, &url, || {
                self.headers(self.client.post(&url).timeout(self.conn.timeout))
                    .json(&body)
            })
            .await?;
            attempts += reply.attempts;
            if reply.is_success() {
                return self.parse(&req, &reply, structured, temperature, attempts);
            }
            if structured
                && rejects_param(
                    reply.status,
                    &reply.body,
                    &["output_config", "json_schema", "structured output"],
                )
            {
                structured = false;
                continue;
            }
            if temperature.is_some() && rejects_param(reply.status, &reply.body, &["temperature"]) {
                temperature = None;
                continue;
            }
            return Err(match reply.into_error() {
                LlmError::Http {
                    status,
                    message,
                    retryable,
                    ..
                } => LlmError::Http {
                    status,
                    message,
                    retryable,
                    attempts,
                },
                other => other,
            });
        }
        Err(LlmError::Parse(
            "the server kept rejecting the request parameters".into(),
        ))
    }

    async fn test(&self) -> LlmResult<TestResult> {
        run_test(self, self.conn.test_model.clone()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, JsonSchema, ModelMatch, Protocol, ProviderOptions, RetryPolicy};
    use crate::test_http::{MockServer, Reply};
    use std::time::Duration;

    const MESSAGE: &str = include_str!("../../tests/fixtures/llm/anthropic_messages_json.json");
    const MODELS: &str = include_str!("../../tests/fixtures/llm/anthropic_models.json");

    fn conn(base: &str) -> ProviderConn {
        ProviderConn {
            protocol: Protocol::Anthropic,
            base_url: base.to_owned(),
            api_key: Some("sk-ant-test-0000000000".into()),
            options: ProviderOptions::default(),
            test_model: Some("claude-sonnet-5".into()),
            retry: RetryPolicy::fast(),
            timeout: Duration::from_secs(5),
        }
    }

    fn schema() -> JsonSchema {
        JsonSchema {
            name: "screen".into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "decision": {"type": "string", "enum": ["include", "exclude", "uncertain"]},
                    "confidence": {"type": "number"}
                },
                "required": ["decision", "confidence"], "additionalProperties": false
            }),
        }
    }

    #[tokio::test]
    async fn sends_documented_headers_and_structured_output() {
        let server = MockServer::start(vec![Reply::json(200, MESSAGE)]).await;
        let p = Anthropic::new(conn(server.base()), reqwest::Client::new());
        let mut req =
            CompletionRequest::new("claude-sonnet-5", vec![ChatMessage::user("screen this")]);
        req.system = Some("You screen records.".into());
        req.json_schema = Some(schema());
        let resp = p.complete(req).await.unwrap();
        assert_eq!(resp.json.as_ref().unwrap()["decision"], "include");
        assert_eq!(
            resp.usage,
            Usage {
                input_tokens: Some(212),
                output_tokens: Some(19)
            }
        );
        assert_eq!(resp.model_match(), ModelMatch::Exact);

        let r = &server.requests()[0];
        assert_eq!(
            (r.method.as_str(), r.path.as_str()),
            ("POST", "/v1/messages")
        );
        assert_eq!(r.header("x-api-key"), Some("sk-ant-test-0000000000"));
        assert_eq!(r.header("anthropic-version"), Some("2023-06-01"));
        assert!(r.header("authorization").is_none());
        let body = r.json();
        assert_eq!(body["system"], "You screen records.");
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(body["temperature"], 0.0);
    }

    #[tokio::test]
    async fn base_url_with_or_without_v1_works() {
        let server = MockServer::start(vec![Reply::json(200, MODELS)]).await;
        let p = Anthropic::new(
            conn(&format!("{}/v1/", server.base())),
            reqwest::Client::new(),
        );
        p.list_models().await.unwrap();
        assert!(server.requests()[0].path.starts_with("/v1/models"));
    }

    #[tokio::test]
    async fn drops_temperature_for_models_that_reject_it() {
        let server = MockServer::start(vec![
            Reply::json(400, r#"{"type":"error","error":{"type":"invalid_request_error","message":"`temperature` is not supported for this model"}}"#),
            Reply::json(200, MESSAGE),
        ])
        .await;
        let p = Anthropic::new(conn(server.base()), reqwest::Client::new());
        let resp = p
            .complete(CompletionRequest::new(
                "claude-sonnet-5",
                vec![ChatMessage::user("hi")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.temperature_applied, None);
        assert_eq!(resp.attempts, 2);
    }

    #[tokio::test]
    async fn overloaded_529_is_retried() {
        let server = MockServer::start(vec![
            Reply::json(
                529,
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            ),
            Reply::json(200, MESSAGE),
        ])
        .await;
        let p = Anthropic::new(conn(server.base()), reqwest::Client::new());
        let resp = p
            .complete(CompletionRequest::new(
                "claude-sonnet-5",
                vec![ChatMessage::user("hi")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.attempts, 2);
    }

    #[tokio::test]
    async fn refusal_is_an_error_not_empty_text() {
        let body = json!({
            "model": "claude-sonnet-5", "content": [], "stop_reason": "refusal",
            "stop_details": {"type": "refusal", "category": "cyber"},
            "usage": {"input_tokens": 5, "output_tokens": 0}
        });
        let server = MockServer::start(vec![Reply::json(200, body.to_string())]).await;
        let p = Anthropic::new(conn(server.base()), reqwest::Client::new());
        let err = p
            .complete(CompletionRequest::new(
                "claude-sonnet-5",
                vec![ChatMessage::user("hi")],
            ))
            .await
            .unwrap_err();
        assert_eq!(err, LlmError::Refusal("cyber".into()));
    }

    #[tokio::test]
    async fn lists_models_across_pages_with_capabilities() {
        let page1 = json!({
            "data": [{"id": "claude-a", "display_name": "A", "capabilities": {"structured_outputs": {"supported": false}}}],
            "has_more": true, "last_id": "claude-a"
        });
        let server = MockServer::start(vec![
            Reply::json(200, page1.to_string()),
            Reply::json(200, MODELS),
        ])
        .await;
        let p = Anthropic::new(conn(server.base()), reqwest::Client::new());
        let models = p.list_models().await.unwrap();
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["claude-a", "claude-sonnet-5", "claude-haiku-4-5"]
        );
        assert_eq!(models[0].structured_output, Some(false));
        assert!(server.requests()[1].path.contains("after_id=claude-a"));
    }

    #[tokio::test]
    async fn bad_key_is_an_auth_error() {
        let server = MockServer::start(vec![Reply::json(401, r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#)]).await;
        let p = Anthropic::new(conn(server.base()), reqwest::Client::new());
        let err = p.test().await.unwrap_err();
        assert_eq!(
            err,
            LlmError::Auth {
                status: 401,
                message: "invalid x-api-key".into()
            }
        );
    }

    #[test]
    fn debug_output_never_contains_the_key() {
        let c = conn("https://api.anthropic.com");
        assert!(!format!("{c:?}").contains("sk-ant-test"));
    }
}
