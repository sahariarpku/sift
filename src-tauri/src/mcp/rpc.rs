//! JSON-RPC for the MCP endpoint: lifecycle, `tools/list` and `tools/call`.
//!
//! Spec: https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle
//! and https://modelcontextprotocol.io/specification/2025-11-25/server/tools
//! - `initialize` answers with the client's protocol version when supported,
//!   else the latest this server speaks.
//! - An unknown tool is a protocol error (-32602); a tool that runs and fails
//!   returns a result with `isError: true` so the model can read the message.
//!
//! Deliberately missing: any tool that records a decision as a person
//! (screening, extraction flags, Gate 2 answers, sign-off). An assistant is
//! not the review's author, and the log must not say it was. Points that
//! wait for a person are settled by bypass mode, which is disclosed.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use super::bridge::Bridge;
use crate::agents::protocol::ReviewOptions;
use crate::commands::{pipeline as cmd, reviews as review_cmd, CommandError};
use crate::state::AppState;

pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];
const LATEST: &str = PROTOCOL_VERSIONS[0];

const INSTRUCTIONS: &str = "Sift runs PRISMA 2020 systematic reviews on this computer. \
You can do two jobs. (1) Act as Sift's AI: when a provider named \"Claude or ChatGPT (via MCP)\" \
is assigned to tasks in Sift's Settings, the app's model calls wait for you. Loop: call \
sift_next_task, do exactly what its system prompt asks, and send the answer with \
sift_submit_task (JSON matching the schema when one is given; copy quotes verbatim). \
Keep looping until sift_next_task reports nothing for a while. (2) Operate reviews: start one, \
check status, pause or resume, and turn on bypass mode so nothing waits for a person. \
Bypass mode and skipped gates leave the review permanently unverified, and the report says so.";

pub struct Server {
    bridge: Arc<Bridge>,
    app: Option<AppHandle>,
    /// `clientInfo.name` from the last `initialize`, for disclosure.
    client: Mutex<Option<String>>,
}

#[derive(Debug)]
struct ToolError(String);

impl From<CommandError> for ToolError {
    fn from(e: CommandError) -> Self {
        Self(e.message)
    }
}

impl From<crate::commands::reviews::CommandError> for ToolError {
    fn from(e: crate::commands::reviews::CommandError) -> Self {
        Self(e.message)
    }
}

impl From<serde_json::Error> for ToolError {
    fn from(e: serde_json::Error) -> Self {
        Self(e.to_string())
    }
}

type ToolResult = Result<String, ToolError>;

fn pretty<T: Serialize>(v: &T) -> ToolResult {
    Ok(serde_json::to_string_pretty(v)?)
}

fn str_arg(args: &Value, key: &str) -> Result<String, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ToolError(format!("missing argument: {key}")))
}

fn bool_arg(args: &Value, key: &str) -> Result<bool, ToolError> {
    args.get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| ToolError(format!("missing argument: {key} (true or false)")))
}

fn review_id() -> Value {
    json!({"type": "string", "description": "Review ID from sift_list_reviews or sift_start_review."})
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required},
    })
}

