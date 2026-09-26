//! AI provider layer (CLAUDE.md §6).
//!
//! Two wire protocols cover every preset: OpenAI-compatible chat completions
//! and Anthropic's native Messages API. Both implement [`LlmProvider`]. Keys
//! are passed in by the caller after reading the keychain; nothing here logs
//! or returns a key.

pub mod anthropic;
pub mod cost;
pub mod http;
pub mod openai_compat;
pub mod presets;
pub mod service;
pub mod store;

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub use http::RetryPolicy;

/// Prompt used by `test()`. Versioned like every other prompt (§13).
pub const CONNECTION_TEST_PROMPT: &str = include_str!("../../../prompts/connection_test.v1.md");
pub const CONNECTION_TEST_PROMPT_VERSION: &str = "connection_test.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    OpenaiCompat,
    Anthropic,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::OpenaiCompat => "openai_compat",
            Protocol::Anthropic => "anthropic",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "openai_compat" => Some(Protocol::OpenaiCompat),
            "anthropic" => Some(Protocol::Anthropic),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    /// `Some(false)` when the server says the model cannot do structured output.
    #[serde(default)]
    pub structured_output: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }
}

/// A JSON Schema the response must satisfy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonSchema {
    /// Short identifier, `[a-zA-Z0-9_-]` (OpenAI requires one).
    pub name: String,
    pub schema: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub json_schema: Option<JsonSchema>,
    /// Defaults to 0 (§6.1). Dropped, and recorded as not applied, when a model rejects it.
    pub temperature: Option<f64>,
    pub max_tokens: u32,
}

impl CompletionRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            system: None,
            messages,
            json_schema: None,
            temperature: Some(0.0),
            max_tokens: 1024,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionResponse {
    pub text: String,
    /// Parsed and schema-validated JSON when the request carried a schema.
    pub json: Option<serde_json::Value>,
    /// Why the text failed JSON parsing or schema validation, when it did.
    pub json_error: Option<String>,
    pub requested_model: String,
    /// The model ID the response body reports.
    pub reported_model: Option<String>,
    /// Gateway routing header (e.g. FreeLLM's `X-Routed-Via`), when present.
    pub routed_via: Option<String>,
    pub usage: Usage,
    /// The temperature actually sent; `None` if the model rejected it and it was dropped.
    pub temperature_applied: Option<f64>,
    /// Whether the server enforced the schema (true) or the prompt asked for JSON (false).
    pub structured_output: bool,
    pub stop_reason: Option<String>,
    pub raw_body: String,
    /// HTTP attempts, including retries and parameter fallbacks.
    pub attempts: u32,
}

impl CompletionResponse {
    /// Which model actually answered, preferring the gateway's routing header.
    pub fn effective_model(&self) -> Option<&str> {
        self.routed_via
            .as_deref()
            .or(self.reported_model.as_deref())
    }

    pub fn model_match(&self) -> ModelMatch {
        match self.effective_model() {
            Some(reported) => model_match(&self.requested_model, reported),
            None => ModelMatch::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub ok: bool,
    pub model: Option<String>,
    pub reported_model: Option<String>,
    pub model_match: ModelMatch,
    pub latency_ms: u64,
    pub message: String,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum LlmError {
    #[error("could not reach {url}: {message}")]
    Unreachable { url: String, message: String },
    #[error("the provider rejected the key (HTTP {status}): {message}")]
    Auth { status: u16, message: String },
    #[error("model not found (HTTP {status}): {message}")]
    ModelNotFound { status: u16, message: String },
    #[error("HTTP {status} after {attempts} attempt(s): {message}")]
    Http {
        status: u16,
        message: String,
        retryable: bool,
        attempts: u32,
    },
    #[error("unexpected response from the provider: {0}")]
    Parse(String),
    #[error("the model declined to answer ({0})")]
    Refusal(String),
    #[error("the response was not valid JSON for the schema: {reason}")]
    InvalidJson { reason: String, raw: String },
    #[error("{0}")]
    Config(String),
}

impl LlmError {
    /// Whether a later attempt could succeed without any change on our side.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            LlmError::Unreachable { .. }
                | LlmError::Http {
                    retryable: true,
                    ..
                }
        )
    }
}

pub type LlmResult<T> = std::result::Result<T, LlmError>;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn list_models(&self) -> LlmResult<Vec<ModelInfo>>;
    async fn complete(&self, req: CompletionRequest) -> LlmResult<CompletionResponse>;
    /// One tiny request against the configured test model (or the first listed model).
    async fn test(&self) -> LlmResult<TestResult>;
}

