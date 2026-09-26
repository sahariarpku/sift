//! SQLite files: one `.sift` file per review, plus `app.db` for app settings.
//!
//! Migrations are plain SQL files applied in order and tracked with
//! `PRAGMA user_version`. A file newer than the app is refused, never downgraded.

use std::path::Path;

use rusqlite::Connection;

use crate::error::{Error, Result};

/// Ordered migrations for review files. Append only; never edit a shipped file.
pub const REVIEW_MIGRATIONS: &[&str] = &[
    include_str!("../../migrations/review/0001_init.sql"),
    include_str!("../../migrations/review/0002_pipeline.sql"),
    include_str!("../../migrations/review/0003_views.sql"),
    include_str!("../../migrations/review/0004_skip_citation_chasing.sql"),
    include_str!("../../migrations/review/0005_stage_skips.sql"),
    include_str!("../../migrations/review/0006_speed_indexes.sql"),
];

/// Ordered migrations for `app.db`.
pub const APP_MIGRATIONS: &[&str] = &[
    include_str!("../../migrations/app/0001_init.sql"),
    include_str!("../../migrations/app/0002_providers.sql"),
];

/// Opens (creating if needed) a review file and brings its schema up to date.
pub fn open_review(path: &Path) -> Result<Connection> {
    open(path, REVIEW_MIGRATIONS)
}

/// Opens (creating if needed) `app.db` and brings its schema up to date.
pub fn open_app(path: &Path) -> Result<Connection> {
    open(path, APP_MIGRATIONS)
}

fn open(path: &Path, migrations: &[&str]) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut conn = Connection::open(path)?;
    configure(&conn)?;
    migrate(&mut conn, migrations)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    // WAL keeps the file readable while a worker writes; NORMAL sync is
    // durable across app crashes (only an OS crash can lose the last commit).
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(())
}

/// Applies every migration past the file's `user_version`, each in its own transaction.
pub fn migrate(conn: &mut Connection, migrations: &[&str]) -> Result<()> {
    let current: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let supported = migrations.len() as i64;
    if current > supported {
        return Err(Error::SchemaTooNew {
            found: current,
            supported,
        });
    }
    for (i, sql) in migrations.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

pub fn schema_version(conn: &Connection) -> Result<i64> {
    Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn review_schema_has_every_section_5_table() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_review(&dir.path().join("r.sift")).unwrap();
        assert_eq!(
            table_names(&conn),
            [
                "appraisal",
                "author_text",
                "event",
                "extraction_value",
                "job",
                "llm_call",
                "protocol_version",
                "record",
                "report",
                "review",
                "review_setting",
                "run_state",
                "search_run",
                "study",
                "synthesis_item",
                "synthesis_link"
            ]
        );
        assert_eq!(
            schema_version(&conn).unwrap(),
            REVIEW_MIGRATIONS.len() as i64
        );
    }

    #[test]
    fn migrations_are_idempotent_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.sift");
        drop(open_review(&path).unwrap());
        let conn = open_review(&path).unwrap();
        assert_eq!(
            schema_version(&conn).unwrap(),
            REVIEW_MIGRATIONS.len() as i64
        );
    }

    #[test]
    fn refuses_file_from_newer_app() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.sift");
        {
            let conn = open_review(&path).unwrap();
            conn.pragma_update(None, "user_version", 99).unwrap();
        }
        assert!(matches!(
            open_review(&path),
            Err(Error::SchemaTooNew { found: 99, .. })
        ));
    }

    #[test]
    fn app_db_schema() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_app(&dir.path().join("app.db")).unwrap();
        assert_eq!(
            table_names(&conn),
            [
                "model_price",
                "provider",
                "review_file",
                "setting",
                "task_assignment"
            ]
        );
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_review(&dir.path().join("r.sift")).unwrap();
        let err = conn.execute(
            "INSERT INTO study (id, review_id, label) VALUES ('s1', 'missing', 'x')",
            [],
        );
        assert!(err.is_err());
    }
}