pub fn tools() -> Vec<Value> {
    vec![
        tool(
            "sift_next_task",
            "Waits for the next model call Sift wants answered and returns it: id, system prompt, messages, and the JSON schema the answer must match (null means plain text). Returns a short note when nothing arrives in time.",
            json!({"wait_seconds": {"type": "integer", "minimum": 1, "maximum": 120, "description": "How long to wait for a task. Default 30."}}),
            &[],
        ),
        tool(
            "sift_submit_task",
            "Answers a task from sift_next_task. When the task has a schema, content must be one JSON object matching it; a mismatch comes back as an error to fix and resubmit. Quotes must be copied verbatim from the text given.",
            json!({
                "id": {"type": "string"},
                "content": {"type": "string", "description": "The answer: JSON text when the task has a schema."},
                "model": {"type": "string", "description": "Your model name, e.g. claude-opus-5-5 or gpt-5. Recorded in the automation disclosure."},
            }),
            &["id", "content"],
        ),
        tool("sift_bridge_status", "How many model calls are waiting for an answer.", json!({}), &[]),
        tool("sift_list_reviews", "Lists the reviews on this computer.", json!({}), &[]),
        tool(
            "sift_start_review",
            "Starts a review from a research question. Sift drafts the protocol and runs on its own; with bypass true it never waits for a person (and stays unverified).",
            json!({
                "question": {"type": "string"},
                "bypass": {"type": "boolean", "description": "Settle every point that would wait for a person with a disclosed rule. Default: the app setting."},
                "options": {"type": "object", "description": "Optional. camelCase fields: yearFrom, yearTo, languages[], reviewType (detect|quantitative|qualitative|mixed), sources[] (eric, openalex, core, pubmed, semantic_scholar, wos, scopus), maxRecordsPerSource, citationChasing, seedCheck, uncertainToFullText, retrievalDeadlineHours."},
            }),
            &["question"],
        ),
        tool(
            "sift_status",
            "Where a review is: stage states, what it is waiting for (halt), jobs queued, running and failed, verification, bypass, and the PRISMA counts.",
            json!({"review_id": review_id()}),
            &["review_id"],
        ),
        tool("sift_pause", "Pauses a review.", json!({"review_id": review_id()}), &["review_id"]),
        tool("sift_resume", "Resumes a paused review.", json!({"review_id": review_id()}), &["review_id"]),
        tool(
            "sift_set_bypass",
            "Turns bypass mode on or off. On: gates are skipped, queues are settled by fixed rules, and the review stays unverified for good.",
            json!({"review_id": review_id(), "on": {"type": "boolean"}}),
            &["review_id", "on"],
        ),
        tool(
            "sift_skip_gate",
            "Skips one human gate (1 protocol, 2 calibration, 3 sign-off). The review stays unverified for good.",
            json!({"review_id": review_id(), "gate": {"type": "integer", "enum": [1, 2, 3]}}),
            &["review_id", "gate"],
        ),
        tool(
            "sift_skip_stage",
            "Stops an optional stage early (e.g. citation_chasing, retrieval). Disclosed in the report.",
            json!({"review_id": review_id(), "stage": {"type": "string"}}),
            &["review_id", "stage"],
        ),
        tool("sift_retry_failed", "Retries a review's failed jobs.", json!({"review_id": review_id()}), &["review_id"]),
        tool(
            "sift_human_queue",
            "Lists records or full texts waiting for a person's decision (read only; settle them with bypass mode or in the app).",
            json!({"review_id": review_id(), "stage": {"type": "string", "enum": ["ta_screen", "ft_screen"]}}),
            &["review_id", "stage"],
        ),
        tool(
            "sift_decision_log",
            "The latest decisions in the review's event log, newest first.",
            json!({"review_id": review_id(), "limit": {"type": "integer", "minimum": 1, "maximum": 200}}),
            &["review_id"],
        ),
        tool("sift_get_protocol", "The review's question, draft and locked protocol, options and search strings.", json!({"review_id": review_id()}), &["review_id"]),
        tool(
            "sift_get_manuscript",
            "The APA 7 manuscript content as JSON. The DOCX itself is exported from the Report screen.",
            json!({"review_id": review_id()}),
            &["review_id"],
        ),
    ]
}