/// Everything needed to talk to one configured provider.
#[derive(Clone)]
pub struct ProviderConn {
    pub protocol: Protocol,
    pub base_url: String,
    pub api_key: Option<String>,
    pub options: ProviderOptions,
    /// Model used by `test()`. Falls back to the first model the server lists.
    pub test_model: Option<String>,
    pub retry: RetryPolicy,
    pub timeout: Duration,
}

// Hand-written so the key can never reach a log line through `{:?}`.
impl std::fmt::Debug for ProviderConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConn")
            .field("protocol", &self.protocol)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("options", &self.options)
            .field("test_model", &self.test_model)
            .finish()
    }
}

/// Per-provider switches, stored as JSON in `provider.options`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProviderOptions {
    /// Ask the server to enforce the JSON schema (OpenAI `response_format`,
    /// Anthropic `output_config.format`). Falls back to prompt-only JSON on rejection.
    pub structured_output: bool,
    /// `max_tokens` or `max_completion_tokens` (OpenAI's newer models require the latter).
    pub max_tokens_param: String,
}

impl Default for ProviderOptions {
    fn default() -> Self {
        Self {
            structured_output: true,
            max_tokens_param: "max_tokens".into(),
        }
    }
}

pub fn build_provider(conn: ProviderConn, client: reqwest::Client) -> Box<dyn LlmProvider> {
    match conn.protocol {
        Protocol::OpenaiCompat => Box::new(openai_compat::OpenAiCompat::new(conn, client)),
        Protocol::Anthropic => Box::new(anthropic::Anthropic::new(conn, client)),
    }
}

/// Shared HTTP client: rustls, sensible timeouts, no cookies.
pub fn http_client() -> LlmResult<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .user_agent(concat!("Sift/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| LlmError::Config(format!("could not build HTTP client: {e}")))
}

/// Shared `test()` body: a tiny request to the test model (or the first listed one).
pub(crate) async fn run_test(
    provider: &dyn LlmProvider,
    test_model: Option<String>,
) -> LlmResult<TestResult> {
    let started = std::time::Instant::now();
    let model = match test_model.filter(|m| !m.trim().is_empty()) {
        Some(m) => m,
        None => provider
            .list_models()
            .await?
            .into_iter()
            .next()
            .map(|m| m.id)
            .ok_or_else(|| {
                LlmError::Config("the provider lists no models; choose a model to test".into())
            })?,
    };
    let mut req = CompletionRequest::new(
        model.clone(),
        vec![ChatMessage::user(CONNECTION_TEST_PROMPT.trim())],
    );
    req.temperature = None;
    req.max_tokens = 16;
    let resp = provider.complete(req).await?;
    let matched = resp.model_match();
    let reported = resp.effective_model().map(str::to_owned);
    let message = match (&matched, &reported) {
        (ModelMatch::Different, Some(r)) => {
            format!(
                "Connected, but the response came from {r}, not {model}. Runs would record {r}."
            )
        }
        _ => format!("Connected. {model} replied."),
    };
    Ok(TestResult {
        ok: true,
        model: Some(model),
        reported_model: reported,
        model_match: matched,
        latency_ms: started.elapsed().as_millis() as u64,
        message,
    })
}

// ---------------------------------------------------------------------------
// Model identity (§6.4: log and warn when the response reports another model)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelMatch {
    /// Same ID.
    Exact,
    /// A dated or tagged snapshot of the requested alias (`gpt-4o` -> `gpt-4o-2024-08-06`).
    Snapshot,
    /// A different model answered. Warn.
    Different,
    /// The response did not say.
    Unknown,
}

