//! MCP server: lets an assistant such as Claude or ChatGPT operate Sift and
//! stand in for an AI provider.
//!
//! - `/mcp` speaks the MCP Streamable HTTP transport (see `http`), with tools
//!   to start, watch and steer reviews (see `rpc`).
//! - `/bridge/v1` is an OpenAI-compatible endpoint the app itself calls when
//!   a task is assigned to the "Claude or ChatGPT (via MCP)" provider. Each
//!   call waits in the `bridge` until the assistant answers it.
//! - `sift --mcp-stdio` relays stdio to the running app for clients that
//!   only launch local commands (see `stdio`).
//!
//! The server binds 127.0.0.1 only and every route needs the bearer token
//! kept in `app.db` (not the keychain: it is a local capability the user
//! pastes into the assistant's config, and reading it must not prompt).

pub mod bridge;
pub mod http;
pub mod rpc;
pub mod stdio;

use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tokio::sync::watch;

use crate::error::{Error, Result};
use crate::settings;
use crate::state::AppState;
use bridge::{Bridge, BridgeStatus};

/// The single model the bridge lists. The model that actually answered is
/// reported per call in `X-Routed-Via` and disclosed.
pub const BRIDGE_MODEL: &str = "mcp-assistant";
const TOKEN_KEY: &str = "mcp_token";

/// True for the app's own bridge endpoint (any port).
pub fn is_bridge_url(base_url: &str) -> bool {
    let u = base_url.trim().trim_end_matches('/');
    (u.starts_with("http://localhost:") || u.starts_with("http://127.0.0.1:"))
        && u.ends_with("/bridge/v1")
}

fn new_token() -> String {
    // uuid v7 fills its low 62 bits with fresh randomness from the OS RNG
    // (a counter may take some of rand_a, never rand_b); four give ~248 bits.
    let part = || {
        let s = uuid::Uuid::now_v7().simple().to_string();
        s[16..].to_owned()
    };
    format!("sift_{}{}{}{}", part(), part(), part(), part())
}

/// The bearer token, created on first use.
pub fn token(app: &Connection) -> Result<String> {
    let existing: Option<String> = app
        .query_row("SELECT value FROM setting WHERE key = ?1", [TOKEN_KEY], |r| {
            r.get(0)
        })
        .optional()?;
    if let Some(t) = existing.filter(|t| !t.is_empty()) {
        return Ok(t);
    }
    regenerate_token(app)
}

pub fn regenerate_token(app: &Connection) -> Result<String> {
    let t = new_token();
    app.execute(
        "INSERT INTO setting (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![TOKEN_KEY, t],
    )?;
    Ok(t)
}

/// The server as it is running now.
pub struct Running {
    port: u16,
    token: String,
    stop: watch::Sender<bool>,
}

#[derive(Default)]
pub struct McpRuntime {
    pub bridge: Arc<Bridge>,
    running: std::sync::Mutex<Option<Running>>,
    last_error: std::sync::Mutex<Option<String>>,
}

