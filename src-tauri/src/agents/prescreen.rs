//! Job that scores records with the optional local Laya pre-screen before
//! title/abstract screening. See `local::laya` for how scores are used.

use async_trait::async_trait;

use super::protocol::{options, require_locked};
use crate::events::{self, ActorType, NewEvent};
use crate::jobs::{Job, JobContext, JobError, JobHandler};
use crate::local::laya::{self, Laya, Mode};
use crate::records;

pub const JOB: &str = "prescreen_ta";

fn db_err(e: impl std::fmt::Display) -> JobError {
    JobError::Fatal(e.to_string())
}

pub struct Prescreen;

#[async_trait]
impl JobHandler for Prescreen {
    async fn run(&self, _job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        let (opts, protocol, todo) = {
            let conn = ctx.open().map_err(db_err)?;
            let opts = options(&conn).map_err(db_err)?.prescreen;
            if opts.mode == Mode::Off {
                return Ok(());
            }
            let protocol = require_locked(&conn).map_err(db_err)?.body;
            (opts, protocol, laya::unscored(&conn).map_err(db_err)?)
        };
        if todo.is_empty() {
            return Ok(());
        }
        let configured = {
            let app = crate::db::open_app(&ctx.env.app_db_path).map_err(db_err)?;
            crate::settings::load(&app).map_err(db_err)?.laya_python
        };
        let status = laya::detect(configured.as_deref(), &opts.model).await;
        let (Some(python), true) = (status.python.as_deref(), status.available) else {
            return Err(JobError::Fatal(format!(
                "The local pre-screen is on for this review but Laya is not ready: {} \
                 Dismiss this job to screen without it.",
                status.message
            )));
        };
        let dir = ctx
            .env
            .app_db_path
            .parent()
            .map(|p| p.join("laya"))
            .unwrap_or_else(|| ctx.data_dir().join("laya"));
        let mut helper = Laya::start(std::path::Path::new(python), &dir, &opts.model)
            .await
            .map_err(|e| JobError::Fatal(e.to_string()))?;
        let questions = laya::questions(&protocol).map_err(JobError::Fatal)?;
        let model_id = format!("{}@laya-mlx {}", helper.model, helper.version);

        for rid in todo {
            let rec = {
                let conn = ctx.open().map_err(db_err)?;
                // Skip records scored meanwhile (a retried or overlapping job).
                if !laya::unscored_one(&conn, &rid).map_err(db_err)? {
                    continue;
                }
                records::record(&conn, &rid).map_err(db_err)?
            };
            let Some(rec) = rec else { continue };
            let state = laya::state(
                rec.title.as_deref().unwrap_or(""),
                rec.abstract_.as_deref().unwrap_or("(no abstract)"),
            );
            let answers = helper
                .ask(&state, &questions)
                .await
                .map_err(|e| JobError::Retryable(e.to_string()))?;
            let Some((score, parts)) = laya::score(&answers) else {
                continue;
            };
            let conn = ctx.open().map_err(db_err)?;
            events::append(
                &conn,
                &NewEvent {
                    review_id: ctx.review_id.clone(),
                    target_type: "record".into(),
                    target_id: rid,
                    stage: laya::STAGE.into(),
                    actor_type: Some(ActorType::Agent),
                    actor_id: laya::ACTOR.into(),
                    provider: Some("local".into()),
                    model_id: Some(model_id.clone()),
                    prompt_version: Some(laya::PROMPT_VERSION.into()),
                    decision: "score".into(),
                    rationale: Some(laya::rationale(&parts)),
                    confidence: Some(score),
                    ..Default::default()
                },
            )
            .map_err(db_err)?;
        }
        Ok(())
    }
}