pub fn model_match(requested: &str, reported: &str) -> ModelMatch {
    let (req, rep) = (requested.trim(), reported.trim());
    if req.eq_ignore_ascii_case(rep) {
        return ModelMatch::Exact;
    }
    // OpenRouter-style "vendor/model" on one side only.
    let strip = |s: &str| s.rsplit('/').next().unwrap_or(s).to_ascii_lowercase();
    let (req_s, rep_s) = (strip(req), strip(rep));
    if req_s == rep_s {
        return ModelMatch::Exact;
    }
    for sep in ['-', '@', ':'] {
        if let Some(rest) = rep_s.strip_prefix(&format!("{req_s}{sep}")) {
            if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) || rest == "latest" {
                return ModelMatch::Snapshot;
            }
        }
    }
    ModelMatch::Different
}

// ---------------------------------------------------------------------------
// JSON output handling

/// Instruction appended to the system prompt when the server does not enforce the schema.
pub fn json_instruction(schema: &JsonSchema) -> String {
    format!(
        "Respond with a single JSON object only, no prose and no code fences. \
         It must validate against this JSON Schema:\n{}",
        schema.schema
    )
}

/// Parses the model's text as JSON (tolerating a surrounding code fence) and
/// validates it against the schema.
pub fn parse_and_validate(text: &str, schema: &JsonSchema) -> LlmResult<serde_json::Value> {
    let trimmed = strip_code_fence(text.trim());
    let mut value: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| LlmError::InvalidJson {
            reason: format!("not JSON: {e}"),
            raw: text.to_owned(),
        })?;
    coerce_to_schema(&mut value, &schema.schema);
    // The schema sent to the provider forbids extra keys (strict mode needs
    // that); locally, harmless extra keys are ignored rather than rejected.
    let mut relaxed = schema.schema.clone();
    allow_extra_keys(&mut relaxed);
    let validator = jsonschema::validator_for(&relaxed)
        .map_err(|e| LlmError::Config(format!("invalid schema {}: {e}", schema.name)))?;
    let errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| e.to_string())
        .take(5)
        .collect();
    if errors.is_empty() {
        Ok(value)
    } else {
        Err(LlmError::InvalidJson {
            reason: errors.join("; "),
            raw: text.to_owned(),
        })
    }
}

