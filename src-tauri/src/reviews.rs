//! Review files on disk and the `review` row inside each one.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde::Serialize;

use crate::db;
use crate::error::{Error, Result};
use crate::time::now_iso;

pub const REVIEW_EXTENSION: &str = "sift";

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSummary {
    pub id: String,
    pub question: String,
    pub verification: String,
    pub created_at: String,
    pub path: String,
}

pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Inserts the `review` row and returns its id.
pub fn insert_review(conn: &Connection, question: &str) -> Result<String> {
    let question = question.trim();
    if question.is_empty() {
        return Err(Error::Invalid("The research question is empty.".into()));
    }
    let id = new_id();
    conn.execute(
        "INSERT INTO review (id, question, created_at) VALUES (?1, ?2, ?3)",
        params![id, question, now_iso()],
    )?;
    Ok(id)
}

/// Creates `<reviews_dir>/<id>.sift`, registers it in `app.db`, and returns its summary.
pub fn create(app: &Connection, reviews_dir: &Path, question: &str) -> Result<ReviewSummary> {
    std::fs::create_dir_all(reviews_dir)?;
    // Write to a temporary name first so a half-created file is never registered.
    let tmp = reviews_dir.join(format!("{}.creating", new_id()));
    let init = || -> Result<String> {
        let conn = db::open_review(&tmp)?;
        let id = insert_review(&conn, question)?;
        conn.pragma_update(None, "journal_mode", "DELETE")?; // fold the WAL back in before rename
        Ok(id)
    };
    let id = match init() {
        Ok(id) => id,
        Err(e) => {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", tmp.display()));
            }
            return Err(e);
        }
    };
    let path = reviews_dir.join(format!("{id}.{REVIEW_EXTENSION}"));
    std::fs::rename(&tmp, &path)?;
    app.execute(
        "INSERT INTO review_file (review_id, path, added_at) VALUES (?1, ?2, ?3)",
        params![id, path.to_string_lossy(), now_iso()],
    )?;
    read_summary(&path)
}

/// Reads the summary from a review file.
pub fn read_summary(path: &Path) -> Result<ReviewSummary> {
    let conn = db::open_review(path)?;
    Ok(conn.query_row(
        "SELECT id, question, verification, created_at FROM review LIMIT 1",
        [],
        |r| {
            Ok(ReviewSummary {
                id: r.get(0)?,
                question: r.get(1)?,
                verification: r.get(2)?,
                created_at: r.get(3)?,
                path: path.to_string_lossy().into_owned(),
            })
        },
    )?)
}

/// Every registered review file that still exists, newest first.
pub fn list(app: &Connection) -> Result<Vec<ReviewSummary>> {
    let mut stmt = app.prepare("SELECT path FROM review_file ORDER BY added_at DESC")?;
    let paths: Vec<PathBuf> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(PathBuf::from)
        .collect();
    paths
        .iter()
        .filter(|p| p.exists())
        .map(|p| read_summary(p))
        .collect()
}

/// Path of a registered review.
pub fn path_of(app: &Connection, review_id: &str) -> Result<PathBuf> {
    app.query_row(
        "SELECT path FROM review_file WHERE review_id = ?1",
        [review_id],
        |r| r.get::<_, String>(0),
    )
    .map(PathBuf::from)
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Error::UnknownReview(review_id.into()),
        other => other.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_writes_a_review_file_and_registers_it() {
        let dir = tempfile::tempdir().unwrap();
        let app = db::open_app(&dir.path().join("app.db")).unwrap();
        let s = create(
            &app,
            &dir.path().join("reviews"),
            "  How do students experience belonging? ",
        )
        .unwrap();
        assert_eq!(s.question, "How do students experience belonging?");
        assert_eq!(s.verification, "unverified");
        assert!(Path::new(&s.path).exists());
        assert!(s.path.ends_with(".sift"));
        assert_eq!(uuid::Uuid::parse_str(&s.id).unwrap().get_version_num(), 7);
        assert_eq!(list(&app).unwrap(), vec![s.clone()]);
        assert_eq!(path_of(&app, &s.id).unwrap(), PathBuf::from(&s.path));
    }

    #[test]
    fn empty_question_is_rejected_and_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let app = db::open_app(&dir.path().join("app.db")).unwrap();
        let reviews = dir.path().join("reviews");
        assert!(matches!(
            create(&app, &reviews, "   "),
            Err(Error::Invalid(_))
        ));
        assert!(list(&app).unwrap().is_empty());
        assert_eq!(std::fs::read_dir(&reviews).unwrap().count(), 0);
    }

    #[test]
    fn unknown_review_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let app = db::open_app(&dir.path().join("app.db")).unwrap();
        assert!(matches!(
            path_of(&app, "nope"),
            Err(Error::UnknownReview(_))
        ));
    }
}
