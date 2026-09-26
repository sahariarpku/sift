//! Agents (§8). Each agent is a versioned prompt plus a fixed JSON output
//! schema generated from a Rust type. Every call is logged with its raw
//! response in `llm_call`, and every decision becomes an event that records
//! provider, model, prompt version, tokens and cost (principle 4).

pub mod appraisal;
pub mod extraction;
pub mod par;
pub mod prescreen;
pub mod protocol;
pub mod screening;
pub mod search;
pub mod synthesis;

use rusqlite::{params, Connection};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::events::{ActorType, NewEvent};
use crate::jobs::{JobContext, JobError};
use crate::llm::service::{Outcome, Route};
use crate::llm::store::Task;
use crate::llm::{ChatMessage, CompletionRequest, JsonSchema as Schema, LlmError};
use crate::time::now_iso;

// ---------------------------------------------------------------------------
// Prompts: markdown files in /prompts, compiled in. The part after a line
// `---user---` is the user message template; `{{name}}` placeholders are filled in.

const PROMPTS: &[(&str, &str)] = &[
    (
        "protocol.v1",
        include_str!("../../../prompts/protocol.v1.md"),
    ),
    (
        "protocol.v2",
        include_str!("../../../prompts/protocol.v2.md"),
    ),
    (
        "laya_prescreen.v1",
        include_str!("../../../prompts/laya_prescreen.v1.md"),
    ),
    (
        "search_broaden.v1",
        include_str!("../../../prompts/search_broaden.v1.md"),
    ),
    (
        "screening_a.v1",
        include_str!("../../../prompts/screening_a.v1.md"),
    ),
    (
        "screening_b.v1",
        include_str!("../../../prompts/screening_b.v1.md"),
    ),
    ("arbiter.v1", include_str!("../../../prompts/arbiter.v1.md")),
    (
        "ft_screening_a.v1",
        include_str!("../../../prompts/ft_screening_a.v1.md"),
    ),
    (
        "ft_screening_b.v1",
        include_str!("../../../prompts/ft_screening_b.v1.md"),
    ),
    (
        "ft_arbiter.v1",
        include_str!("../../../prompts/ft_arbiter.v1.md"),
    ),
    (
        "extraction.v1",
        include_str!("../../../prompts/extraction.v1.md"),
    ),
    (
        "extraction.v2",
        include_str!("../../../prompts/extraction.v2.md"),
    ),
    (
        "appraisal.v1",
        include_str!("../../../prompts/appraisal.v1.md"),
    ),
    (
        "appraisal.v2",
        include_str!("../../../prompts/appraisal.v2.md"),
    ),
    (
        "appraisal.v3",
        include_str!("../../../prompts/appraisal.v3.md"),
    ),
    (
        "appraisal_reconcile.v1",
        include_str!("../../../prompts/appraisal_reconcile.v1.md"),
    ),
    (
        "synthesis_codes.v1",
        include_str!("../../../prompts/synthesis_codes.v1.md"),
    ),
    (
        "synthesis_themes.v1",
        include_str!("../../../prompts/synthesis_themes.v1.md"),
    ),
    (
        "synthesis_analytical.v1",
        include_str!("../../../prompts/synthesis_analytical.v1.md"),
    ),
    ("cerqual.v1", include_str!("../../../prompts/cerqual.v1.md")),
    ("swim.v1", include_str!("../../../prompts/swim.v1.md")),
    ("grade.v1", include_str!("../../../prompts/grade.v1.md")),
    (
        "manuscript.v1",
        include_str!("../../../prompts/manuscript.v1.md"),
    ),
    (
        "synthesis_codes.v2",
        include_str!("../../../prompts/synthesis_codes.v2.md"),
    ),
    (
        "synthesis_themes.v2",
        include_str!("../../../prompts/synthesis_themes.v2.md"),
    ),
    (
        "synthesis_analytical.v2",
        include_str!("../../../prompts/synthesis_analytical.v2.md"),
    ),
    ("cerqual.v2", include_str!("../../../prompts/cerqual.v2.md")),
    ("swim.v2", include_str!("../../../prompts/swim.v2.md")),
    ("grade.v2", include_str!("../../../prompts/grade.v2.md")),
    (
        "manuscript.v2",
        include_str!("../../../prompts/manuscript.v2.md"),
    ),
    (
        "manuscript.v3",
        include_str!("../../../prompts/manuscript.v3.md"),
    ),
    (
        "extraction_requote.v1",
        include_str!("../../../prompts/extraction_requote.v1.md"),
    ),
];

