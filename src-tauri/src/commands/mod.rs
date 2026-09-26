//! Tauri commands. Every command here must also be listed in `build.rs` and
//! allowed in `capabilities/default.json`.

pub mod pipeline;
pub mod reviews;
pub mod settings;

use serde::Serialize;

use crate::error::Error;
use crate::llm::LlmError;

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

impl From<rusqlite::Error> for CommandError {
    fn from(e: rusqlite::Error) -> Self {
        Self {
            message: Error::from(e).to_string(),
        }
    }
}

impl From<LlmError> for CommandError {
    fn from(e: LlmError) -> Self {
        Self {
            message: e.to_string(),
        }
    }
}

pub type CmdResult<T> = Result<T, CommandError>;
