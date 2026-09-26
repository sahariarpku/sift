//! Tokio worker that drains one review file's job queue.
//!
//! Up to `Env::job_concurrency` jobs run at once. After every job the worker
//! calls the pipeline orchestrator, which enqueues the next work or stops at
//! a gate. The orchestrator only ever runs on this loop, so it never races
//! with itself.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::Connection;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;
use tokio::task::JoinSet;

use super::Job;
use crate::db;
use crate::error::{Error, Result};
use crate::pipeline::env::Env;

/// How long an idle worker waits before checking the queue again even without a wake-up.
const IDLE_POLL: Duration = Duration::from_secs(5);
/// A run paused by a provider rate limit resumes by itself after this long.
const RATE_LIMIT_RESUME: Duration = Duration::from_secs(120);

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// Worth another attempt (timeouts, 429, 5xx).
    #[error("{0}")]
    Retryable(String),
    /// Retrying will not help (invalid payload, programming error).
    #[error("{0}")]
    Fatal(String),
    /// Stop the whole run and tell the user (spending cap, model unavailable, §6.4).
    #[error("{0}")]
    Pause(String),
    /// Like `Pause`, but the run resumes on its own after a rate-limit cooldown.
    #[error("{0}")]
    RateLimited(String),
}

/// What a handler can reach while it runs.
#[derive(Clone)]
pub struct JobContext {
    pub db_path: PathBuf,
    pub review_id: String,
    pub env: Arc<Env>,
}

impl JobContext {
    /// Opens a fresh connection to the review file for this job.
    pub fn open(&self) -> Result<Connection> {
        db::open_review(&self.db_path)
    }

    /// Folder next to the review file for PDFs and raw responses.
    pub fn data_dir(&self) -> PathBuf {
        let stem = self
            .db_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.db_path.with_file_name(format!("{stem}.files"))
    }
}

#[async_trait]
pub trait JobHandler: Send + Sync {
    async fn run(&self, job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError>;
}

/// Called after every job, and when the worker wakes. Enqueues the next work.
pub trait Orchestrator: Send + Sync {
    fn advance(&self, ctx: &JobContext) -> Result<()>;
}

/// Maps a job `kind` to its handler.
#[derive(Default, Clone)]
pub struct Registry {
    handlers: HashMap<String, Arc<dyn JobHandler>>,
    orchestrator: Option<Arc<dyn Orchestrator>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(mut self, kind: &str, handler: Arc<dyn JobHandler>) -> Self {
        self.handlers.insert(kind.to_owned(), handler);
        self
    }

    pub fn with_orchestrator(mut self, o: Arc<dyn Orchestrator>) -> Self {
        self.orchestrator = Some(o);
        self
    }

    fn get(&self, kind: &str) -> Option<Arc<dyn JobHandler>> {
        self.handlers.get(kind).cloned()
    }

    /// The registry the app ships with: every pipeline job plus the orchestrator.
    pub fn standard() -> Self {
        crate::pipeline::register(Self::new().register(NoopHandler::KIND, Arc::new(NoopHandler)))
    }
}

/// Test job: waits `sleep_ms` (default 0) and succeeds.
pub struct NoopHandler;

impl NoopHandler {
    pub const KIND: &'static str = "noop";
}

#[async_trait]
impl JobHandler for NoopHandler {
    async fn run(&self, job: &Job, _ctx: &JobContext) -> std::result::Result<(), JobError> {
        let ms = job
            .payload
            .get("sleep_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(())
    }
}

/// Runs one blocking database operation off the async runtime.
async fn with_db<T, F>(path: &Path, f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&Connection) -> Result<T> + Send + 'static,
{
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let conn = db::open_review(&path)?;
        f(&conn)
    })
    .await
    .map_err(|e| Error::Task(e.to_string()))?
}

pub struct Worker {
    ctx: JobContext,
    registry: Arc<Registry>,
    wake: Arc<Notify>,
    /// Signals the loop to claim jobs a background advance just queued.
    claim_now: Arc<Notify>,
    advancing: Arc<AtomicBool>,
    advance_again: Arc<AtomicBool>,
}

