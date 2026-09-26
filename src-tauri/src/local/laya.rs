//! Optional local pre-screen with Laya-MLX.
//!
//! Source: https://github.com/mizorewww/laya-mlx (Apache-2.0), an MLX port of
//! Convai Innovations' Laya typed-decision models. It answers constrained
//! questions (`noul` = P(true)) with a 421M-parameter encoder on Apple
//! Silicon. It writes no text, so it cannot give a rationale or quote and
//! never decides a record on its own (§2, principle 3). Its scores only
//! change how much AI screening a record gets:
//! - speed: records ranked lowest get Screener A only; if A does not
//!   exclude, Screener B and the arbiter run as usual;
//! - safety net (both modes): records ranked highest that both screeners
//!   excluded go to the arbiter instead of being excluded.
//!
//! Sift does not bundle Python. It runs the user's own interpreter with
//! `laya-mlx` installed as a helper process speaking JSON lines, offline
//! (`HF_HUB_OFFLINE=1`), so weights must already be downloaded.
//! Measured on an 8 GB M1 (2026-09-25): about 0.45 s per question at the
//! 512-token context, so about 2 to 3 s per record. Scoring several records
//! in one forward pass gave identical scores but was about 5x slower there
//! (memory-bound), so records are scored one at a time; instead, scoring runs
//! side by side with the AI screeners (see `pipeline::advance`).
//!
//! While scoring is still under way a record is ranked against the scores so
//! far. Speed mode's single screening waits until `MIN_RANKED` records are
//! scored so an early rank is not taken from a handful of scores.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::agents::protocol::ProtocolBody;

pub const DEFAULT_MODEL: &str = "aac6fef/laya-mlx";
pub const PROMPT_VERSION: &str = "laya_prescreen.v1";
pub const STAGE: &str = "ta_prescreen";
pub const ACTOR: &str = "laya";
const SIDECAR: &str = include_str!("laya_sidecar.py");
const START_TIMEOUT: Duration = Duration::from_secs(180);
const ASK_TIMEOUT: Duration = Duration::from_secs(120);
/// Scores needed before speed mode single-screens while scoring continues.
pub const MIN_RANKED: i64 = 50;