impl McpRuntime {
    fn stop(&self) {
        if let Some(r) = self
            .running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = r.stop.send(true);
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    pub enabled: bool,
    pub running: bool,
    pub port: u16,
    pub url: String,
    pub bridge_url: String,
    pub token: String,
    /// Command for clients that launch a local process (Claude Desktop).
    pub stdio_command: Option<String>,
    pub error: Option<String>,
    pub bridge: BridgeStatus,
}

/// Starts, restarts or stops the server to match the settings.
pub fn sync(app: &AppHandle) -> Result<McpStatus> {
    let state = app.state::<AppState>();
    let (s, token) = {
        let db = state.app_db()?;
        (settings::load(&db)?, token(&db)?)
    };
    let rt = &state.mcp;
    let current = rt
        .running
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|r| (r.port, r.token.clone()));
    let wanted = s.mcp_enabled.then(|| (s.mcp_port, token.clone()));
    if current != wanted {
        rt.stop();
        *rt.last_error.lock().unwrap_or_else(|p| p.into_inner()) = None;
        if let Some((port, token)) = wanted {
            match std::net::TcpListener::bind(("127.0.0.1", port))
                .and_then(|l| l.set_nonblocking(true).map(|_| l))
            {
                Ok(listener) => {
                    let (tx, rx) = watch::channel(false);
                    let ctx = Arc::new(http::Ctx {
                        server: rpc::Server::new(rt.bridge.clone(), Some(app.clone())),
                        token: token.clone(),
                        port,
                    });
                    tauri::async_runtime::spawn(async move {
                        match tokio::net::TcpListener::from_std(listener) {
                            Ok(l) => http::serve(l, ctx, rx).await,
                            Err(e) => eprintln!("MCP server could not start: {e}"),
                        }
                    });
                    *rt.running.lock().unwrap_or_else(|p| p.into_inner()) = Some(Running {
                        port,
                        token,
                        stop: tx,
                    });
                }
                Err(e) => {
                    *rt.last_error.lock().unwrap_or_else(|p| p.into_inner()) = Some(format!(
                        "Port {port} is not available ({e}). Choose another port."
                    ));
                }
            }
        }
    }
    status(app)
}

pub fn status(app: &AppHandle) -> Result<McpStatus> {
    let state = app.state::<AppState>();
    let (s, token) = {
        let db = state.app_db()?;
        (settings::load(&db)?, token(&db)?)
    };
    let rt = &state.mcp;
    let running = rt
        .running
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_some();
    let port = s.mcp_port;
    let error = rt
        .last_error
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let status = McpStatus {
        enabled: s.mcp_enabled,
        running,
        port,
        url: format!("http://127.0.0.1:{port}/mcp"),
        bridge_url: format!("http://localhost:{port}/bridge/v1"),
        token,
        stdio_command: std::env::current_exe()
            .ok()
            .map(|p| format!("{} --mcp-stdio", p.display())),
        error,
        bridge: rt.bridge.status(),
    };
    Ok(status)
}

pub fn regenerate(app: &AppHandle) -> Result<McpStatus> {
    regenerate_token(&*app.state::<AppState>().app_db()?)?;
    sync(app)
}

/// The directory Tauri uses as `app_data_dir` for this identifier, found
/// without a running app (for `--mcp-stdio`). Tauri joins the platform data
/// directory with the bundle identifier.
pub fn app_data_dir() -> Result<std::path::PathBuf> {
    const IDENTIFIER: &str = "org.sift.desktop";
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    let base = if cfg!(target_os = "macos") {
        env("HOME").map(|h| std::path::PathBuf::from(h).join("Library/Application Support"))
    } else if cfg!(target_os = "windows") {
        env("APPDATA").map(std::path::PathBuf::from)
    } else {
        env("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| env("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share")))
    };
    base.map(|b| b.join(IDENTIFIER))
        .ok_or_else(|| Error::Invalid("could not find the app data directory".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn bridge_urls_are_recognised() {
        assert!(is_bridge_url("http://localhost:31480/bridge/v1"));
        assert!(is_bridge_url("http://127.0.0.1:9000/bridge/v1/"));
        assert!(!is_bridge_url("https://api.openai.com/v1"));
        assert!(!is_bridge_url("http://localhost:11434/v1"));
        assert!(!is_bridge_url("http://evil.example:31480/bridge/v1"));
    }

    #[test]
    fn the_token_is_stable_until_regenerated() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open_app(&dir.path().join("app.db")).unwrap();
        let a = token(&conn).unwrap();
        assert_eq!(a, token(&conn).unwrap());
        assert!(a.starts_with("sift_") && a.len() == 5 + 64);
        let b = regenerate_token(&conn).unwrap();
        assert_ne!(a, b);
        assert_eq!(b, token(&conn).unwrap());
    }

    #[test]
    fn identifier_matches_tauri_config() {
        let conf = include_str!("../../tauri.conf.json");
        assert!(conf.contains(r#""identifier": "org.sift.desktop""#));
        assert!(app_data_dir().is_ok());
    }
}