/// Repairs unambiguous shape slips from models that ignore structured output,
/// guided by the schema: a `{code: verdict}` map where a list of two-field
/// objects is expected, enum synonyms ("meets" for "met", "fails" for
/// "failed", "unclear" for "cannot_judge"), and numbers sent as strings.
/// Anything else is left for validation to reject. The raw text is always
/// logged, so the repair is auditable.
pub fn coerce_to_schema(value: &mut serde_json::Value, schema: &serde_json::Value) {
    use serde_json::Value;
    let types: Vec<&str> = match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if let Some(Value::Array(options)) = schema.get("enum") {
        if let Value::String(s) = value {
            if let Some(hit) = enum_synonym(s, options) {
                *s = hit;
            }
        }
        return;
    }
    if types.contains(&"number") || types.contains(&"integer") {
        if let Value::String(s) = value {
            if let Ok(n) = s.trim().parse::<f64>() {
                if let Some(n) = serde_json::Number::from_f64(n) {
                    *value = Value::Number(n);
                }
            } else if types.contains(&"integer") {
                // "[[page 6]]", "page 6", "p. 6": exactly one whole number in the text.
                let runs: Vec<&str> = s
                    .split(|c: char| !c.is_ascii_digit())
                    .filter(|r| !r.is_empty())
                    .collect();
                if let [only] = runs.as_slice() {
                    if let Ok(n) = only.parse::<i64>() {
                        *value = Value::Number(n.into());
                    }
                }
            }
        }
        return;
    }
    if types.contains(&"array") {
        let Some(items) = schema.get("items") else {
            return;
        };
        if let Value::Object(map) = value {
            // {"aim": {"value": .., "source_quote": ..}} -> [{"field": "aim", "value": .., ..}]:
            // an object of objects, each missing the one required string key.
            let missing_key: Vec<&String> = items
                .get("properties")
                .and_then(Value::as_object)
                .map(|props| {
                    props
                        .iter()
                        .filter(|(k, p)| {
                            p.get("type").and_then(Value::as_str) == Some("string")
                                && map.values().all(|v| v.get(k.as_str()).is_none())
                        })
                        .map(|(k, _)| k)
                        .collect()
                })
                .unwrap_or_default();
            if !map.is_empty() && map.values().all(Value::is_object) && missing_key.len() == 1 {
                let key = missing_key[0].clone();
                let list = std::mem::take(map)
                    .into_iter()
                    .map(|(k, v)| {
                        let mut o = v.as_object().cloned().unwrap_or_default();
                        o.insert(key.clone(), Value::String(k));
                        Value::Object(o)
                    })
                    .collect();
                *value = Value::Array(list);
            }
        }
        if let Value::Object(map) = value {
            // {"IN-POP": "fails"} -> [{"code": "IN-POP", "judgement": "fails"}]
            let keys: Vec<&String> = items
                .get("required")
                .and_then(Value::as_array)
                .map(|r| r.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                .and_then(|r| {
                    let props = items.get("properties")?.as_object()?;
                    (r.len() == 2 && props.len() == 2)
                        .then(|| props.keys().filter(|k| r.contains(&k.as_str())).collect())
                })
                .unwrap_or_default();
            if keys.len() == 2 && map.values().all(|v| !v.is_object() && !v.is_array()) {
                let (k0, k1) = (keys[0], keys[1]);
                // The key field is the one without an enum (e.g. "code").
                let (key_field, val_field) = if items["properties"][k0].get("enum").is_some() {
                    (k1, k0)
                } else {
                    (k0, k1)
                };
                let list = std::mem::take(map)
                    .into_iter()
                    .map(|(k, v)| {
                        let mut o = serde_json::Map::new();
                        o.insert(key_field.clone(), Value::String(k));
                        o.insert(val_field.clone(), v);
                        Value::Object(o)
                    })
                    .collect();
                *value = Value::Array(list);
            }
        }
        if let Value::Array(list) = value {
            list.iter_mut().for_each(|v| coerce_to_schema(v, items));
        }
        return;
    }
    let props = schema.get("properties").and_then(Value::as_object);
    let non_empty_list = value.as_array().is_some_and(|l| !l.is_empty());
    if let (true, Some(props)) = (non_empty_list, props) {
        // [{..}, {..}] -> {"codes": [{..}, {..}]}: the wrapper was left off an
        // object whose only property is a list. An empty list stays rejected.
        if let [(key, sub)] = props.iter().collect::<Vec<_>>().as_slice() {
            if sub.get("type").and_then(Value::as_str) == Some("array") {
                let mut wrapped = serde_json::Map::new();
                wrapped.insert((*key).clone(), std::mem::take(value));
                *value = Value::Object(wrapped);
            }
        }
    }
    if let (Value::Object(map), Some(props)) = (&mut *value, props) {
        // {"ids": [..]} where "finding_ids" is required: exactly one required key
        // is missing, exactly one key is unknown, and the required key ends with
        // "_" plus the unknown one.
        let required: Vec<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let missing: Vec<&str> = required
            .iter()
            .copied()
            .filter(|k| !map.contains_key(*k))
            .collect();
        let unknown: Vec<String> = map
            .keys()
            .filter(|k| !props.contains_key(k.as_str()))
            .cloned()
            .collect();
        if let ([want], [have]) = (missing.as_slice(), unknown.as_slice()) {
            if want.ends_with(&format!("_{have}")) {
                if let Some(v) = map.remove(have) {
                    map.insert((*want).to_owned(), v);
                }
            }
        }
        for (k, sub) in props {
            if let Some(v) = map.get_mut(k) {
                coerce_to_schema(v, sub);
                // An optional reasoning aid that is still malformed is dropped
                // rather than guessed at; required fields are left to fail.
                let omittable = sub
                    .get("description")
                    .and_then(Value::as_str)
                    .is_some_and(|d| d.contains(MAY_BE_OMITTED));
                if omittable && !fits(v, sub) {
                    map.remove(k);
                }
            }
        }
    }
}

fn fits(value: &serde_json::Value, schema: &serde_json::Value) -> bool {
    let mut relaxed = schema.clone();
    allow_extra_keys(&mut relaxed);
    jsonschema::validator_for(&relaxed).is_ok_and(|v| v.is_valid(value))
}

/// Maps a near-miss enum value onto the schema's own spelling, only when the
/// meaning is unambiguous.
fn enum_synonym(s: &str, options: &[serde_json::Value]) -> Option<String> {
    let norm = |x: &str| x.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    let n = norm(s);
    let allowed: Vec<&str> = options.iter().filter_map(|o| o.as_str()).collect();
    if let Some(a) = allowed.iter().find(|a| norm(a) == n) {
        return Some((*a).to_owned());
    }
    const GROUPS: &[&[&str]] = &[
        &[
            "met",
            "meets",
            "meet",
            "yes",
            "true",
            "satisfied",
            "fulfilled",
            "pass",
            "passes",
        ],
        &[
            "failed",
            "fails",
            "fail",
            "no",
            "false",
            "not_met",
            "unmet",
            "does_not_meet",
        ],
        &[
            "cannot_judge",
            "cannot_be_judged",
            "can_not_judge",
            "unclear",
            "unknown",
            "not_reported",
            "insufficient_information",
            "cannot_tell",
        ],
        &["include", "included", "inclusion"],
        &["exclude", "excluded", "exclusion"],
        &["uncertain", "unsure"],
    ];
    let group = GROUPS.iter().find(|g| g.contains(&n.as_str()))?;
    let hits: Vec<&&str> = allowed
        .iter()
        .filter(|a| group.contains(&norm(a).as_str()))
        .collect();
    (hits.len() == 1).then(|| (*hits[0]).to_owned())
}

/// Marker in a property's description: the provider is asked for it, but a
/// response without it is still accepted.
pub const MAY_BE_OMITTED: &str = "(may be omitted)";

fn allow_extra_keys(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            map.remove("additionalProperties");
            let omittable: Vec<String> = map
                .get("properties")
                .and_then(|p| p.as_object())
                .map(|props| {
                    props
                        .iter()
                        .filter(|(_, s)| {
                            s.get("description")
                                .and_then(|d| d.as_str())
                                .is_some_and(|d| d.contains(MAY_BE_OMITTED))
                        })
                        .map(|(k, _)| k.clone())
                        .collect()
                })
                .unwrap_or_default();
            if let Some(serde_json::Value::Array(req)) = map.get_mut("required") {
                req.retain(|r| !r.as_str().is_some_and(|r| omittable.iter().any(|o| o == r)));
            }
            map.values_mut().for_each(allow_extra_keys);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(allow_extra_keys),
        _ => {}
    }
}

