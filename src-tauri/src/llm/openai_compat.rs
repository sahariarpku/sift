//! OpenAI-compatible chat completions: OpenAI, OpenRouter, Gemini's
//! compatibility endpoint, Groq, Mistral, Ollama, LM Studio and custom gateways.
//!
//! Sources (checked 2026-09-24):
//! - `POST {base}/chat/completions`, `GET {base}/models`, `Authorization: Bearer`:
//!   https://platform.openai.com/docs/api-reference/chat,
//!   https://openrouter.ai/docs/api_reference/overview,
//!   https://docs.ollama.com/api/openai-compatibility
//! - `response_format: {type: "json_schema", json_schema: {name, schema, strict}}`:
//!   OpenAI structured outputs; Mistral documents the same shape
//!   (https://docs.mistral.ai/api/endpoint/chat).
//! - Gateways may name the model that actually answered in `X-Routed-Via`
//!   (FreeLLM); OpenRouter reports it in the body's `model` field.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::http::{self, HttpReply};
use super::{
    json_instruction, parse_and_validate, rejects_param, run_test, CompletionRequest,
    CompletionResponse, LlmError, LlmProvider, LlmResult, ModelInfo, ProviderConn, Role,
    TestResult, Usage,
};

pub struct OpenAiCompat {
    conn: ProviderConn,
    client: reqwest::Client,
}