#[derive(Debug, Clone)]
pub struct Prompt {
    pub version: &'static str,
    pub system: String,
    pub user: String,
}

pub fn prompt(version: &str) -> Result<Prompt, AgentError> {
    let (v, text) = PROMPTS
        .iter()
        .find(|(v, _)| *v == version)
        .ok_or_else(|| AgentError::Fatal(format!("unknown prompt {version}")))?;
    let (system, user) = text
        .split_once("\n---user---\n")
        .unwrap_or((text, "{{input}}"));
    Ok(Prompt {
        version: v,
        system: system.trim().to_owned(),
        user: user.trim().to_owned(),
    })
}

/// Fills `{{name}}` placeholders. Unknown placeholders are left visible so a
/// template bug shows up in the logged request rather than silently.
pub fn render(template: &str, vars: &[(&str, String)]) -> String {
    let mut out = template.to_owned();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

// ---------------------------------------------------------------------------
// Output schemas

/// JSON Schema for an agent's output type, shaped for strict structured
/// output: subschemas inlined, every property required (nullable where the
/// Rust field is an `Option`), no extra properties, no `format` keywords.
pub fn schema_for<T: JsonSchema>(name: &str) -> Schema {
    let root = schemars::generate::SchemaSettings::draft07()
        .with(|s| s.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<T>();
    let mut value: Value = root.into();
    tidy(&mut value);
    if let Some(obj) = value.as_object_mut() {
        obj.remove("$schema");
        obj.remove("title");
        obj.remove("definitions");
        obj.remove("$defs");
    }
    Schema {
        name: name.to_owned(),
        schema: value,
    }
}

fn tidy(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.remove("format");
            map.remove("title");
            if let Some(Value::Object(props)) = map.get("properties") {
                let keys: Vec<Value> = props.keys().map(|k| Value::String(k.clone())).collect();
                map.insert("required".into(), Value::Array(keys));
                map.insert("additionalProperties".into(), Value::Bool(false));
            }
            for (_, child) in map.iter_mut() {
                tidy(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(tidy),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Calls

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// Two invalid responses, or output that failed validation twice: a person decides.
    #[error("needs a person: {0}")]
    NeedsHuman(String),
    #[error("{0}")]
    Pause(String),
    #[error("{0}")]
    RateLimited(String),
    #[error("{0}")]
    Retryable(String),
    #[error("{0}")]
    Fatal(String),
}

impl From<crate::error::Error> for AgentError {
    fn from(e: crate::error::Error) -> Self {
        AgentError::Fatal(e.to_string())
    }
}

impl From<AgentError> for JobError {
    fn from(e: AgentError) -> Self {
        match e {
            AgentError::Pause(m) => JobError::Pause(m),
            AgentError::RateLimited(m) => JobError::RateLimited(m),
            AgentError::Retryable(m) => JobError::Retryable(m),
            AgentError::NeedsHuman(m) | AgentError::Fatal(m) => JobError::Fatal(m),
        }
    }
}

/// Maps a provider failure after its own retries. The configured model is
/// never swapped (strict mode, §6.4); the run pauses with the error instead.
fn from_llm(task: Task, e: LlmError) -> AgentError {
    match &e {
        LlmError::Http { status: 429, .. } => AgentError::RateLimited(format!(
            "{} is rate-limited by the provider. The run resumes in two minutes. ({e})",
            task.label()
        )),
        // Temporary upstream trouble (5xx, timeouts, gateway overload): wait
        // and retry the same model. The model is never swapped (§6.4).
        LlmError::Http {
            retryable: true, ..
        }
        | LlmError::Unreachable { .. } => AgentError::RateLimited(format!(
            "{}'s model is temporarily unavailable. The run retries the same model in two minutes. ({e})",
            task.label()
        )),
        LlmError::Auth { .. } | LlmError::ModelNotFound { .. } | LlmError::Config(_) => {
            AgentError::Pause(format!(
                "{}: {e}. Check Settings, then resume.",
                task.label()
            ))
        }
        _ => AgentError::Retryable(e.to_string()),
    }
}

/// Provenance for events written from an agent result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CallMeta {
    pub provider: String,
    /// The model that actually answered (reported by the response), else the requested one.
    pub model_id: String,
    pub prompt_version: String,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub cost_usd: Option<f64>,
    pub model_warning: Option<String>,
}

impl CallMeta {
    /// An agent event carrying this call's provenance.
    pub fn event(
        &self,
        review_id: &str,
        target_type: &str,
        target_id: &str,
        stage: &str,
        actor_id: &str,
        decision: &str,
    ) -> NewEvent {
        NewEvent {
            review_id: review_id.into(),
            target_type: target_type.into(),
            target_id: target_id.into(),
            stage: stage.into(),
            actor_type: Some(ActorType::Agent),
            actor_id: actor_id.into(),
            provider: Some(self.provider.clone()),
            model_id: Some(self.model_id.clone()),
            prompt_version: Some(self.prompt_version.clone()),
            decision: decision.into(),
            tokens_in: self.tokens_in,
            tokens_out: self.tokens_out,
            cost_usd: self.cost_usd,
            ..Default::default()
        }
    }
}

/// Checks the spending cap (§6.4). Pauses the run when reached.
fn check_cap(ctx: &JobContext) -> Result<(), AgentError> {
    let app = ctx.env.app_db()?;
    let cap = crate::settings::load(&app)?.spending_cap_usd;
    let conn = ctx.open()?;
    let spent = crate::events::total_cost(&conn)? + unlogged_cost(&conn)?;
    if crate::llm::cost::cap_reached(spent, cap) {
        return Err(AgentError::Pause(format!(
            "Spending cap reached: ${spent:.2} of ${:.2}. Raise the cap in Settings, then resume.",
            cap.unwrap_or(0.0)
        )));
    }
    Ok(())
}

/// Cost of calls whose events were never written (failed validation), so the cap sees all spend.
fn unlogged_cost(conn: &Connection) -> crate::error::Result<f64> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(cost_usd), 0.0) FROM llm_call WHERE json_valid = 0",
        [],
        |r| r.get(0),
    )?)
}