fn strip_code_fence(s: &str) -> &str {
    let Some(rest) = s.strip_prefix("```") else {
        return s;
    };
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_alphanumeric());
    rest.strip_suffix("```").unwrap_or(rest).trim()
}

/// True when a 400 body says the server rejected a named parameter.
pub(crate) fn rejects_param(status: u16, body: &str, names: &[&str]) -> bool {
    if status != 400 && status != 422 {
        return false;
    }
    let b = body.to_ascii_lowercase();
    names.iter().any(|n| b.contains(n))
}

/// Short, safe error text from a provider error body (never the request).
pub(crate) fn error_message(body: &str) -> String {
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let msg = parsed
        .as_ref()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.pointer("/message"))
                .or_else(|| v.pointer("/error"))
                .and_then(|m| m.as_str())
        })
        .map(str::to_owned)
        .unwrap_or_else(|| body.chars().take(300).collect());
    msg.chars().take(500).collect()
}

#[cfg(test)]
mod tests {

    #[test]
    fn repairs_criteria_maps_and_enum_synonyms_from_schema_ignoring_models() {
        // Shape returned by a gateway route that ignores response_format (live run, 2026-09-25).
        let schema = JsonSchema {
            name: "screening_b".into(),
            schema: serde_json::json!({
                "type": "object", "additionalProperties": false,
                "required": ["criteria", "decision", "confidence", "reason_code"],
                "properties": {
                    "criteria": {"type": "array", "items": {
                        "type": "object", "additionalProperties": false,
                        "required": ["code", "judgement"],
                        "properties": {
                            "code": {"type": "string"},
                            "judgement": {"type": "string", "enum": ["met", "failed", "cannot_judge"]}
                        }}},
                    "decision": {"type": "string", "enum": ["include", "exclude", "uncertain"]},
                    "confidence": {"type": "number"},
                    "reason_code": {"type": ["string", "null"]}
                }
            }),
        };
        let text = r#"{"criteria": {"IN-POP": "fails", "IN-INT": "meets", "IN-DATE": "Cannot be judged"},
                       "decision": "Excluded", "confidence": "0.9", "reason_code": "EX-SETTING"}"#;
        let v = parse_and_validate(text, &schema).unwrap();
        assert_eq!(v["decision"], "exclude");
        assert_eq!(v["confidence"], 0.9);
        let crit = v["criteria"].as_array().unwrap();
        assert_eq!(crit.len(), 3);
        assert!(crit
            .iter()
            .any(|c| c["code"] == "IN-POP" && c["judgement"] == "failed"));
        assert!(crit
            .iter()
            .any(|c| c["code"] == "IN-INT" && c["judgement"] == "met"));
        assert!(crit
            .iter()
            .any(|c| c["code"] == "IN-DATE" && c["judgement"] == "cannot_judge"));

