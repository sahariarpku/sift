//! Persistent job queue stored in each review file's `job` table.
//!
//! Lifecycle: `queued -> running -> done`, or back to `queued` on a retryable
//! failure, `failed` after `MAX_ATTEMPTS`, `paused` when no handler exists or
//! a later phase pauses it (strict mode, spending cap). A job left `running`
//! by a crash, sleep or quit is returned to `queued` by [`recover`] when the
//! worker starts, so work always resumes.

mod worker;

pub use worker::{JobContext, JobError, JobHandler, NoopHandler, Orchestrator, Registry, Worker};

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::time::now_iso;

pub const MAX_ATTEMPTS: i64 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Failed,
    Paused,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
            JobStatus::Paused => "paused",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "queued" => JobStatus::Queued,
            "running" => JobStatus::Running,
            "done" => JobStatus::Done,
            "failed" => JobStatus::Failed,
            "paused" => JobStatus::Paused,
            other => return Err(Error::Invalid(format!("unknown job status {other:?}"))),
        })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: i64,
    pub review_id: String,
    pub kind: String,
    pub payload: serde_json::Value,
    pub status: JobStatus,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub updated_at: String,
}

type RawJob = (
    i64,
    String,
    String,
    String,
    String,
    i64,
    Option<String>,
    String,
);

fn read_job(r: &Row<'_>) -> rusqlite::Result<RawJob> {
    Ok((
        r.get("id")?,
        r.get("review_id")?,
        r.get("kind")?,
        r.get("payload")?,
        r.get("status")?,
        r.get("attempts")?,
        r.get("last_error")?,
        r.get("updated_at")?,
    ))
}

fn to_job(raw: RawJob) -> Result<Job> {
    let (id, review_id, kind, payload, status, attempts, last_error, updated_at) = raw;
    Ok(Job {
        id,
        review_id,
        kind,
        payload: serde_json::from_str(&payload)?,
        status: JobStatus::parse(&status)?,
        attempts,
        last_error,
        updated_at,
    })
}

pub fn enqueue(
    conn: &Connection,
    review_id: &str,
    kind: &str,
    payload: &serde_json::Value,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO job (review_id, kind, payload, status, updated_at) VALUES (?1, ?2, ?3, 'queued', ?4)",
        params![review_id, kind, payload.to_string(), now_iso()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Returns jobs interrupted mid-run to the queue. Call once when a worker
/// starts, before it claims anything. Returns how many were recovered.
pub fn recover(conn: &Connection) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE job SET status = 'queued', last_error = 'interrupted; requeued at startup', updated_at = ?1
         WHERE status = 'running'",
        [now_iso()],
    )?)
}

/// Job kinds claimed ahead of the queue: quick jobs other work waits on (the
/// local pre-screen decides screening order; retrieval is network-bound and
/// should not queue behind slow AI screening).
pub const PRIORITY: &[&str] = &["prescreen_ta", "retrieve"];

/// Atomically moves the oldest queued job (priority kinds first) to `running` and returns it.
pub fn claim_next(conn: &Connection) -> Result<Option<Job>> {
    let priority = PRIORITY
        .iter()
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(",");
    conn.query_row(
        &format!(
            "UPDATE job SET status = 'running', attempts = attempts + 1, updated_at = ?1
             WHERE id = (SELECT id FROM job WHERE status = 'queued'
                         ORDER BY kind IN ({priority}) DESC, id LIMIT 1)
             RETURNING *"
        ),
        [now_iso()],
        read_job,
    )
    .optional()?
    .map(to_job)
    .transpose()
}

fn set_status(conn: &Connection, id: i64, status: JobStatus, error: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE job SET status = ?1, last_error = ?2, updated_at = ?3 WHERE id = ?4",
        params![status.as_str(), error, now_iso(), id],
    )?;
    Ok(())
}

pub fn complete(conn: &Connection, id: i64) -> Result<()> {
    set_status(conn, id, JobStatus::Done, None)
}

pub fn pause(conn: &Connection, id: i64, reason: &str) -> Result<()> {
    set_status(conn, id, JobStatus::Paused, Some(reason))
}

/// Records a failure. Retryable failures go back to the queue until
/// `MAX_ATTEMPTS`; fatal ones fail immediately. Returns the new status.
pub fn fail(conn: &Connection, id: i64, error: &str, retryable: bool) -> Result<JobStatus> {
    let attempts: i64 =
        conn.query_row("SELECT attempts FROM job WHERE id = ?1", [id], |r| r.get(0))?;
    let status = if retryable && attempts < MAX_ATTEMPTS {
        JobStatus::Queued
    } else {
        JobStatus::Failed
    };
    set_status(conn, id, status, Some(error))?;
    Ok(status)
}