fn log_calls(
    ctx: &JobContext,
    route: &Route,
    prompt_version: &str,
    req: &CompletionRequest,
    out: &Outcome,
) -> crate::error::Result<()> {
    let conn = ctx.open()?;
    let request = json!({"system": req.system, "messages": req.messages, "schema": req.json_schema, "max_tokens": req.max_tokens});
    for r in &out.responses {
        conn.execute(
            "INSERT INTO llm_call (review_id, task, provider, model_requested, model_reported, routed_via, prompt_version,
                temperature_applied, structured_output, tokens_in, tokens_out, cost_usd, request, raw_response, json_valid, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                ctx.review_id,
                route.task.as_str(),
                route.provider_name,
                route.model_id,
                r.reported_model,
                r.routed_via,
                prompt_version,
                r.temperature_applied,
                r.structured_output,
                r.usage.input_tokens.map(|t| t as i64),
                r.usage.output_tokens.map(|t| t as i64),
                crate::llm::cost::estimate(&r.usage, route.price),
                request.to_string(),
                r.raw_body,
                req.json_schema.as_ref().map(|_| r.json.is_some()),
                now_iso(),
            ],
        )?;
    }
    Ok(())
}

pub struct Reply<T> {
    pub value: T,
    pub meta: CallMeta,
}

