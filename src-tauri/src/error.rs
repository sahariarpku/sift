use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("keychain error: {0}")]
    Secret(String),
    #[error(
        "review file is at schema version {found}, newer than this app supports ({supported})"
    )]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("unknown review: {0}")]
    UnknownReview(String),
    #[error("{0}")]
    Invalid(String),
    #[error("background task failed: {0}")]
    Task(String),
}

pub type Result<T> = std::result::Result<T, Error>;