/// Parks a job as `paused` without using up an attempt, so resuming retries it.
pub fn requeue_paused(conn: &Connection, id: i64, reason: &str) -> Result<()> {
    conn.execute(
        "UPDATE job SET status = 'paused', attempts = MAX(attempts - 1, 0), last_error = ?1, updated_at = ?2 WHERE id = ?3",
        params![reason, now_iso(), id],
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunPause {
    pub reason: String,
    pub rate_limited: bool,
    pub since: String,
}

impl RunPause {
    pub fn paused_for(&self) -> std::time::Duration {
        let since = crate::time::parse_iso_ms(&self.since).unwrap_or_else(crate::time::now_ms);
        std::time::Duration::from_millis((crate::time::now_ms() - since).max(0) as u64)
    }
}

/// The run's pause, if it is paused.
pub fn run_paused(conn: &Connection) -> Result<Option<RunPause>> {
    Ok(conn
        .query_row(
            "SELECT pause_reason, rate_limited, updated_at FROM run_state WHERE paused = 1 LIMIT 1",
            [],
            |r| {
                Ok(RunPause {
                    reason: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    rate_limited: r.get(1)?,
                    since: r.get(2)?,
                })
            },
        )
        .optional()?)
}

/// Pauses the whole run. Queued jobs stay queued; the worker stops claiming.
pub fn pause_run(conn: &Connection, reason: &str, rate_limited: bool) -> Result<()> {
    let review_id: String = conn.query_row("SELECT id FROM review LIMIT 1", [], |r| r.get(0))?;
    conn.execute(
        "INSERT INTO run_state (review_id, paused, pause_reason, rate_limited, updated_at) VALUES (?1, 1, ?2, ?3, ?4)
         ON CONFLICT(review_id) DO UPDATE SET paused = 1, pause_reason = excluded.pause_reason,
            rate_limited = excluded.rate_limited, updated_at = excluded.updated_at",
        params![review_id, reason, rate_limited, now_iso()],
    )?;
    Ok(())
}

/// Resumes the run and returns paused jobs to the queue.
pub fn resume_run(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE run_state SET paused = 0, pause_reason = NULL, rate_limited = 0, updated_at = ?1",
        [now_iso()],
    )?;
    conn.execute(
        "UPDATE job SET status = 'queued', updated_at = ?1 WHERE status = 'paused'",
        [now_iso()],
    )?;
    Ok(())
}

/// Enqueues unless an identical job (same kind and payload) exists in any
/// state. A failed job is retried by the user, never re-enqueued in a loop.
pub fn enqueue_once(
    conn: &Connection,
    review_id: &str,
    kind: &str,
    payload: &serde_json::Value,
) -> Result<Option<i64>> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM job WHERE kind = ?1 AND payload = ?2)",
        params![kind, payload.to_string()],
        |r| r.get(0),
    )?;
    if exists {
        return Ok(None);
    }
    enqueue(conn, review_id, kind, payload).map(Some)
}

/// Jobs of these kinds that are not finished.
pub fn outstanding(conn: &Connection, kinds: &[&str]) -> Result<i64> {
    let mut n = 0;
    for k in kinds {
        n += conn.query_row(
            "SELECT COUNT(*) FROM job WHERE kind = ?1 AND status IN ('queued','running','paused')",
            [k],
            |r| r.get::<_, i64>(0),
        )?;
    }
    Ok(n)
}

/// Jobs of these kinds that failed for good.
pub fn failed(conn: &Connection, kinds: &[&str]) -> Result<Vec<Job>> {
    Ok(list(conn)?
        .into_iter()
        .filter(|j| j.status == JobStatus::Failed && kinds.contains(&j.kind.as_str()))
        .collect())
}

/// Puts failed jobs back in the queue with a fresh attempt count.
pub fn retry_failed(conn: &Connection) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE job SET status = 'queued', attempts = 0, last_error = NULL, updated_at = ?1 WHERE status = 'failed'",
        [now_iso()],
    )?)
}

/// Kinds whose failures the user may set aside: a source that cannot be
/// searched, or a record whose PDF cannot be fetched. Both are disclosed.
pub const DISMISSIBLE: &[&str] = &[
    "search_source",
    "seed_search",
    "seed_check",
    "search_broaden",
    "citation_chasing",
    "retrieve",
    "prescreen_ta",
];

/// Marks failed dismissible jobs done, keeping the error. Returns how many.
pub fn dismiss_failed(conn: &Connection) -> Result<usize> {
    let mut n = 0;
    for k in DISMISSIBLE {
        n += conn.execute(
            "UPDATE job SET status = 'done', last_error = 'dismissed by the user: ' || COALESCE(last_error, ''), updated_at = ?1
             WHERE status = 'failed' AND kind = ?2",
            params![now_iso(), k],
        )?;
    }
    Ok(n)
}

