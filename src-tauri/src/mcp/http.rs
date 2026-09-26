//! A minimal HTTP/1.1 server on 127.0.0.1 for the MCP endpoint and the AI
//! bridge. One request per connection (`Connection: close`), bodies by
//! `Content-Length` only. Hand-rolled to avoid a new dependency; it only
//! ever serves local clients.
//!
//! Streamable HTTP transport, per
//! https://modelcontextprotocol.io/specification/2025-11-25/basic/transports:
//! - `POST /mcp` carries one JSON-RPC message. A request gets
//!   `application/json` with one response; a notification or response gets
//!   202 with no body.
//! - `GET /mcp` gets 405: this server opens no SSE stream.
//! - An `Origin` header that is not this server's own gets 403 (DNS
//!   rebinding), and so does a `Host` that is not loopback.
//! - An unsupported `MCP-Protocol-Version` header gets 400.
//!
//! Bridge routes follow the OpenAI chat completions shape the app's
//! `openai_compat` adapter speaks:
//! https://platform.openai.com/docs/api-reference/chat/create
//! - `GET /bridge/v1/models`
//! - `POST /bridge/v1/chat/completions`
//!
//! Every route needs `Authorization: Bearer <token>`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use super::rpc::{self, Server};
use super::BRIDGE_MODEL;

/// Full texts go through the bridge, so allow large bodies.
const MAX_BODY: usize = 32 * 1024 * 1024;
const MAX_HEAD: usize = 64 * 1024;
/// How long a model call waits for an assistant. The app's HTTP timeout for
/// bridge providers is longer, so this answer (504) arrives first.
pub const BRIDGE_WAIT: Duration = Duration::from_secs(900);
/// How long a connection test waits for an assistant.
const TEST_WAIT: Duration = Duration::from_secs(60);

pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, PartialEq)]
pub struct Response {
    pub status: u16,
    pub content_type: Option<&'static str>,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

impl Response {
    fn json(status: u16, v: &Value) -> Self {
        Self {
            status,
            content_type: Some("application/json"),
            headers: vec![],
            body: v.to_string().into_bytes(),
        }
    }

    fn text(status: u16, msg: &str) -> Self {
        Self {
            status,
            content_type: Some("text/plain; charset=utf-8"),
            headers: vec![],
            body: msg.as_bytes().to_vec(),
        }
    }

    fn empty(status: u16) -> Self {
        Self {
            status,
            content_type: None,
            headers: vec![],
            body: vec![],
        }
    }

    /// OpenAI-style error body, so the adapter reports the message.
    fn api_error(status: u16, msg: &str) -> Self {
        Self::json(status, &json!({"error": {"message": msg, "type": "sift_bridge"}}))
    }