/// Calls the model assigned to `task` with a prompt and typed output.
///
/// `validate` checks the parsed output against things a schema cannot express
/// (a quote that must appear verbatim, a criterion code that must exist). An
/// output that fails validation is retried once; two failures return
/// `NeedsHuman` (§6.4).
pub async fn call<T, F>(
    ctx: &JobContext,
    task: Task,
    prompt_version: &str,
    vars: &[(&str, String)],
    max_tokens: u32,
    validate: F,
) -> Result<Reply<T>, AgentError>
where
    T: DeserializeOwned + JsonSchema,
    F: Fn(&T) -> Result<(), String>,
{
    check_cap(ctx)?;
    let p = prompt(prompt_version)?;
    // Keychain reads are synchronous and can wait on a macOS approval
    // prompt; run them on the blocking pool so other jobs keep running.
    let env = ctx.env.clone();
    let route = tokio::task::spawn_blocking(move || -> Result<_, AgentError> {
        let app = env.app_db()?;
        env.llm
            .route(&app, env.secrets.as_ref(), task)
            .map_err(|e| AgentError::Pause(e.to_string()))
    })
    .await
    .map_err(|e| AgentError::Fatal(format!("routing task failed: {e}")))??;
    let schema = schema_for::<T>(prompt_version.split('.').next().unwrap_or("output"));
    let mut req = CompletionRequest::new(
        route.model_id.clone(),
        vec![ChatMessage::user(render(&p.user, vars))],
    );
    req.system = Some(render(&p.system, vars));
    req.json_schema = Some(schema);
    req.max_tokens = max_tokens;

    let mut last_problem = String::new();
    let mut total_in = 0i64;
    let mut total_out = 0i64;
    let mut total_cost = Some(0.0);
    for _attempt in 0..2 {
        let out = ctx
            .env
            .llm
            .complete(&route, req.clone())
            .await
            .map_err(|e| from_llm(task, e))?;
        log_calls(ctx, &route, p.version, &req, &out)?;
        for r in &out.responses {
            total_in += r.usage.input_tokens.unwrap_or(0) as i64;
            total_out += r.usage.output_tokens.unwrap_or(0) as i64;
        }
        total_cost = match (total_cost, out.cost_usd) {
            (Some(a), Some(b)) => Some(a + b),
            _ => None,
        };
        if out.needs_human {
            let why = out
                .responses
                .last()
                .and_then(|r| r.json_error.clone())
                .unwrap_or_default();
            return Err(AgentError::NeedsHuman(format!(
                "two invalid JSON responses ({why})"
            )));
        }
        let last = out
            .responses
            .last()
            .ok_or_else(|| AgentError::Retryable("no response".into()))?;
        let Some(json) = out.json.clone() else {
            return Err(AgentError::Retryable("no JSON in response".into()));
        };
        let value: T = match serde_json::from_value(json) {
            Ok(v) => v,
            Err(e) => {
                last_problem = format!("output did not match the expected shape: {e}");
                continue;
            }
        };
        match validate(&value) {
            Ok(()) => {
                let meta = CallMeta {
                    provider: route.provider_name.clone(),
                    model_id: last.effective_model().unwrap_or(&route.model_id).to_owned(),
                    prompt_version: p.version.to_owned(),
                    tokens_in: Some(total_in),
                    tokens_out: Some(total_out),
                    cost_usd: total_cost,
                    model_warning: out.model_warning.clone(),
                };
                return Ok(Reply { value, meta });
            }
            Err(problem) => last_problem = problem,
        }
    }
    Err(AgentError::NeedsHuman(format!(
        "output failed validation twice: {last_problem}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct Out {
        decision: Decision,
        note: Option<String>,
        items: Vec<Item>,
    }
    #[derive(Deserialize, JsonSchema)]
    #[serde(rename_all = "lowercase")]
    #[allow(dead_code)]
    enum Decision {
        Include,
        Exclude,
    }
    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct Item {
        code: String,
    }

    #[test]
    fn schemas_are_strict_and_inlined() {
        let s = schema_for::<Out>("out").schema;
        assert!(s.get("$schema").is_none());
        assert_eq!(s["additionalProperties"], false);
        let required: Vec<&str> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            required.len(),
            3,
            "optional fields are required-but-nullable"
        );
        assert_eq!(
            s["properties"]["items"]["items"]["additionalProperties"],
            false
        );
        assert!(!s.to_string().contains("$ref"));
    }

    #[test]
    fn every_prompt_parses_and_has_a_version() {
        for (v, _) in PROMPTS {
            let p = prompt(v).unwrap();
            assert!(!p.system.is_empty(), "{v}");
            assert!(!p.user.is_empty(), "{v}");
            assert!(v.contains(".v"), "{v} must carry a version suffix");
        }
    }

    #[test]
    fn transient_failures_wait_and_config_errors_stop() {
        let http = |status| LlmError::Http {
            status,
            message: "x".into(),
            retryable: status >= 500 || status == 429,
            attempts: 3,
        };
        assert!(matches!(
            from_llm(Task::ScreenerB, http(429)),
            AgentError::RateLimited(_)
        ));
        assert!(matches!(
            from_llm(Task::ScreenerB, http(502)),
            AgentError::RateLimited(_)
        ));
        let unreachable = LlmError::Unreachable {
            url: "u".into(),
            message: "m".into(),
        };
        assert!(matches!(
            from_llm(Task::ScreenerB, unreachable),
            AgentError::RateLimited(_)
        ));
        let auth = LlmError::Auth {
            status: 401,
            message: "bad key".into(),
        };
        assert!(matches!(
            from_llm(Task::ScreenerB, auth),
            AgentError::Pause(_)
        ));
        let missing = LlmError::ModelNotFound {
            status: 404,
            message: "gone".into(),
        };
        assert!(matches!(
            from_llm(Task::Arbiter, missing),
            AgentError::Pause(_)
        ));
    }

    #[test]
    fn render_fills_placeholders() {
        assert_eq!(
            render("Q: {{q}} / {{missing}}", &[("q", "why?".into())]),
            "Q: why? / {{missing}}"
        );
    }
}