        // A property marked "(may be omitted)" is optional locally; others stay required.
        let mut lenient = schema.clone();
        lenient.schema["properties"]["criteria"]["description"] =
            serde_json::json!(format!("Reasoning aid {MAY_BE_OMITTED}"));
        let without = r#"{"decision": "include", "confidence": 1, "reason_code": null}"#;
        assert!(parse_and_validate(without, &lenient).is_ok());
        assert!(parse_and_validate(without, &schema).is_err());
        assert!(parse_and_validate(
            r#"{"criteria": [], "confidence": 1, "reason_code": null}"#,
            &lenient
        )
        .is_err());

        // A malformed optional field is dropped, not guessed ("does not apply" has no safe mapping).
        let odd = r#"{"criteria": {"EX-POP": "does not apply", "IN-POP": "meets"},
                      "decision": "include", "confidence": 1, "reason_code": null}"#;
        let v = parse_and_validate(odd, &lenient).unwrap();
        assert!(v.get("criteria").is_none());
        assert!(
            parse_and_validate(odd, &schema).is_err(),
            "required fields are never dropped"
        );

        // Page markers copied from the prompt become the page number; two numbers stay invalid.
        let paged = JsonSchema {
            name: "p".into(),
            schema: serde_json::json!({"type": "object", "required": ["page"],
                "properties": {"page": {"type": ["integer", "null"]}}}),
        };
        assert_eq!(
            parse_and_validate(r#"{"page": "[[page 6]]"}"#, &paged).unwrap()["page"],
            6
        );
        assert_eq!(
            parse_and_validate(r#"{"page": "p. 12"}"#, &paged).unwrap()["page"],
            12
        );
        assert!(parse_and_validate(r#"{"page": "pages 3-4"}"#, &paged).is_err());

        // Ambiguous or unknown values are not guessed.
        let bad =
            r#"{"criteria": [], "decision": "probably", "confidence": 1, "reason_code": null}"#;
        assert!(parse_and_validate(bad, &schema).is_err());
    }

    #[test]
    fn restores_a_missing_list_wrapper_and_a_shortened_key() {
        // Shape returned for synthesis coding by a local route (live run, 2026-09-26).
        let schema = JsonSchema {
            name: "synthesis_codes".into(),
            schema: serde_json::json!({
                "type": "object", "additionalProperties": false, "required": ["codes"],
                "properties": {"codes": {"type": "array", "items": {
                    "type": "object", "additionalProperties": false,
                    "required": ["code", "finding_ids"],
                    "properties": {
                        "code": {"type": "string"},
                        "finding_ids": {"type": "array", "items": {"type": "string"}}
                    }}}}
            }),
        };
        let text = r#"[{"code": "language challenges", "ids": ["F1"]},
                       {"code": "racial discrimination", "ids": ["F2", "F5"]}]"#;
        let v = parse_and_validate(text, &schema).unwrap();
        let codes = v["codes"].as_array().unwrap();
        assert_eq!(codes.len(), 2);
        assert_eq!(codes[1]["finding_ids"], serde_json::json!(["F2", "F5"]));

        // A key that is not a suffix of the missing one is not renamed.
        assert!(parse_and_validate(r#"[{"code": "x", "sources": ["F1"]}]"#, &schema).is_err());
        // Two unknown keys are ambiguous and left to fail.
        assert!(parse_and_validate(
            r#"{"codes": [{"code": "x", "ids": ["F1"], "notes": "y"}]}"#,
            &schema
        )
        .is_err());
        // An object with more than one property is never built from a bare list.
        let two = JsonSchema {
            name: "t".into(),
            schema: serde_json::json!({"type": "object", "required": ["a", "b"],
                "properties": {"a": {"type": "array"}, "b": {"type": "string"}}}),
        };
        assert!(parse_and_validate("[1, 2]", &two).is_err());
    }
    use super::*;
    use serde_json::json;

    #[test]
    fn model_match_classes() {
        assert_eq!(model_match("gpt-4o", "gpt-4o"), ModelMatch::Exact);
        assert_eq!(
            model_match("gpt-4o", "gpt-4o-2024-08-06"),
            ModelMatch::Snapshot
        );
        assert_eq!(
            model_match("claude-opus-4-5", "claude-opus-4-5@20251101"),
            ModelMatch::Snapshot
        );
        assert_eq!(model_match("openai/gpt-4o", "gpt-4o"), ModelMatch::Exact);
        assert_eq!(
            model_match("llama3.2", "llama3.2:latest"),
            ModelMatch::Snapshot
        );
        assert_eq!(model_match("gpt-4o", "gpt-4o-mini"), ModelMatch::Different);
        assert_eq!(
            model_match("auto", "dots-studio/dots-3-note-preview:free"),
            ModelMatch::Different
        );
    }

    fn schema() -> JsonSchema {
        JsonSchema {
            name: "screen".into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "decision": {"type": "string", "enum": ["include", "exclude", "uncertain"]},
                    "confidence": {"type": "number", "minimum": 0, "maximum": 1}
                },
                "required": ["decision", "confidence"],
                "additionalProperties": false
            }),
        }
    }

    #[test]
    fn parses_valid_json_and_code_fences() {
        let v =
            parse_and_validate(r#"{"decision":"include","confidence":0.9}"#, &schema()).unwrap();
        assert_eq!(v["decision"], "include");
        let fenced = "```json\n{\"decision\":\"exclude\",\"confidence\":0.2}\n```";
        assert_eq!(
            parse_and_validate(fenced, &schema()).unwrap()["decision"],
            "exclude"
        );
    }

    #[test]
    fn rejects_invalid_json_and_schema_violations() {
        assert!(matches!(
            parse_and_validate("The record should be included.", &schema()),
            Err(LlmError::InvalidJson { .. })
        ));
        let err =
            parse_and_validate(r#"{"decision":"maybe","confidence":2}"#, &schema()).unwrap_err();
        let LlmError::InvalidJson { reason, .. } = err else {
            panic!("wrong error")
        };
        assert!(reason.contains("maybe") || reason.contains("enum"));
    }

    #[test]
    fn an_object_keyed_by_field_becomes_the_expected_list() {
        let schema = JsonSchema {
            name: "extraction".into(),
            schema: json!({
                "type": "object",
                "properties": {"fields": {"type": "array", "items": {
                    "type": "object",
                    "properties": {
                        "field": {"type": "string"},
                        "value": {"type": ["string", "null"]},
                        "page": {"type": ["integer", "null"]}
                    },
                    "required": ["field", "value", "page"],
                    "additionalProperties": false
                }}},
                "required": ["fields"],
                "additionalProperties": false
            }),
        };
        let v = parse_and_validate(
            r#"{"fields": {"aim": {"value": "to explore", "page": "[[page 2]]"}, "funding": {"value": null, "page": null}}}"#,
            &schema,
        )
        .unwrap();
        let list = v["fields"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        let aim = list.iter().find(|f| f["field"] == "aim").unwrap();
        assert_eq!(
            (aim["value"].as_str(), aim["page"].as_i64()),
            (Some("to explore"), Some(2))
        );
        // An empty list where an object is expected is still rejected.
        assert!(parse_and_validate("[]", &schema).is_err());
    }

    #[test]
    fn error_message_prefers_structured_fields() {
        assert_eq!(
            error_message(
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#
            ),
            "Overloaded"
        );
        assert_eq!(error_message(r#"{"message":"bad key"}"#), "bad key");
        assert_eq!(error_message("plain text"), "plain text");
    }

    #[test]
    fn param_rejection_detection() {
        assert!(rejects_param(
            400,
            r#"{"error":{"message":"temperature is not supported"}}"#,
            &["temperature"]
        ));
        assert!(!rejects_param(500, "temperature", &["temperature"]));
    }
}
