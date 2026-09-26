//! `sift --mcp-stdio`: relays the MCP stdio transport to the running app's
//! HTTP endpoint, for clients that only launch local commands.
//!
//! Per https://modelcontextprotocol.io/specification/2025-11-25/basic/transports
//! messages are newline-delimited JSON-RPC on stdin and stdout; nothing else
//! may be written to stdout, so diagnostics go to stderr.

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::{db, settings};

fn error_reply(id: &Value, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": message}})
}

async fn relay() -> Result<(), String> {
    let dir = super::app_data_dir().map_err(|e| e.to_string())?;
    let (port, token) = {
        let conn = db::open_app(&dir.join("app.db")).map_err(|e| e.to_string())?;
        let s = settings::load(&conn).map_err(|e| e.to_string())?;
        (s.mcp_port, super::token(&conn).map_err(|e| e.to_string())?)
    };
    let url = format!("http://127.0.0.1:{port}/mcp");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|e| e.to_string())?;
    let mut version: Option<String> = None;
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                let reply = super::rpc::parse_error();
                out.write_all(format!("{reply}\n").as_bytes()).await.map_err(|e| e.to_string())?;
                out.flush().await.map_err(|e| e.to_string())?;
                continue;
            }
        };
        let id = msg.get("id").cloned();
        let is_request = msg.get("method").is_some() && id.is_some();
        let mut req = client
            .post(&url)
            .bearer_auth(&token)
            .header("Accept", "application/json, text/event-stream")
            .json(&msg);
        if let Some(v) = &version {
            req = req.header("MCP-Protocol-Version", v);
        }
        let reply: Option<Value> = match req.send().await {
            Ok(r) if r.status().as_u16() == 202 => None,
            Ok(r) => match r.json::<Value>().await {
                Ok(v) => Some(v),
                Err(e) => id.as_ref().map(|i| error_reply(i, &format!("bad reply from Sift: {e}"))),
            },
            Err(_) => id.as_ref().filter(|_| is_request).map(|i| {
                error_reply(
                    i,
                    "Sift is not running or its MCP server is off. Open Sift, go to Settings, and turn on the MCP server.",
                )
            }),
        };
        if let Some(reply) = reply {
            if msg.get("method").and_then(Value::as_str) == Some("initialize") {
                version = reply
                    .pointer("/result/protocolVersion")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            out.write_all(format!("{reply}\n").as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            out.flush().await.map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Runs the relay until stdin closes. Returns the process exit code.
pub fn run() -> i32 {
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("sift --mcp-stdio: {e}");
            return 1;
        }
    };
    match rt.block_on(relay()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("sift --mcp-stdio: {e}");
            1
        }
    }
}