impl OpenAiCompat {
    pub fn new(conn: ProviderConn, client: reqwest::Client) -> Self {
        Self { conn, client }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.conn.base_url.trim_end_matches('/'))
    }

    fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.conn.api_key.as_deref().filter(|k| !k.is_empty()) {
            Some(key) => rb.bearer_auth(key),
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
        let mut messages = Vec::new();
        if !system.is_empty() {
            messages.push(json!({"role": "system", "content": system}));
        }
        for m in &req.messages {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            messages.push(json!({"role": role, "content": m.content}));
        }
        let mut body = json!({"model": req.model, "messages": messages, "stream": false});
        body[self.conn.options.max_tokens_param.as_str()] = json!(req.max_tokens);
        if let Some(t) = temperature {
            body["temperature"] = json!(t);
        }
        if let (Some(schema), true) = (&req.json_schema, structured) {
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": {"name": schema.name, "schema": schema.schema, "strict": true}
            });
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
        let choice = v.pointer("/choices/0").ok_or_else(|| {
            LlmError::Parse(format!(
                "no choices in response: {}",
                super::error_message(&reply.body)
            ))
        })?;
        let message = &choice["message"];
        let text = match message["content"].as_str() {
            Some(t) => t.to_owned(),
            None => match message["refusal"].as_str() {
                Some(r) => return Err(LlmError::Refusal(r.chars().take(300).collect())),
                None => String::new(),
            },
        };
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
                input_tokens: v.pointer("/usage/prompt_tokens").and_then(Value::as_u64),
                output_tokens: v
                    .pointer("/usage/completion_tokens")
                    .and_then(Value::as_u64),
            },
            temperature_applied: temperature,
            structured_output: structured && req.json_schema.is_some(),
            stop_reason: choice["finish_reason"].as_str().map(str::to_owned),
            raw_body: reply.body.clone(),
            attempts,
        })
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompat {
    async fn list_models(&self) -> LlmResult<Vec<ModelInfo>> {
        let url = self.url("models");
        let reply = http::send(&self.conn.retry, &url, || {
            self.authed(self.client.get(&url).timeout(self.conn.timeout))
        })
        .await?;
        if !reply.is_success() {
            return Err(reply.into_error());
        }
        let v: Value = serde_json::from_str(&reply.body)
            .map_err(|e| LlmError::Parse(format!("model list is not JSON: {e}")))?;
        let data = v["data"]
            .as_array()
            .or_else(|| v["models"].as_array())
            .ok_or_else(|| LlmError::Parse("model list has no `data` array".into()))?;
        let mut models: Vec<ModelInfo> = data
            .iter()
            .filter_map(|m| {
                let id = m["id"].as_str()?.to_owned();
                let display_name = m["name"].as_str().map(str::to_owned);
                Some(ModelInfo {
                    id,
                    display_name,
                    structured_output: None,
                })
            })
            .collect();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

    async fn complete(&self, req: CompletionRequest) -> LlmResult<CompletionResponse> {
        let url = self.url("chat/completions");
        let mut structured = req.json_schema.is_some() && self.conn.options.structured_output;
        let mut temperature = req.temperature;
        let mut attempts = 0;
        // At most two parameter fallbacks (structured output, temperature), each once.
        for _ in 0..3 {
            let body = self.body(&req, structured, temperature);
            let reply = http::send(&self.conn.retry, &url, || {
                self.authed(self.client.post(&url).timeout(self.conn.timeout))
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
                    &["response_format", "json_schema", "structured"],
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

    const MODELS: &str = include_str!("../../tests/fixtures/llm/freellm_models.json");
    const CHAT: &str = include_str!("../../tests/fixtures/llm/freellm_chat_auto.json");

    fn conn(base: &str, key: Option<&str>) -> ProviderConn {
        ProviderConn {
            protocol: Protocol::OpenaiCompat,
            base_url: format!("{base}/v1"),
            api_key: key.map(str::to_owned),
            options: ProviderOptions::default(),
            test_model: None,
            retry: RetryPolicy::fast(),
            timeout: Duration::from_secs(5),
        }
    }

    fn chat(content: &str, model: &str) -> String {
        json!({
            "id": "x", "object": "chat.completion", "model": model,
            "choices": [{"index": 0, "finish_reason": "stop", "message": {"role": "assistant", "content": content}}],
            "usage": {"prompt_tokens": 50, "completion_tokens": 7}
        })
        .to_string()
    }

    fn schema() -> JsonSchema {
        JsonSchema {
            name: "screen".into(),
            schema: json!({
                "type": "object",
                "properties": {"decision": {"type": "string", "enum": ["include", "exclude", "uncertain"]}},
                "required": ["decision"], "additionalProperties": false
            }),
        }
    }

    #[tokio::test]
    async fn lists_models_from_recorded_gateway_response() {
        let server = MockServer::start(vec![Reply::json(200, MODELS)]).await;
        let p = OpenAiCompat::new(
            conn(server.base(), Some("secret-key-123456")),
            reqwest::Client::new(),
        );
        let models = p.list_models().await.unwrap();
        assert_eq!(models.len(), 6);
        assert!(models.iter().any(|m| m.id == "auto"));
        let req = &server.requests()[0];
        assert_eq!(req.path, "/v1/models");
        assert_eq!(
            req.header("authorization"),
            Some("Bearer secret-key-123456")
        );
    }

    #[tokio::test]
    async fn completes_with_recorded_response_and_flags_the_routed_model() {
        let server = MockServer::start(vec![Reply::json(200, CHAT).header(
            "X-Routed-Via",
            "openrouter/dots-studio/dots-3-note-preview:free",
        )])
        .await;
        let p = OpenAiCompat::new(conn(server.base(), Some("k")), reqwest::Client::new());
        let resp = p
            .complete(CompletionRequest::new(
                "auto",
                vec![ChatMessage::user("Reply OK")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.text.trim(), "OK");
        assert_eq!(
            resp.usage,
            Usage {
                input_tokens: Some(20),
                output_tokens: Some(26)
            }
        );
        assert_eq!(
            resp.reported_model.as_deref(),
            Some("dots-studio/dots-3-note-preview:free")
        );
        assert_eq!(
            resp.routed_via.as_deref(),
            Some("openrouter/dots-studio/dots-3-note-preview:free")
        );
        assert_eq!(resp.model_match(), ModelMatch::Different);
        assert_eq!(resp.temperature_applied, Some(0.0));
        let body = server.requests()[0].json();
        assert_eq!(body["temperature"], 0.0);
        assert_eq!(body["max_tokens"], 1024);
    }

    #[tokio::test]
    async fn keyless_local_server_gets_no_auth_header() {
        let server = MockServer::start(vec![Reply::json(200, chat("OK", "llama3.2:latest"))]).await;
        let p = OpenAiCompat::new(conn(server.base(), None), reqwest::Client::new());
        let resp = p
            .complete(CompletionRequest::new(
                "llama3.2",
                vec![ChatMessage::user("hi")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.model_match(), ModelMatch::Snapshot);
        assert!(server.requests()[0].header("authorization").is_none());
    }

    #[tokio::test]
    async fn structured_output_is_requested_and_validated() {
        let server = MockServer::start(vec![Reply::json(
            200,
            chat(r#"{"decision":"exclude"}"#, "gpt-x"),
        )])
        .await;
        let p = OpenAiCompat::new(conn(server.base(), Some("k")), reqwest::Client::new());
        let mut req = CompletionRequest::new("gpt-x", vec![ChatMessage::user("screen")]);
        req.json_schema = Some(schema());
        let resp = p.complete(req).await.unwrap();
        assert_eq!(resp.json.unwrap()["decision"], "exclude");
        assert!(resp.structured_output);
        let body = server.requests()[0].json();
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    }

    #[tokio::test]
    async fn falls_back_to_prompt_json_when_response_format_is_rejected() {
        let server = MockServer::start(vec![
            Reply::json(400, r#"{"error":{"message":"response_format json_schema is not supported by this model"}}"#),
            Reply::json(200, chat(r#"{"decision":"include"}"#, "m")),
        ])
        .await;
        let p = OpenAiCompat::new(conn(server.base(), None), reqwest::Client::new());
        let mut req = CompletionRequest::new("m", vec![ChatMessage::user("screen")]);
        req.json_schema = Some(schema());
        let resp = p.complete(req).await.unwrap();
        assert!(!resp.structured_output);
        assert_eq!(resp.attempts, 2);
        let second = server.requests()[1].json();
        assert!(second.get("response_format").is_none());
        assert!(second["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("JSON Schema"));
    }

    #[tokio::test]
    async fn drops_temperature_when_the_model_rejects_it_and_records_that() {
        let server = MockServer::start(vec![
            Reply::json(400, r#"{"error":{"message":"Unsupported value: 'temperature' does not support 0 with this model."}}"#),
            Reply::json(200, chat("OK", "o-model")),
        ])
        .await;
        let p = OpenAiCompat::new(conn(server.base(), Some("k")), reqwest::Client::new());
        let resp = p
            .complete(CompletionRequest::new(
                "o-model",
                vec![ChatMessage::user("hi")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.temperature_applied, None);
        assert!(server.requests()[1].json().get("temperature").is_none());
    }

    #[tokio::test]
    async fn invalid_json_is_reported_not_hidden() {
        let server = MockServer::start(vec![Reply::json(200, chat("I think include.", "m"))]).await;
        let p = OpenAiCompat::new(conn(server.base(), None), reqwest::Client::new());
        let mut req = CompletionRequest::new("m", vec![ChatMessage::user("screen")]);
        req.json_schema = Some(schema());
        let resp = p.complete(req).await.unwrap();
        assert!(resp.json.is_none());
        assert!(resp.json_error.unwrap().contains("not JSON"));
    }

    #[tokio::test]
    async fn max_completion_tokens_option_is_respected() {
        let server = MockServer::start(vec![Reply::json(200, chat("OK", "gpt-5"))]).await;
        let mut c = conn(server.base(), Some("k"));
        c.options.max_tokens_param = "max_completion_tokens".into();
        let p = OpenAiCompat::new(c, reqwest::Client::new());
        p.complete(CompletionRequest::new(
            "gpt-5",
            vec![ChatMessage::user("hi")],
        ))
        .await
        .unwrap();
        let body = server.requests()[0].json();
        assert_eq!(body["max_completion_tokens"], 1024);
        assert!(body.get("max_tokens").is_none());
    }

    #[tokio::test]
    async fn auth_failure_maps_to_auth_error() {
        let server = MockServer::start(vec![Reply::json(
            401,
            r#"{"error":{"message":"Invalid API key"}}"#,
        )])
        .await;
        let p = OpenAiCompat::new(conn(server.base(), Some("bad")), reqwest::Client::new());
        let err = p.list_models().await.unwrap_err();
        assert_eq!(
            err,
            LlmError::Auth {
                status: 401,
                message: "Invalid API key".into()
            }
        );
    }

    #[tokio::test]
    async fn test_uses_first_listed_model_when_none_is_set() {
        let server = MockServer::routes(vec![
            (
                Some("GET"),
                "/v1/models",
                vec![Reply::json(
                    200,
                    include_str!("../../tests/fixtures/llm/ollama_models.json"),
                )],
            ),
            (
                Some("POST"),
                "/v1/chat/completions",
                vec![Reply::json(200, chat("OK", "llama3.2:latest"))],
            ),
        ])
        .await;
        let p = OpenAiCompat::new(conn(server.base(), None), reqwest::Client::new());
        let result = p.test().await.unwrap();
        assert!(result.ok);
        assert_eq!(result.model.as_deref(), Some("llama3.2:latest"));
        let body = server.requests()[1].json();
        assert_eq!(body["max_tokens"], 16);
        assert!(body.get("temperature").is_none());
    }
}