#[derive(Debug, thiserror::Error)]
pub enum LayaError {
    #[error("{0}")]
    Unavailable(String),
    #[error("Laya helper: {0}")]
    Helper(String),
    #[error("Laya helper did not answer within {0} seconds")]
    Timeout(u64),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

type Result<T> = std::result::Result<T, LayaError>;

// ---------------------------------------------------------------------------
// Per-review choice

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Off,
    SafetyNet,
    Speed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Prescreen {
    pub mode: Mode,
    /// Speed mode: this share of records, ranked lowest, get one screener.
    pub speed_fraction: f64,
    /// This share of records, ranked highest, is sent to the arbiter when both screeners exclude.
    pub net_fraction: f64,
    pub model: String,
}

impl Default for Prescreen {
    fn default() -> Self {
        Self {
            mode: Mode::Off,
            speed_fraction: 0.3,
            net_fraction: 0.1,
            model: DEFAULT_MODEL.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Detection

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayaStatus {
    pub available: bool,
    pub python: Option<String>,
    pub version: Option<String>,
    pub model: String,
    pub model_downloaded: bool,
    pub message: String,
}

/// Interpreters to try. Apps started from Finder do not inherit the shell's
/// PATH, so common install locations are listed explicitly.
pub fn python_candidates(configured: Option<&str>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(p) = configured {
        out.push(PathBuf::from(p));
    }
    if let Some(home) = std::env::var_os("HOME") {
        out.push(Path::new(&home).join("homebrew/bin/python3"));
    }
    for p in [
        "/opt/homebrew/bin/python3",
        "/usr/local/bin/python3",
        "/usr/bin/python3",
    ] {
        out.push(PathBuf::from(p));
    }
    out
}

/// Where the Hugging Face cache keeps a model (respects `HF_HOME`).
pub fn model_cache_dir(model: &str) -> Option<PathBuf> {
    let hub = match std::env::var_os("HF_HOME") {
        Some(h) => PathBuf::from(h).join("hub"),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".cache/huggingface/hub"),
    };
    Some(hub.join(format!("models--{}", model.replace('/', "--"))))
}

pub async fn detect(configured: Option<&str>, model: &str) -> LayaStatus {
    let model_downloaded = model_cache_dir(model).is_some_and(|d| d.join("snapshots").is_dir());
    let mut status = LayaStatus {
        available: false,
        python: None,
        version: None,
        model: model.to_owned(),
        model_downloaded,
        message: String::new(),
    };
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        status.message = "Laya-MLX needs a Mac with Apple Silicon.".into();
        return status;
    }
    for py in python_candidates(configured) {
        if !py.is_file() {
            continue;
        }
        let probe = Command::new(&py)
            .args([
                "-c",
                "import laya_mlx; print(getattr(laya_mlx, '__version__', 'unknown'))",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output();
        let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(30), probe).await else {
            continue;
        };
        if out.status.success() {
            status.python = Some(py.display().to_string());
            status.version = Some(String::from_utf8_lossy(&out.stdout).trim().to_owned());
            break;
        }
    }
    status.available = status.python.is_some() && model_downloaded;
    status.message = match (&status.python, model_downloaded) {
        (None, _) => "No Python with laya-mlx found. Install it with: pip install laya-mlx".into(),
        (Some(_), false) => format!(
            "laya-mlx is installed but the model is not downloaded. Run once: hf download {model}"
        ),
        (Some(_), true) => "Ready. Runs offline on this Mac.".into(),
    };
    status
}

// ---------------------------------------------------------------------------
// Helper process

pub struct Laya {
    _child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    pub version: String,
    pub model: String,
}

impl Laya {
    /// Starts the helper with the user's Python. The script is written into `dir`.
    pub async fn start(python: &Path, dir: &Path, model: &str) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let script = dir.join("laya_sidecar.py");
        std::fs::write(&script, SIDECAR)?;
        let args = vec![
            "-u".to_owned(),
            script.display().to_string(),
            model.to_owned(),
        ];
        Self::start_command(&python.display().to_string(), &args, model).await
    }

    /// Starts any program speaking the helper protocol (tests use a stub).
    pub async fn start_command(program: &str, args: &[String], model: &str) -> Result<Self> {
        let mut child = Command::new(program)
            .args(args)
            .env("HF_HUB_OFFLINE", "1")
            .env("PYTHONWARNINGS", "ignore")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| LayaError::Helper("no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LayaError::Helper("no stdout".into()))?;
        let mut lines = BufReader::new(stdout).lines();
        let first = read_line(&mut lines, START_TIMEOUT).await?;
        if let Some(e) = first.get("error").and_then(Value::as_str) {
            return Err(LayaError::Unavailable(format!(
                "Laya could not load {model}: {e}"
            )));
        }
        if first.get("ready") != Some(&Value::Bool(true)) {
            return Err(LayaError::Helper(format!("unexpected first line: {first}")));
        }
        Ok(Self {
            _child: child,
            stdin,
            lines,
            version: first["version"].as_str().unwrap_or("unknown").to_owned(),
            model: model.to_owned(),
        })
    }

    /// Asks typed questions about one state; returns Laya's `answers` object.
    pub async fn ask(&mut self, state: &str, questions: &Value) -> Result<Value> {
        let mut line = serde_json::to_string(&json!({"state": state, "questions": questions}))
            .map_err(|e| LayaError::Helper(e.to_string()))?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        let reply = read_line(&mut self.lines, ASK_TIMEOUT).await?;
        if let Some(e) = reply.get("error").and_then(Value::as_str) {
            return Err(LayaError::Helper(e.to_owned()));
        }
        Ok(reply.get("answers").cloned().unwrap_or(Value::Null))
    }
}

async fn read_line(lines: &mut Lines<BufReader<ChildStdout>>, limit: Duration) -> Result<Value> {
    let next = tokio::time::timeout(limit, lines.next_line())
        .await
        .map_err(|_| LayaError::Timeout(limit.as_secs()))??;
    let text = next.ok_or_else(|| LayaError::Helper("the helper exited".into()))?;
    serde_json::from_str(&text).map_err(|e| LayaError::Helper(format!("bad reply ({e}): {text}")))
}

// ---------------------------------------------------------------------------
// Questions and scores

/// One `noul` question per inclusion criterion, keyed by criterion code.
pub fn questions(protocol: &ProtocolBody) -> std::result::Result<Value, String> {
    let prompt = crate::agents::prompt(PROMPT_VERSION).map_err(|e| e.to_string())?;
    let mut q = serde_json::Map::new();
    for c in &protocol.inclusion_criteria {
        q.insert(
            c.code.clone(),
            json!({
                "type": "noul",
                "instructions": crate::agents::render(&prompt.system, &[("criterion", c.text.clone())]),
            }),
        );
    }
    Ok(Value::Object(q))
}

pub fn state(title: &str, abstract_: &str) -> String {
    let user = crate::agents::prompt(PROMPT_VERSION)
        .map(|p| p.user)
        .unwrap_or_else(|_| "Title: {{title}}\nAbstract: {{abstract}}".into());
    crate::agents::render(
        &user,
        &[
            ("title", title.to_owned()),
            ("abstract", abstract_.to_owned()),
        ],
    )
}

/// Per-criterion P(met) and their geometric mean. `None` if no answers.
pub fn score(answers: &Value) -> Option<(f64, Vec<(String, f64)>)> {
    let map = answers.as_object()?;
    let mut parts: Vec<(String, f64)> = map
        .iter()
        .filter_map(|(code, a)| Some((code.clone(), a.get("noul")?.as_f64()?)))
        .collect();
    if parts.is_empty() {
        return None;
    }
    parts.sort_by(|a, b| a.0.cmp(&b.0));
    let log_sum: f64 = parts.iter().map(|(_, p)| p.clamp(1e-6, 1.0).ln()).sum();
    Some(((log_sum / parts.len() as f64).exp(), parts))
}

pub fn rationale(parts: &[(String, f64)]) -> String {
    let body = parts
        .iter()
        .map(|(c, p)| format!("{c} {p:.2}"))
        .collect::<Vec<_>>()
        .join("; ");
    format!("Local pre-screen, P(criterion met): {body}. Score is their geometric mean.")
}

/// Records still needing a Laya score, most recent search order.
pub fn unscored(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT s.id FROM v_screenable s
         WHERE NOT EXISTS (SELECT 1 FROM event e WHERE e.stage = ?1 AND e.target_id = s.id AND e.decision = 'score')
         ORDER BY s.rec_order",
    )?;
    let rows = stmt.query_map([STAGE], |r| r.get(0))?;
    rows.collect()
}

/// Still to score: screenable (not a duplicate, not from a skipped run) and unscored.
pub fn unscored_one(conn: &Connection, record_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM v_screenable WHERE id = ?2)
            AND NOT EXISTS (SELECT 1 FROM event WHERE stage = ?1 AND target_id = ?2 AND decision = 'score')",
        params![STAGE, record_id],
        |r| r.get(0),
    )
}

