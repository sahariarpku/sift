//! Tauri commands. Every command here must also be listed in `build.rs` and
//! allowed in `capabilities/default.json`.

use serde::Serialize;
use tauri::State;

use crate::db;
use crate::error::Error;
use crate::jobs::{self, Job, NoopHandler};
use crate::reviews::{self, ReviewSummary};
use crate::state::AppState;

/// Error shape sent to the frontend: a plain message, never a secret.
#[derive(Debug, Serialize)]
pub struct CommandError {
    pub message: String,
}

impl From<Error> for CommandError {
    fn from(e: Error) -> Self {
        Self {
            message: e.to_string(),
        }
    }
}

type CmdResult<T> = Result<T, CommandError>;

#[tauri::command]
pub async fn create_review(
    state: State<'_, AppState>,
    question: String,
) -> CmdResult<ReviewSummary> {
    let summary = reviews::create(&*state.app_db()?, &state.reviews_dir, &question)?;
    state.ensure_worker(&summary.id)?;
    Ok(summary)
}

#[tauri::command]
pub async fn list_reviews(state: State<'_, AppState>) -> CmdResult<Vec<ReviewSummary>> {
    Ok(reviews::list(&*state.app_db()?)?)
}

/// Queues a `noop` job that waits `sleep_ms`. Used to check that the queue
/// survives a restart; replaced by real job kinds from Phase 2.
#[tauri::command]
pub async fn enqueue_noop_job(
    state: State<'_, AppState>,
    review_id: String,
    sleep_ms: u64,
) -> CmdResult<i64> {
    let path = reviews::path_of(&*state.app_db()?, &review_id)?;
    let conn = db::open_review(&path)?;
    let id = jobs::enqueue(
        &conn,
        &review_id,
        NoopHandler::KIND,
        &serde_json::json!({ "sleep_ms": sleep_ms }),
    )?;
    state.ensure_worker(&review_id)?;
    Ok(id)
}

#[tauri::command]
pub async fn list_jobs(state: State<'_, AppState>, review_id: String) -> CmdResult<Vec<Job>> {
    let path = reviews::path_of(&*state.app_db()?, &review_id)?;
    Ok(jobs::list(&db::open_review(&path)?)?)
}

/// Estimated spend so far, summed from the review's event log.
#[tauri::command]
pub async fn review_spend(state: State<'_, AppState>, review_id: String) -> CmdResult<f64> {
    let path = reviews::path_of(&*state.app_db()?, &review_id)?;
    Ok(crate::events::total_cost(&db::open_review(&path)?)?)
}