    fn with_header(mut self, k: &'static str, v: String) -> Self {
        self.headers.push((k, v));
        self
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        504 => "Gateway Timeout",
        _ => "Error",
    }
}

pub struct Ctx {
    pub server: Server,
    pub token: String,
    pub port: u16,
}

fn loopback_host(host: &str, port: u16) -> bool {
    ["127.0.0.1", "localhost", "[::1]"]
        .iter()
        .any(|h| host.eq_ignore_ascii_case(&format!("{h}:{port}")) || host.eq_ignore_ascii_case(h))
}

fn authorised(req: &Request, token: &str) -> bool {
    req.header("authorization")
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .is_some_and(|t| constant_time_eq(t.trim().as_bytes(), token.as_bytes()))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Routes one parsed request.
pub async fn route(ctx: &Ctx, req: Request) -> Response {
    if !req
        .header("host")
        .is_some_and(|h| loopback_host(h.trim(), ctx.port))
    {
        return Response::text(403, "Host must be 127.0.0.1 or localhost.");
    }
    if let Some(origin) = req.header("origin") {
        let own = [
            format!("http://127.0.0.1:{}", ctx.port),
            format!("http://localhost:{}", ctx.port),
        ];
        if !own.iter().any(|o| o.eq_ignore_ascii_case(origin.trim())) {
            return Response::text(403, "Requests from web pages are not accepted.");
        }
    }
    if !authorised(&req, &ctx.token) {
        return Response::text(401, "Missing or wrong bearer token. Copy it from Sift's Settings.")
            .with_header("WWW-Authenticate", "Bearer".into());
    }
    let path = req.path.split('?').next().unwrap_or("");
    match (req.method.as_str(), path) {
        ("POST", "/mcp") => mcp_post(ctx, &req).await,
        (_, "/mcp") => Response::text(405, "This server does not open an SSE stream.")
            .with_header("Allow", "POST".into()),
        ("GET", "/bridge/v1/models") => Response::json(
            200,
            &json!({"object": "list", "data": [{"id": BRIDGE_MODEL, "object": "model", "owned_by": "sift"}]}),
        ),
        ("POST", "/bridge/v1/chat/completions") => chat(ctx, &req).await,
        _ => Response::text(404, "Not found."),
    }
}

async fn mcp_post(ctx: &Ctx, req: &Request) -> Response {
    if let Some(v) = req.header("mcp-protocol-version") {
        if !rpc::PROTOCOL_VERSIONS.contains(&v.trim()) {
            return Response::text(400, "Unsupported MCP-Protocol-Version.");
        }
    }
    let msg: Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(_) => return Response::json(400, &rpc::parse_error()),
    };
    if msg.is_array() {
        // Batches were removed from the protocol in 2025-06-18.
        return Response::json(
            400,
            &json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": "Batches are not supported"}}),
        );
    }
    match ctx.server.handle(msg).await {
        Some(reply) => Response::json(200, &reply),
        None => Response::empty(202),
    }
}

/// Splits an OpenAI chat request into what the assistant needs to see.
fn task_parts(body: &Value) -> (Option<String>, Vec<Value>, Option<(String, Value)>) {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for m in body["messages"].as_array().into_iter().flatten() {
        let role = m["role"].as_str().unwrap_or("user");
        let content = match &m["content"] {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if role == "system" || role == "developer" {
            system.push(content);
        } else {
            messages.push(json!({"role": role, "content": content}));
        }
    }
    let schema = body
        .pointer("/response_format/json_schema")
        .and_then(|js| {
            Some((
                js["name"].as_str().unwrap_or("answer").to_owned(),
                js.get("schema")?.clone(),
            ))
        });
    let system = (!system.is_empty()).then(|| system.join("\n\n"));
    (system, messages, schema)
}

async fn chat(ctx: &Ctx, req: &Request) -> Response {
    let body: Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(e) => return Response::api_error(400, &format!("invalid JSON: {e}")),
    };
    let model = body["model"].as_str().unwrap_or(BRIDGE_MODEL).to_owned();
    let (system, messages, schema) = task_parts(&body);
    // The Settings "Test connection" button should not hang for 15 minutes.
    let test_prompt = crate::llm::CONNECTION_TEST_PROMPT.trim();
    let is_test = messages
        .iter()
        .any(|m| m["content"].as_str().map(str::trim) == Some(test_prompt));
    let limit = if is_test { TEST_WAIT } else { BRIDGE_WAIT };
    let ticket = ctx.server.bridge().post(system, messages, schema);
    let id = ticket.id().to_owned();
    match ticket.wait(limit).await {
        Some(answer) => Response::json(
            200,
            &json!({
                "id": id,
                "object": "chat.completion",
                "created": crate::time::now_ms() / 1000,
                "model": model,
                "choices": [{"index": 0, "finish_reason": "stop",
                             "message": {"role": "assistant", "content": answer.content}}],
            }),
        )
        // Read by the adapter as the model that actually answered.
        .with_header("X-Routed-Via", answer.answered_by),
        None if is_test => Response::api_error(
            504,
            "No assistant is connected. Ask Claude or ChatGPT to call sift_next_task, then test again.",
        ),
        None => Response::api_error(
            504,
            "No assistant answered within 15 minutes. Ask Claude or ChatGPT to call sift_next_task, then retry the failed jobs.",
        ),
    }
}

async fn read_request(stream: &mut BufReader<TcpStream>) -> Result<Option<Request>, Response> {
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let n = stream
            .read_until(b'\n', &mut line)
            .await
            .map_err(|_| Response::text(400, "Bad request."))?;
        if n == 0 {
            return Ok(None);
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        head.extend_from_slice(&line);
        if head.len() > MAX_HEAD {
            return Err(Response::text(413, "Headers too large."));
        }
    }
    let head = String::from_utf8(head).map_err(|_| Response::text(400, "Bad request."))?;
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let (method, path) = match (first.next(), first.next()) {
        (Some(m), Some(p)) => (m.to_owned(), p.to_owned()),
        _ => return Err(Response::text(400, "Bad request.")),
    };
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    let find = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    let length = match find("content-length") {
        Some(v) => v
            .parse::<usize>()
            .map_err(|_| Response::text(400, "Bad Content-Length."))?,
        None if find("transfer-encoding").is_some() => {
            return Err(Response::text(411, "Send a Content-Length."));
        }
        None => 0,
    };
    if length > MAX_BODY {
        return Err(Response::text(413, "Body too large."));
    }
    let mut body = vec![0; length];
    stream
        .read_exact(&mut body)
        .await
        .map_err(|_| Response::text(400, "Body shorter than Content-Length."))?;
    Ok(Some(Request {
        method,
        path,
        headers,
        body,
    }))
}

async fn write_response(stream: &mut BufReader<TcpStream>, r: &Response) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n",
        r.status,
        reason(r.status),
        r.body.len()
    );
    if let Some(ct) = r.content_type {
        head.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    for (k, v) in &r.headers {
        // Header values come from our own code; strip line breaks regardless.
        head.push_str(&format!("{k}: {}\r\n", v.replace(['\r', '\n'], " ")));
    }
    head.push_str("\r\n");
    let s = stream.get_mut();
    s.write_all(head.as_bytes()).await?;
    s.write_all(&r.body).await?;
    s.flush().await
}

