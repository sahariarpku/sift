//! Minimal HTTP/1.1 mock server for tests. No test in this crate calls a live
//! API (§13); providers and sources are pointed at this server instead.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Reply {
    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.into(),
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    /// Path including the query string.
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl Recorded {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

struct Route {
    method: Option<String>,
    path_prefix: String,
    replies: Vec<Reply>,
    handler: Option<Handler>,
}

/// Computes a reply from the request (for mocks that must answer by content).
pub type Handler = Arc<dyn Fn(&Recorded) -> Reply + Send + Sync>;

#[derive(Clone)]
pub struct MockServer {
    base: String,
    recorded: Arc<Mutex<Vec<Recorded>>>,
}

impl MockServer {
    /// Serves `replies` in order for any request; the last reply repeats.
    pub async fn start(replies: Vec<Reply>) -> Self {
        Self::routes(vec![(None, "/", replies)]).await
    }

    /// Answers every request with `handler`.
    pub async fn handler(handler: Handler) -> Self {
        Self::build(vec![Route {
            method: None,
            path_prefix: "/".into(),
            replies: Vec::new(),
            handler: Some(handler),
        }])
        .await
    }

    /// Routes by optional method and path prefix (first match wins). Each
    /// route serves its replies in order; the last one repeats.
    pub async fn routes(routes: Vec<(Option<&str>, &str, Vec<Reply>)>) -> Self {
        Self::build(
            routes
                .into_iter()
                .map(|(m, p, r)| Route {
                    method: m.map(str::to_owned),
                    path_prefix: p.to_owned(),
                    replies: r,
                    handler: None,
                })
                .collect(),
        )
        .await
    }

    async fn build(routes: Vec<Route>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let addr = listener.local_addr().expect("mock server address");
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let routes: Arc<Mutex<Vec<Route>>> = Arc::new(Mutex::new(routes));
        let rec = recorded.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (rec, routes) = (rec.clone(), routes.clone());
                tokio::spawn(async move {
                    let _ = handle(stream, rec, routes).await;
                });
            }
        });
        Self {
            base: format!("http://{addr}"),
            recorded,
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.recorded.lock().expect("recorded lock").clone()
    }
}

async fn handle(
    mut stream: TcpStream,
    recorded: Arc<Mutex<Vec<Recorded>>>,
    routes: Arc<Mutex<Vec<Route>>>,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    while buf.len() < header_end + len {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body =
        String::from_utf8_lossy(&buf[header_end..(header_end + len).min(buf.len())]).to_string();

    let rec = Recorded {
        method: method.clone(),
        path: path.clone(),
        headers,
        body,
    };
    let reply = {
        let mut routes = routes.lock().expect("routes lock");
        let route = routes.iter_mut().find(|r| {
            r.method.as_deref().is_none_or(|m| m == method) && path.starts_with(&r.path_prefix)
        });
        match route {
            Some(Route {
                handler: Some(h), ..
            }) => h(&rec),
            Some(r) if r.replies.len() > 1 => r.replies.remove(0),
            Some(r) => r
                .replies
                .first()
                .cloned()
                .unwrap_or_else(|| Reply::json(500, "{}")),
            None => Reply::json(404, r#"{"error":{"message":"no mock route"}}"#),
        }
    };
    recorded.lock().expect("recorded lock").push(rec);

    let mut out = format!("HTTP/1.1 {} Mock\r\n", reply.status);
    for (k, v) in &reply.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        reply.body.len()
    ));
    out.push_str(&reply.body);
    stream.write_all(out.as_bytes()).await?;
    stream.shutdown().await
}