pub fn scored_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(DISTINCT target_id) FROM event WHERE stage = ?1 AND decision = 'score'",
        [STAGE],
        |r| r.get(0),
    )
}

// ---------------------------------------------------------------------------
// Routing

#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub score: f64,
    /// Share of scored records with a lower score (0 = lowest).
    pub percentile: f64,
    /// Screener A only unless A does not exclude.
    pub single: bool,
    /// Send to the arbiter when both screeners exclude.
    pub net: bool,
}

pub fn route(conn: &Connection, p: &Prescreen, record_id: &str) -> rusqlite::Result<Option<Route>> {
    if p.mode == Mode::Off {
        return Ok(None);
    }
    let score: Option<f64> = conn
        .query_row(
            "SELECT confidence FROM event WHERE stage = ?1 AND target_id = ?2 AND decision = 'score'
             ORDER BY id DESC LIMIT 1",
            params![STAGE, record_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let Some(score) = score else { return Ok(None) };
    let (lower, total): (i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(confidence < ?2), 0), COUNT(*) FROM (
            SELECT target_id, MAX(id), confidence FROM event
            WHERE stage = ?1 AND decision = 'score' GROUP BY target_id)",
        params![STAGE, score],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let percentile = if total > 0 {
        lower as f64 / total as f64
    } else {
        0.5
    };
    let mut r = decide(p, score, percentile);
    // A rank taken from a few early scores is not trusted to cut screening.
    if r.single && total < MIN_RANKED && !unscored(conn)?.is_empty() {
        r.single = false;
    }
    Ok(Some(r))
}

pub fn decide(p: &Prescreen, score: f64, percentile: f64) -> Route {
    Route {
        score,
        percentile,
        single: p.mode == Mode::Speed && percentile < p.speed_fraction,
        net: p.mode != Mode::Off && percentile >= 1.0 - p.net_fraction,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometric_mean_score_and_rationale() {
        let answers = json!({
            "IN-POP": {"type": "noul", "noul": 0.8},
            "IN-CONTEXT": {"type": "noul", "noul": 0.2},
        });
        let (s, parts) = score(&answers).unwrap();
        assert!((s - 0.4).abs() < 1e-9);
        assert_eq!(parts[0].0, "IN-CONTEXT");
        assert!(rationale(&parts).contains("IN-POP 0.80"));
        assert!(score(&json!({})).is_none());
    }

    #[test]
    fn routing_by_rank() {
        let speed = Prescreen {
            mode: Mode::Speed,
            ..Default::default()
        };
        assert!(decide(&speed, 0.1, 0.05).single);
        assert!(!decide(&speed, 0.5, 0.5).single);
        assert!(decide(&speed, 0.9, 0.95).net);
        let net = Prescreen {
            mode: Mode::SafetyNet,
            ..Default::default()
        };
        assert!(
            !decide(&net, 0.1, 0.05).single,
            "safety net never single-screens"
        );
        assert!(decide(&net, 0.9, 0.95).net);
        assert!(!decide(&Prescreen::default(), 0.9, 0.99).net);
    }

    #[test]
    fn questions_come_from_inclusion_criteria() {
        let p = crate::agents::protocol::tests::sample_protocol();
        let q = questions(&p).unwrap();
        let obj = q.as_object().unwrap();
        assert_eq!(obj.len(), p.inclusion_criteria.len());
        let first = &p.inclusion_criteria[0];
        assert_eq!(obj[&first.code]["type"], "noul");
        assert!(obj[&first.code]["instructions"]
            .as_str()
            .unwrap()
            .contains(&first.text));
        assert!(state("T", "A").contains("Title: T"));
    }

    #[test]
    fn route_ranks_records_against_all_scores_and_respects_mode() {
        use crate::events::{self, ActorType, NewEvent};
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let review = crate::reviews::insert_review(&conn, "Q?").unwrap();
        for i in 0..10 {
            events::append(
                &conn,
                &NewEvent {
                    review_id: review.clone(),
                    target_type: "record".into(),
                    target_id: format!("r{i}"),
                    stage: STAGE.into(),
                    actor_type: Some(ActorType::Agent),
                    actor_id: ACTOR.into(),
                    decision: "score".into(),
                    confidence: Some(f64::from(i) / 10.0),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let speed = Prescreen {
            mode: Mode::Speed,
            ..Default::default()
        };
        let low = route(&conn, &speed, "r1").unwrap().unwrap();
        assert_eq!((low.percentile, low.single, low.net), (0.1, true, false));
        let mid = route(&conn, &speed, "r5").unwrap().unwrap();
        assert!(!mid.single && !mid.net);
        let top = route(&conn, &speed, "r9").unwrap().unwrap();
        assert!(top.net && !top.single);
        assert!(route(&conn, &speed, "unscored").unwrap().is_none());
        assert!(route(&conn, &Prescreen::default(), "r1").unwrap().is_none());
    }

    #[test]
    fn speed_mode_waits_for_enough_scores_while_scoring_continues() {
        use crate::events::{self, ActorType, NewEvent};
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let review = crate::reviews::insert_review(&conn, "Q?").unwrap();
        conn.execute(
            "INSERT INTO search_run (id, review_id, source, query_string, query_ast, run_at, hits) VALUES ('s', ?1, 'openalex', 'q', '{}', 'now', 11)",
            [&review],
        )
        .unwrap();
        for i in 0..11 {
            conn.execute(
                "INSERT INTO record (id, search_run_id, title) VALUES (?1, 's', 't')",
                [format!("r{i}")],
            )
            .unwrap();
        }
        let score = |i: i32| {
            events::append(
                &conn,
                &NewEvent {
                    review_id: review.clone(),
                    target_type: "record".into(),
                    target_id: format!("r{i}"),
                    stage: STAGE.into(),
                    actor_type: Some(ActorType::Agent),
                    actor_id: ACTOR.into(),
                    decision: "score".into(),
                    confidence: Some(f64::from(i) / 10.0),
                    ..Default::default()
                },
            )
            .unwrap();
        };
        for i in 0..10 {
            score(i);
        }
        let speed = Prescreen {
            mode: Mode::Speed,
            ..Default::default()
        };
        // r10 is unscored and only 10 of MIN_RANKED scores exist: both screeners run.
        let early = route(&conn, &speed, "r1").unwrap().unwrap();
        assert!(!early.single);
        assert!(
            route(&conn, &speed, "r9").unwrap().unwrap().net,
            "the safety net applies at once"
        );
        // Once every record is scored the rank is final, however few there are.
        score(10);
        assert!(route(&conn, &speed, "r1").unwrap().unwrap().single);
    }

    #[tokio::test]
    async fn helper_protocol_round_trip_with_a_stub() {
        let script = r#"echo '{"ready":true,"version":"stub"}'; while read l; do echo '{"answers":{"IN-POP":{"noul":0.7}}}'; done"#;
        let mut laya = Laya::start_command("/bin/sh", &["-c".into(), script.into()], "stub/model")
            .await
            .unwrap();
        assert_eq!(laya.version, "stub");
        let a = laya.ask("Title: x", &json!({})).await.unwrap();
        assert_eq!(a["IN-POP"]["noul"], 0.7);
    }

    #[tokio::test]
    async fn helper_load_error_is_reported() {
        let script = r#"echo '{"error":"ModuleNotFoundError: laya_mlx"}'"#;
        let err = Laya::start_command("/bin/sh", &["-c".into(), script.into()], "m")
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("ModuleNotFoundError"));
    }
}