impl Worker {
    pub fn new(
        db_path: PathBuf,
        review_id: String,
        registry: Arc<Registry>,
        env: Arc<Env>,
    ) -> Self {
        Self {
            ctx: JobContext {
                db_path,
                review_id,
                env,
            },
            registry,
            wake: Arc::new(Notify::new()),
            claim_now: Arc::new(Notify::new()),
            advancing: Arc::new(AtomicBool::new(false)),
            advance_again: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Notify this after enqueueing or a human action so the worker reacts at once.
    pub fn waker(&self) -> Arc<Notify> {
        self.wake.clone()
    }

    /// Returns interrupted jobs to the queue. Call before the first claim.
    pub async fn recover(&self) -> Result<usize> {
        with_db(&self.ctx.db_path, super::recover).await
    }

    fn advance(&self) {
        if let Some(o) = &self.registry.orchestrator {
            if let Err(e) = o.advance(&self.ctx) {
                eprintln!("orchestrator error for {}: {e}", self.ctx.review_id);
            }
        }
    }

    /// Advances the pipeline on a blocking thread so a slow advance never
    /// holds up claiming and finishing jobs. Requests that arrive while one is
    /// running are folded into one more pass; afterwards the loop is told to
    /// claim whatever the advance queued.
    fn advance_soon(&self) {
        let Some(o) = self.registry.orchestrator.clone() else {
            return;
        };
        if self.advancing.swap(true, Ordering::SeqCst) {
            self.advance_again.store(true, Ordering::SeqCst);
            return;
        }
        let ctx = self.ctx.clone();
        let (advancing, again, claim) = (
            self.advancing.clone(),
            self.advance_again.clone(),
            self.claim_now.clone(),
        );
        tokio::task::spawn_blocking(move || {
            loop {
                again.store(false, Ordering::SeqCst);
                if let Err(e) = o.advance(&ctx) {
                    eprintln!("orchestrator error for {}: {e}", ctx.review_id);
                }
                if !again.load(Ordering::SeqCst) {
                    break;
                }
            }
            advancing.store(false, Ordering::SeqCst);
            claim.notify_one();
        });
    }

    async fn claim(&self) -> Result<Option<Job>> {
        with_db(&self.ctx.db_path, |c| {
            if super::run_paused(c)?.is_some() {
                return Ok(None);
            }
            super::claim_next(c)
        })
        .await
    }

    fn spawn(&self, set: &mut JoinSet<()>, job: Job) {
        let ctx = self.ctx.clone();
        let handler = self.registry.get(&job.kind);
        set.spawn(async move {
            let id = job.id;
            let Some(handler) = handler else {
                let reason = format!("no handler for job kind {:?}", job.kind);
                let _ = with_db(&ctx.db_path, move |c| super::pause(c, id, &reason)).await;
                return;
            };
            // Run in its own task so a panic fails this job, not the worker.
            let inner_ctx = ctx.clone();
            let outcome = tokio::spawn(async move { handler.run(&job, &inner_ctx).await }).await;
            let path = ctx.db_path.clone();
            let _ = with_db(&path, move |c| {
                match outcome {
                    Ok(Ok(())) => super::complete(c, id)?,
                    Ok(Err(JobError::Retryable(msg))) => {
                        super::fail(c, id, &msg, true)?;
                    }
                    Ok(Err(JobError::Fatal(msg))) => {
                        super::fail(c, id, &msg, false)?;
                    }
                    Ok(Err(JobError::Pause(msg))) => {
                        super::requeue_paused(c, id, &msg)?;
                        super::pause_run(c, &msg, false)?;
                    }
                    Ok(Err(JobError::RateLimited(msg))) => {
                        super::requeue_paused(c, id, &msg)?;
                        super::pause_run(c, &msg, true)?;
                    }
                    Err(join) => {
                        super::fail(c, id, &format!("handler panicked: {join}"), false)?;
                    }
                }
                Ok(())
            })
            .await;
        });
    }

    /// Runs jobs (concurrently) until nothing is queued or running, advancing
    /// the pipeline after each. Returns how many jobs ran. Used by tests.
    pub async fn drain(&self) -> Result<usize> {
        let limit = self.ctx.env.job_concurrency.max(1);
        let mut set = JoinSet::new();
        let mut ran = 0;
        self.advance();
        loop {
            while set.len() < limit {
                match self.claim().await? {
                    Some(job) => {
                        ran += 1;
                        self.spawn(&mut set, job);
                    }
                    None => break,
                }
            }
            if set.join_next().await.is_none() {
                self.advance();
                // Advancing may have enqueued more work.
                match self.claim().await? {
                    Some(job) => {
                        ran += 1;
                        self.spawn(&mut set, job);
                    }
                    None => return Ok(ran),
                }
            } else {
                self.advance();
            }
        }
    }

    /// Recovers, then processes jobs forever.
    pub async fn run_forever(self) {
        if let Err(e) = self.recover().await {
            eprintln!(
                "job recovery failed for {}: {e}",
                self.ctx.db_path.display()
            );
        }
        let limit = self.ctx.env.job_concurrency.max(1);
        let mut set: JoinSet<()> = JoinSet::new();
        self.advance_soon();
        loop {
            // Fill free slots.
            while set.len() < limit {
                match self.claim().await {
                    Ok(Some(job)) => self.spawn(&mut set, job),
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("job claim error for {}: {e}", self.ctx.review_id);
                        break;
                    }
                }
            }
            tokio::select! {
                Some(_) = set.join_next(), if !set.is_empty() => self.advance_soon(),
                _ = self.wake.notified() => self.advance_soon(),
                _ = self.claim_now.notified() => {}
                _ = tokio::time::sleep(IDLE_POLL) => {
                    self.auto_resume().await;
                    self.advance_soon();
                }
            }
        }
    }

    /// Resumes a run that paused only because a provider rate-limited it.
    async fn auto_resume(&self) {
        let _ = with_db(&self.ctx.db_path, |c| {
            if let Some(state) = super::run_paused(c)? {
                if state.rate_limited && state.paused_for() >= RATE_LIMIT_RESUME {
                    super::resume_run(c)?;
                }
            }
            Ok(())
        })
        .await;
    }
}
