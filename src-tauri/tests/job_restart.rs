#![allow(clippy::unwrap_used)] // test-only crate

//! Phase 0 exit criterion: a job survives an app restart.
//!
//! "Restart" here means everything in memory is dropped (connections, workers,
//! tasks) and a new worker is built from the review file alone.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use sift_lib::jobs::{
    self, Job, JobContext, JobError, JobHandler, JobStatus, NoopHandler, Registry, Worker,
};
use sift_lib::pipeline::env::Env;
use sift_lib::{db, reviews};

struct Counting(Arc<AtomicUsize>);

#[async_trait]
impl JobHandler for Counting {
    async fn run(&self, _job: &Job, _ctx: &JobContext) -> Result<(), JobError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn env(dir: &std::path::Path) -> Arc<Env> {
    Env::detached(dir.join("app.db"))
}

/// Only the noop handler: these tests exercise the queue, not the pipeline.
fn plain() -> Registry {
    Registry::new().register(NoopHandler::KIND, Arc::new(NoopHandler))
}

fn new_review(dir: &std::path::Path) -> (std::path::PathBuf, String) {
    let app = db::open_app(&dir.join("app.db")).unwrap();
    let s = reviews::create(
        &app,
        &dir.join("reviews"),
        "Does tutoring raise attainment?",
    )
    .unwrap();
    (s.path.into(), s.id)
}

#[tokio::test]
async fn queued_job_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (path, rid) = new_review(dir.path());

    // Session 1: enqueue, then quit before any worker runs.
    let id = {
        let conn = db::open_review(&path).unwrap();
        jobs::enqueue(&conn, &rid, "count", &json!({})).unwrap()
    };

    // Session 2: a fresh worker finds and runs it.
    let runs = Arc::new(AtomicUsize::new(0));
    let worker = Worker::new(
        path.clone(),
        rid.clone(),
        Arc::new(Registry::new().register("count", Arc::new(Counting(runs.clone())))),
        env(dir.path()),
    );
    worker.recover().await.unwrap();
    assert_eq!(worker.drain().await.unwrap(), 1);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let job = jobs::get(&db::open_review(&path).unwrap(), id)
        .unwrap()
        .unwrap();
    assert_eq!(job.status, JobStatus::Done);
}

#[tokio::test]
async fn job_interrupted_mid_run_resumes_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (path, rid) = new_review(dir.path());
    let id = {
        let conn = db::open_review(&path).unwrap();
        jobs::enqueue(
            &conn,
            &rid,
            NoopHandler::KIND,
            &json!({ "sleep_ms": 60_000 }),
        )
        .unwrap()
    };

    // Session 1: the worker starts a long job and the process dies mid-run.
    let session1 = tokio::spawn(
        Worker::new(
            path.clone(),
            rid.clone(),
            Arc::new(plain()),
            env(dir.path()),
        )
        .run_forever(),
    );
    let conn = db::open_review(&path).unwrap();
    for _ in 0..200 {
        if jobs::get(&conn, id).unwrap().unwrap().status == JobStatus::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        jobs::get(&conn, id).unwrap().unwrap().status,
        JobStatus::Running
    );
    session1.abort();
    let _ = session1.await;
    drop(conn);

    // The file still says "running": nothing cleaned up after the crash.
    let conn = db::open_review(&path).unwrap();
    assert_eq!(
        jobs::get(&conn, id).unwrap().unwrap().status,
        JobStatus::Running
    );

    // Session 2: recovery requeues it and it completes.
    let runs = Arc::new(AtomicUsize::new(0));
    let worker = Worker::new(
        path.clone(),
        rid.clone(),
        Arc::new(Registry::new().register(NoopHandler::KIND, Arc::new(Counting(runs.clone())))),
        env(dir.path()),
    );
    assert_eq!(worker.recover().await.unwrap(), 1);
    assert_eq!(worker.drain().await.unwrap(), 1);
    let job = jobs::get(&conn, id).unwrap().unwrap();
    assert_eq!(job.status, JobStatus::Done);
    assert_eq!(
        job.attempts, 2,
        "one interrupted attempt plus the resumed one"
    );
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unknown_kind_is_paused_not_lost() {
    let dir = tempfile::tempdir().unwrap();
    let (path, rid) = new_review(dir.path());
    let id = jobs::enqueue(
        &db::open_review(&path).unwrap(),
        &rid,
        "from_the_future",
        &json!({}),
    )
    .unwrap();
    let worker = Worker::new(
        path.clone(),
        rid.clone(),
        Arc::new(plain()),
        env(dir.path()),
    );
    worker.drain().await.unwrap();
    let job = jobs::get(&db::open_review(&path).unwrap(), id)
        .unwrap()
        .unwrap();
    assert_eq!(job.status, JobStatus::Paused);
    assert!(job.last_error.unwrap().contains("no handler"));
}

struct Panics;

#[async_trait]
impl JobHandler for Panics {
    async fn run(&self, _job: &Job, _ctx: &JobContext) -> Result<(), JobError> {
        panic!("boom");
    }
}

#[tokio::test]
async fn panicking_handler_fails_the_job_not_the_worker() {
    let dir = tempfile::tempdir().unwrap();
    let (path, rid) = new_review(dir.path());
    let conn = db::open_review(&path).unwrap();
    let bad = jobs::enqueue(&conn, &rid, "panics", &json!({})).unwrap();
    let good = jobs::enqueue(&conn, &rid, NoopHandler::KIND, &json!({})).unwrap();
    let worker = Worker::new(
        path.clone(),
        rid.clone(),
        Arc::new(plain().register("panics", Arc::new(Panics))),
        env(dir.path()),
    );
    assert_eq!(worker.drain().await.unwrap(), 2);
    assert_eq!(
        jobs::get(&conn, bad).unwrap().unwrap().status,
        JobStatus::Failed
    );
    assert_eq!(
        jobs::get(&conn, good).unwrap().unwrap().status,
        JobStatus::Done
    );
}