/// Resolves when the client closes its side of the connection. Clients send
/// one request per connection, so anything after it is read and ignored.
async fn hung_up(stream: &mut BufReader<TcpStream>) {
    let mut scratch = [0u8; 1024];
    while let Ok(n) = stream.read(&mut scratch).await {
        if n == 0 {
            return;
        }
    }
}

async fn connection(ctx: Arc<Ctx>, stream: TcpStream) {
    let mut stream = BufReader::new(stream);
    let response = match read_request(&mut stream).await {
        // A caller that gives up drops the route future, so a model call it
        // was waiting on is withdrawn from the bridge (`Ticket::drop`) and a
        // `sift_next_task` long poll stops before it can claim a task.
        Ok(Some(req)) => tokio::select! {
            r = route(&ctx, req) => r,
            () = hung_up(&mut stream) => return,
        },
        Ok(None) => return,
        Err(r) => r,
    };
    let _ = write_response(&mut stream, &response).await;
}

/// Serves until `stop` turns true.
pub async fn serve(listener: TcpListener, ctx: Arc<Ctx>, mut stop: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                if let Ok((stream, peer)) = accepted {
                    if peer.ip().is_loopback() {
                        tokio::spawn(connection(ctx.clone(), stream));
                    }
                }
            }
            _ = stop.changed() => {
                if *stop.borrow() { return; }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::bridge::Bridge;

    fn ctx() -> Ctx {
        Ctx {
            server: Server::new(Bridge::new(), None),
            token: "secret-token".into(),
            port: 31480,
        }
    }

    fn req(method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Request {
        let mut h: Vec<(String, String)> = vec![
            ("Host".into(), "127.0.0.1:31480".into()),
            ("Authorization".into(), "Bearer secret-token".into()),
        ];
        for (k, v) in headers {
            h.retain(|(x, _)| !x.eq_ignore_ascii_case(k));
            h.push(((*k).into(), (*v).into()));
        }
        Request {
            method: method.into(),
            path: path.into(),
            headers: h,
            body: body.as_bytes().to_vec(),
        }
    }

    fn body(r: &Response) -> Value {
        serde_json::from_slice(&r.body).unwrap()
    }

    #[tokio::test]
    async fn requests_get_json_and_notifications_get_202() {
        let c = ctx();
        let r = route(&c, req("POST", "/mcp", &[], r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)).await;
        assert_eq!(r.status, 200);
        assert_eq!(r.content_type, Some("application/json"));
        assert_eq!(body(&r)["result"], json!({}));
        let r = route(&c, req("POST", "/mcp", &[], r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)).await;
        assert_eq!(r.status, 202);
        assert!(r.body.is_empty());
    }

    #[tokio::test]
    async fn guards_reject_foreign_origins_hosts_tokens_and_versions() {
        let c = ctx();
        let ping = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let status = |r: Response| r.status;
        assert_eq!(status(route(&c, req("POST", "/mcp", &[("Origin", "https://evil.example")], ping)).await), 403);
        assert_eq!(status(route(&c, req("POST", "/mcp", &[("Host", "evil.example:31480")], ping)).await), 403);
        assert_eq!(status(route(&c, req("POST", "/mcp", &[("Authorization", "Bearer wrong")], ping)).await), 401);
        assert_eq!(status(route(&c, req("POST", "/mcp", &[("MCP-Protocol-Version", "2024-01-01")], ping)).await), 400);
        assert_eq!(status(route(&c, req("POST", "/mcp", &[("MCP-Protocol-Version", "2025-11-25")], ping)).await), 200);
        assert_eq!(status(route(&c, req("GET", "/mcp", &[], "")).await), 405);
        assert_eq!(status(route(&c, req("POST", "/mcp", &[], "not json")).await), 400);
        assert_eq!(status(route(&c, req("POST", "/mcp", &[], "[]")).await), 400);
    }

    #[tokio::test]
    async fn the_bridge_lists_one_model_and_parks_chat_calls_for_the_assistant() {
        let c = Arc::new(ctx());
        let r = route(&c, req("GET", "/bridge/v1/models", &[], "")).await;
        assert_eq!(body(&r)["data"][0]["id"], BRIDGE_MODEL);

        let chat_body = json!({
            "model": BRIDGE_MODEL,
            "messages": [{"role": "system", "content": "screen it"}, {"role": "user", "content": "record 1"}],
            "response_format": {"type": "json_schema", "json_schema": {"name": "screen", "strict": true,
                "schema": {"type": "object", "properties": {"d": {"type": "string"}}, "required": ["d"], "additionalProperties": false}}},
        })
        .to_string();
        let c2 = c.clone();
        let waiting = tokio::spawn(async move {
            route(&c2, req("POST", "/bridge/v1/chat/completions", &[], &chat_body)).await
        });
        let task = c.server.bridge().next(Duration::from_secs(5)).await.unwrap();
        assert_eq!(task.system.as_deref(), Some("screen it"));
        assert_eq!(task.messages, vec![json!({"role": "user", "content": "record 1"})]);
        assert_eq!(task.schema_name.as_deref(), Some("screen"));
        c.server
            .bridge()
            .submit(&task.id, r#"{"d":"include"}"#.into(), "claude-code (claude-opus-5-5)".into())
            .unwrap();
        let r = waiting.await.unwrap();
        assert_eq!(r.status, 200);
        let v = body(&r);
        assert_eq!(v["choices"][0]["message"]["content"], r#"{"d":"include"}"#);
        assert_eq!(v["model"], BRIDGE_MODEL);
        assert!(r
            .headers
            .contains(&("X-Routed-Via", "claude-code (claude-opus-5-5)".into())));
    }

    #[tokio::test]
    async fn a_real_socket_round_trip_works() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let c = Arc::new(Ctx {
            server: Server::new(Bridge::new(), None),
            token: "tok".into(),
            port,
        });
        let (tx, rx) = watch::channel(false);
        let handle = tokio::spawn(serve(listener, c, rx));
        let r = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/mcp"))
            .bearer_auth("tok")
            .header("Accept", "application/json, text/event-stream")
            .json(&json!({"jsonrpc": "2.0", "id": 7, "method": "tools/list"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let v: Value = r.json().await.unwrap();
        assert_eq!(v["id"], 7);
        assert!(v["result"]["tools"].as_array().unwrap().len() > 5);
        tx.send(true).unwrap();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn a_model_call_whose_caller_hangs_up_leaves_the_bridge() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let c = Arc::new(Ctx {
            server: Server::new(Bridge::new(), None),
            token: "tok".into(),
            port,
        });
        let (tx, rx) = watch::channel(false);
        let handle = tokio::spawn(serve(listener, c.clone(), rx));
        let call = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/bridge/v1/chat/completions"))
            .bearer_auth("tok")
            .json(&json!({"model": BRIDGE_MODEL, "messages": [{"role": "user", "content": "hi"}]}))
            .timeout(Duration::from_millis(300))
            .send()
            .await;
        assert!(call.unwrap_err().is_timeout());
        let mut waiting = c.server.bridge().status().waiting;
        for _ in 0..50 {
            if waiting == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            waiting = c.server.bridge().status().waiting;
        }
        assert_eq!(waiting, 0, "the abandoned call is still queued");
        assert!(c.server.bridge().next(Duration::from_millis(20)).await.is_none());
        tx.send(true).unwrap();
        handle.await.unwrap();
    }
}