fn ok(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn err(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

pub fn parse_error() -> Value {
    err(&Value::Null, -32700, "Parse error")
}

impl Server {
    pub fn new(bridge: Arc<Bridge>, app: Option<AppHandle>) -> Self {
        Self {
            bridge,
            app,
            client: Mutex::new(None),
        }
    }

    pub fn bridge(&self) -> &Arc<Bridge> {
        &self.bridge
    }

    /// Handles one JSON-RPC message. `None` means no reply is due (a
    /// notification, or a response from the client).
    pub async fn handle(&self, msg: Value) -> Option<Value> {
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // A response to something we never sent, or garbage.
            return msg
                .get("id")
                .filter(|_| msg.get("result").is_none() && msg.get("error").is_none())
                .map(|id| err(id, -32600, "Invalid Request"));
        };
        let id = msg.get("id")?.clone();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        Some(match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str);
                let version = asked
                    .filter(|v| PROTOCOL_VERSIONS.contains(v))
                    .unwrap_or(LATEST);
                if let Some(name) = params.pointer("/clientInfo/name").and_then(Value::as_str) {
                    *self.client.lock().unwrap_or_else(|p| p.into_inner()) = Some(name.to_owned());
                }
                ok(
                    &id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {"name": "sift", "title": crate::APP_NAME, "version": env!("CARGO_PKG_VERSION")},
                        "instructions": INSTRUCTIONS,
                    }),
                )
            }
            "ping" => ok(&id, json!({})),
            "tools/list" => ok(&id, json!({"tools": tools()})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                if !tools().iter().any(|t| t["name"] == name) {
                    return Some(err(&id, -32602, &format!("Unknown tool: {name}")));
                }
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let (text, is_error) = match self.call(name, &args).await {
                    Ok(t) => (t, false),
                    Err(ToolError(m)) => (m, true),
                };
                ok(
                    &id,
                    json!({"content": [{"type": "text", "text": text}], "isError": is_error}),
                )
            }
            other => err(&id, -32601, &format!("Method not found: {other}")),
        })
    }

    fn answered_by(&self, model: Option<&str>) -> String {
        let client = self
            .client
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .unwrap_or_else(|| "mcp-client".into());
        match model.map(str::trim).filter(|m| !m.is_empty()) {
            Some(m) => format!("{client} ({m})"),
            None => client,
        }
    }

    fn app(&self) -> Result<&AppHandle, ToolError> {
        self.app
            .as_ref()
            .ok_or_else(|| ToolError("the Sift app is not attached to this server".into()))
    }

    async fn call(&self, name: &str, args: &Value) -> ToolResult {
        match name {
            "sift_next_task" => {
                let secs = args
                    .get("wait_seconds")
                    .and_then(Value::as_u64)
                    .unwrap_or(30)
                    .clamp(1, 120);
                match self.bridge.next(Duration::from_secs(secs)).await {
                    Some(task) => pretty(&task),
                    None => Ok(format!(
                        "No task arrived in {secs} seconds. Call again to keep waiting, or check sift_status."
                    )),
                }
            }
            "sift_submit_task" => {
                let id = str_arg(args, "id")?;
                let content = args
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolError("missing argument: content".into()))?;
                let by = self.answered_by(args.get("model").and_then(Value::as_str));
                self.bridge
                    .submit(&id, content.to_owned(), by)
                    .map_err(|e| ToolError(e.to_string()))?;
                Ok("Answer accepted.".into())
            }
            "sift_bridge_status" => pretty(&self.bridge.status()),
            _ => self.control(name, args).await,
        }
    }

    async fn control(&self, name: &str, args: &Value) -> ToolResult {
        let app = self.app()?;
        let state = || app.state::<AppState>();
        match name {
            "sift_list_reviews" => pretty(&review_cmd::list_reviews(state()).await?),
            "sift_start_review" => {
                let question = str_arg(args, "question")?;
                let options: ReviewOptions = match args.get("options") {
                    Some(o) if !o.is_null() => serde_json::from_value(o.clone())?,
                    _ => ReviewOptions::default(),
                };
                let summary = cmd::start_review(state(), question, options).await?;
                if let Some(on) = args.get("bypass").and_then(Value::as_bool) {
                    cmd::set_bypass(state(), summary.id.clone(), on).await?;
                }
                pretty(&summary)
            }
            "sift_status" => {
                let id = str_arg(args, "review_id")?;
                let progress = cmd::review_progress(state(), id.clone()).await?;
                let prisma = cmd::get_prisma(state(), id.clone()).await?;
                let bypass = cmd::get_bypass(state(), id).await?;
                pretty(&json!({"progress": progress, "bypass": bypass, "prisma": prisma}))
            }
            "sift_pause" => {
                cmd::pause_review(state(), str_arg(args, "review_id")?).await?;
                Ok("Paused.".into())
            }
            "sift_resume" => {
                cmd::resume_review(state(), str_arg(args, "review_id")?).await?;
                Ok("Resumed.".into())
            }
            "sift_set_bypass" => {
                let on = bool_arg(args, "on")?;
                cmd::set_bypass(state(), str_arg(args, "review_id")?, on).await?;
                Ok(if on {
                    "Bypass mode is on. The review will not wait for a person and stays unverified."
                } else {
                    "Bypass mode is off."
                }
                .into())
            }
            "sift_skip_gate" => {
                let gate = args
                    .get("gate")
                    .and_then(Value::as_u64)
                    .and_then(|g| u8::try_from(g).ok())
                    .filter(|g| (1..=3).contains(g))
                    .ok_or_else(|| ToolError("gate must be 1, 2 or 3".into()))?;
                cmd::skip_gate(state(), str_arg(args, "review_id")?, gate).await?;
                Ok(format!("Gate {gate} skipped. The review stays unverified."))
            }
            "sift_skip_stage" => pretty(
                &cmd::skip_stage(state(), str_arg(args, "review_id")?, str_arg(args, "stage")?)
                    .await?,
            ),
            "sift_retry_failed" => {
                let n = cmd::retry_failed_jobs(state(), str_arg(args, "review_id")?).await?;
                Ok(format!("{n} failed jobs queued again."))
            }
            "sift_human_queue" => pretty(
                &cmd::human_queue(state(), str_arg(args, "review_id")?, str_arg(args, "stage")?)
                    .await?,
            ),
            "sift_decision_log" => {
                let limit = args
                    .get("limit")
                    .and_then(Value::as_i64)
                    .unwrap_or(30)
                    .clamp(1, 200);
                pretty(&cmd::decision_log(state(), str_arg(args, "review_id")?, limit, None).await?)
            }
            "sift_get_protocol" => {
                pretty(&cmd::get_protocol(state(), str_arg(args, "review_id")?).await?)
            }
            "sift_get_manuscript" => {
                pretty(&cmd::get_manuscript(state(), str_arg(args, "review_id")?).await?)
            }
            other => Err(ToolError(format!("Unknown tool: {other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> Server {
        Server::new(Bridge::new(), None)
    }

    fn call(id: i64, name: &str, args: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args}})
    }

    #[tokio::test]
    async fn initialize_negotiates_the_version_and_names_the_client() {
        let s = server();
        let r = s
            .handle(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "claude-code", "version": "2"}}}))
            .await
            .unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(r["result"]["capabilities"]["tools"]["listChanged"], false);
        assert_eq!(s.answered_by(Some("claude-opus-5-5")), "claude-code (claude-opus-5-5)");

        let r = s
            .handle(json!({"jsonrpc": "2.0", "id": 2, "method": "initialize", "params": {"protocolVersion": "1999-01-01"}}))
            .await
            .unwrap();
        assert_eq!(r["result"]["protocolVersion"], LATEST);
    }

    #[tokio::test]
    async fn notifications_and_client_responses_get_no_reply() {
        let s = server();
        assert!(s
            .handle(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await
            .is_none());
        assert!(s
            .handle(json!({"jsonrpc": "2.0", "id": 5, "result": {}}))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn unknown_methods_and_tools_are_protocol_errors() {
        let s = server();
        let r = s.handle(json!({"jsonrpc": "2.0", "id": 1, "method": "resources/list"})).await.unwrap();
        assert_eq!(r["error"]["code"], -32601);
        let r = s.handle(call(2, "sift_delete_everything", json!({}))).await.unwrap();
        assert_eq!(r["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn every_tool_has_a_schema_and_no_tool_decides_as_a_person() {
        let r = server().handle(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).await.unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        assert!(tools.len() >= 10);
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object");
            let name = t["name"].as_str().unwrap();
            assert!(name.starts_with("sift_"));
            for forbidden in ["decide", "judge", "sign_off", "resolve", "calibration"] {
                assert!(!name.contains(forbidden), "{name}");
            }
        }
    }

    #[tokio::test]
    async fn the_bridge_round_trip_runs_through_tool_calls() {
        let s = server();
        let ticket = s.bridge().post(Some("say hi".into()), vec![json!({"role": "user", "content": "hi"})], None);
        let r = s.handle(call(1, "sift_next_task", json!({"wait_seconds": 1}))).await.unwrap();
        assert_eq!(r["result"]["isError"], false);
        let task: Value = serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(task["id"], ticket.id());
        let r = s
            .handle(call(2, "sift_submit_task", json!({"id": task["id"], "content": "hello", "model": "gpt-5"})))
            .await
            .unwrap();
        assert_eq!(r["result"]["isError"], false);
        let answer = ticket.wait(Duration::from_millis(50)).await.unwrap();
        assert_eq!(answer.content, "hello");
        assert_eq!(answer.answered_by, "mcp-client (gpt-5)");
    }

    #[tokio::test]
    async fn tool_failures_are_results_the_model_can_read() {
        let s = server();
        let r = s.handle(call(1, "sift_submit_task", json!({"id": "t_missing", "content": "x"}))).await.unwrap();
        assert_eq!(r["result"]["isError"], true);
        let r = s.handle(call(2, "sift_status", json!({"review_id": "r"}))).await.unwrap();
        assert_eq!(r["result"]["isError"], true);
    }
}