/// Marks unstarted jobs of one kind done without running them, keeping the
/// reason in `last_error`. `only` limits it to jobs whose payload field
/// `only.0` is one of `only.1`. Returns how many.
pub fn set_aside(
    conn: &Connection,
    kind: &str,
    only: Option<(&str, &std::collections::HashSet<String>)>,
    reason: &str,
) -> Result<usize> {
    let mut n = 0;
    for job in list(conn)? {
        let unstarted = matches!(job.status, JobStatus::Queued | JobStatus::Paused);
        let chosen = only.is_none_or(|(key, values)| {
            job.payload[key]
                .as_str()
                .is_some_and(|v| values.contains(v))
        });
        if job.kind == kind && unstarted && chosen {
            n += conn.execute(
                "UPDATE job SET status = 'done', last_error = ?1, updated_at = ?2 WHERE id = ?3",
                params![reason, now_iso(), job.id],
            )?;
        }
    }
    Ok(n)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<Job>> {
    conn.query_row("SELECT * FROM job WHERE id = ?1", [id], read_job)
        .optional()?
        .map(to_job)
        .transpose()
}

pub fn list(conn: &Connection) -> Result<Vec<Job>> {
    let mut stmt = conn.prepare("SELECT * FROM job ORDER BY id DESC")?;
    let raws = stmt
        .query_map([], read_job)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter().map(to_job).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, reviews};
    use serde_json::json;

    fn setup() -> (tempfile::TempDir, Connection, String) {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open_review(&dir.path().join("r.sift")).unwrap();
        let rid = reviews::insert_review(&conn, "q").unwrap();
        (dir, conn, rid)
    }

    #[test]
    fn priority_kinds_jump_the_queue() {
        let (_d, conn, rid) = setup();
        let a = enqueue(&conn, &rid, "screen_ta", &json!({"n": 1})).unwrap();
        let p = enqueue(&conn, &rid, "prescreen_ta", &json!({})).unwrap();
        assert_eq!(claim_next(&conn).unwrap().unwrap().id, p);
        assert_eq!(claim_next(&conn).unwrap().unwrap().id, a);
    }

    #[test]
    fn claims_in_fifo_order_and_completes() {
        let (_d, conn, rid) = setup();
        let a = enqueue(&conn, &rid, "noop", &json!({"n": 1})).unwrap();
        let b = enqueue(&conn, &rid, "noop", &json!({"n": 2})).unwrap();
        let first = claim_next(&conn).unwrap().unwrap();
        assert_eq!(
            (first.id, first.status, first.attempts),
            (a, JobStatus::Running, 1)
        );
        assert_eq!(first.payload, json!({"n": 1}));
        complete(&conn, a).unwrap();
        assert_eq!(claim_next(&conn).unwrap().unwrap().id, b);
        assert!(claim_next(&conn).unwrap().is_none());
        assert_eq!(get(&conn, a).unwrap().unwrap().status, JobStatus::Done);
    }

    #[test]
    fn retryable_failures_requeue_until_max_attempts() {
        let (_d, conn, rid) = setup();
        let id = enqueue(&conn, &rid, "noop", &json!({})).unwrap();
        for attempt in 1..=MAX_ATTEMPTS {
            let job = claim_next(&conn).unwrap().unwrap();
            assert_eq!(job.attempts, attempt);
            let status = fail(&conn, id, "HTTP 503", true).unwrap();
            let expected = if attempt < MAX_ATTEMPTS {
                JobStatus::Queued
            } else {
                JobStatus::Failed
            };
            assert_eq!(status, expected);
        }
        assert!(claim_next(&conn).unwrap().is_none());
        assert_eq!(
            get(&conn, id).unwrap().unwrap().last_error.as_deref(),
            Some("HTTP 503")
        );
    }

    #[test]
    fn fatal_failure_does_not_retry() {
        let (_d, conn, rid) = setup();
        let id = enqueue(&conn, &rid, "noop", &json!({})).unwrap();
        claim_next(&conn).unwrap();
        assert_eq!(
            fail(&conn, id, "bad input", false).unwrap(),
            JobStatus::Failed
        );
    }

    #[test]
    fn recover_requeues_only_running_jobs() {
        let (_d, conn, rid) = setup();
        let running = enqueue(&conn, &rid, "noop", &json!({})).unwrap();
        let done = enqueue(&conn, &rid, "noop", &json!({})).unwrap();
        let paused = enqueue(&conn, &rid, "noop", &json!({})).unwrap();
        claim_next(&conn).unwrap();
        claim_next(&conn).unwrap();
        complete(&conn, done).unwrap();
        claim_next(&conn).unwrap();
        pause(&conn, paused, "strict mode").unwrap();

        assert_eq!(recover(&conn).unwrap(), 1);
        assert_eq!(
            get(&conn, running).unwrap().unwrap().status,
            JobStatus::Queued
        );
        assert_eq!(get(&conn, done).unwrap().unwrap().status, JobStatus::Done);
        assert_eq!(
            get(&conn, paused).unwrap().unwrap().status,
            JobStatus::Paused
        );
    }
}
