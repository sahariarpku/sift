//! Batch upload: the user drops a pile of PDFs they downloaded themselves and
//! each one is attached to the report it belongs to.
//!
//! Matching runs in two steps. `stage` looks for a DOI in the file name and
//! the raw PDF bytes (publisher metadata usually carries one). A file with no
//! DOI match is kept under `staged/` while the frontend reads its first pages
//! with pdf.js; `match_staged` then tries the DOIs and the exact title in that
//! text. Anything still unmatched waits for the user to pick its record
//! (`assign_staged`). Every attachment is a human `uploaded` retrieval event.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::downloads::{attach_with, find_dois, match_report};
use super::retrieval::{without_pdf, MAX_PDF_BYTES};
use crate::error::{Error, Result};

#[derive(Debug, PartialEq)]
pub enum Staged {
    /// Attached to a report straight away; carries (report id, title).
    Matched(String, String),
    /// Kept for a text match or the user's choice; carries the staged id.
    Waiting(String),
}

fn check_pdf(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_PDF_BYTES {
        return Err(Error::Invalid("That PDF is too large.".into()));
    }
    if !bytes.starts_with(b"%PDF") {
        return Err(Error::Invalid("That file is not a PDF.".into()));
    }
    Ok(())
}

fn rationale(file: &str, how: &str) -> String {
    format!("Batch upload by the user ({file}); matched by {how}.")
}

/// Staged ids are ours (UUIDs); refuse anything that could leave `staged/`.
fn staged_path(data_dir: &Path, staged_id: &str) -> Result<PathBuf> {
    if staged_id.is_empty() || !staged_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(Error::Invalid("Unknown staged file.".into()));
    }
    Ok(data_dir.join("staged").join(format!("{staged_id}.pdf")))
}

fn read_staged(data_dir: &Path, staged_id: &str) -> Result<(PathBuf, Vec<u8>)> {
    let path = staged_path(data_dir, staged_id)?;
    let bytes =
        std::fs::read(&path).map_err(|_| Error::Invalid("That staged file is gone.".into()))?;
    Ok((path, bytes))
}

/// Attaches the PDF by a DOI in its name or bytes, or stages it.
pub fn stage(conn: &Connection, data_dir: &Path, filename: &str, bytes: &[u8]) -> Result<Staged> {
    check_pdf(bytes)?;
    let mut dois = find_dois(filename);
    for d in find_dois(&String::from_utf8_lossy(bytes)) {
        if !dois.contains(&d) {
            dois.push(d);
        }
    }
    if let Some((report_id, how)) = match_report(conn, &without_pdf(conn)?, &dois, None)? {
        let title = attach_with(
            conn,
            data_dir,
            &report_id,
            bytes,
            &rationale(filename, &how),
        )?;
        return Ok(Staged::Matched(report_id, title));
    }
    let id = crate::reviews::new_id();
    let path = staged_path(data_dir, &id)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, bytes)?;
    Ok(Staged::Waiting(id))
}

/// Tries the text pdf.js read from a staged file's first pages.
/// Returns (report id, title) when it matched.
pub fn match_staged(
    conn: &Connection,
    data_dir: &Path,
    staged_id: &str,
    filename: &str,
    text: &str,
) -> Result<Option<(String, String)>> {
    let (path, bytes) = read_staged(data_dir, staged_id)?;
    let dois = find_dois(text);
    let Some((report_id, how)) = match_report(conn, &without_pdf(conn)?, &dois, Some(text))? else {
        return Ok(None);
    };
    let title = attach_with(
        conn,
        data_dir,
        &report_id,
        &bytes,
        &rationale(filename, &how),
    )?;
    let _ = std::fs::remove_file(path);
    Ok(Some((report_id, title)))
}

/// The user says which record a staged file belongs to.
pub fn assign_staged(
    conn: &Connection,
    data_dir: &Path,
    staged_id: &str,
    filename: &str,
    report_id: &str,
) -> Result<String> {
    if !without_pdf(conn)?.iter().any(|r| r == report_id) {
        return Err(Error::Invalid("That record already has a PDF.".into()));
    }
    let (path, bytes) = read_staged(data_dir, staged_id)?;
    let title = attach_with(
        conn,
        data_dir,
        report_id,
        &bytes,
        &rationale(filename, "the user's choice"),
    )?;
    let _ = std::fs::remove_file(path);
    Ok(title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{self, ActorType, NewEvent};
    use rusqlite::params;

    /// Papers a, b and c have no PDF; b was marked not retrieved.
    fn setup() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let review = crate::reviews::insert_review(&conn, "Q?").unwrap();
        conn.execute(
            "INSERT INTO search_run (id, review_id, source, query_string, query_ast, run_at, hits) VALUES ('s1', ?1, 'openalex', 'q', '{}', 'now', 3)",
            [&review],
        )
        .unwrap();
        for (rec, doi, title, decision) in [
            (
                "a",
                Some("10.1080/03075079.2010.536528"),
                "Chinese students and their experiences of UK higher education",
                "queued_for_upload",
            ),
            (
                "b",
                Some("10.1177/1028315318773137"),
                "Belonging and loneliness among international postgraduates in England",
                "not_retrieved",
            ),
            (
                "c",
                None,
                "Academic adjustment of international doctoral students in Australian universities",
                "not_retrieved",
            ),
        ] {
            conn.execute(
                "INSERT INTO record (id, search_run_id, doi, title) VALUES (?1, 's1', ?2, ?3)",
                params![rec, doi, title],
            )
            .unwrap();
            let rep = format!("p{rec}");
            conn.execute(
                "INSERT INTO report (id, record_id, retrieval_status) VALUES (?1, ?2, 'pending')",
                params![rep, rec],
            )
            .unwrap();
            events::append(
                &conn,
                &NewEvent {
                    review_id: review.clone(),
                    target_type: "report".into(),
                    target_id: rep,
                    stage: "retrieval".into(),
                    actor_type: Some(ActorType::Agent),
                    actor_id: "retrieval_agent".into(),
                    decision: decision.into(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        (dir, conn)
    }

    fn status(conn: &Connection, report: &str) -> String {
        conn.query_row(
            "SELECT retrieval_status FROM report WHERE id = ?1",
            [report],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn uploading_a_and_c_fills_their_rows_and_leaves_b() {
        let (dir, conn) = setup();
        // a: DOI inside the PDF's metadata, under an unhelpful file name.
        let a = stage(
            &conn,
            dir.path(),
            "download (3).pdf",
            b"%PDF-1.7 <prism:doi>10.1080/03075079.2010.536528</prism:doi>",
        )
        .unwrap();
        assert!(matches!(a, Staged::Matched(ref r, _) if r == "pa"));

        // c: no DOI anywhere, so it is staged and matched by its title.
        let Staged::Waiting(id) = stage(&conn, dir.path(), "thesis.pdf", b"%PDF-1.4 x").unwrap()
        else {
            panic!("c has no DOI to match on");
        };
        let text = "University of Somewhere\nAcademic adjustment of international doctoral\nstudents in Australian universities\nA thesis";
        let (rep, title) = match_staged(&conn, dir.path(), &id, "thesis.pdf", text)
            .unwrap()
            .unwrap();
        assert_eq!(rep, "pc");
        assert!(title.starts_with("Academic adjustment"));
        assert!(!staged_path(dir.path(), &id).unwrap().exists());

        assert_eq!(status(&conn, "pa"), "uploaded");
        assert_eq!(status(&conn, "pc"), "uploaded");
        assert_eq!(without_pdf(&conn).unwrap(), vec!["pb".to_owned()]);
        assert!(dir.path().join("pdfs/pc.pdf").exists());

        let (actor, why): (String, String) = conn
            .query_row(
                "SELECT actor_type, rationale FROM event WHERE target_id = 'pc' AND decision = 'uploaded'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(actor, "human");
        assert!(why.contains("thesis.pdf") && why.contains("its title"));
    }

    #[test]
    fn flattened_doi_in_file_name_matches_a_not_retrieved_record() {
        let (dir, conn) = setup();
        let out = stage(
            &conn,
            dir.path(),
            "10.1177_1028315318773137.pdf",
            b"%PDF-1.7 body",
        )
        .unwrap();
        assert!(matches!(out, Staged::Matched(ref r, _) if r == "pb"));
    }

    #[test]
    fn unmatched_file_waits_for_the_users_choice() {
        let (dir, conn) = setup();
        let Staged::Waiting(id) = stage(&conn, dir.path(), "scan.pdf", b"%PDF-1.4 x").unwrap()
        else {
            panic!("nothing to match on");
        };
        assert!(
            match_staged(&conn, dir.path(), &id, "scan.pdf", "Unrelated text")
                .unwrap()
                .is_none()
        );
        let title = assign_staged(&conn, dir.path(), &id, "scan.pdf", "pb").unwrap();
        assert!(title.starts_with("Belonging"));
        assert_eq!(status(&conn, "pb"), "uploaded");
        // pb has its PDF now; a second assignment is refused.
        let Staged::Waiting(id2) = stage(&conn, dir.path(), "scan2.pdf", b"%PDF-1.4 y").unwrap()
        else {
            panic!("nothing to match on");
        };
        assert!(assign_staged(&conn, dir.path(), &id2, "scan2.pdf", "pb").is_err());
    }

    #[test]
    fn rejects_non_pdfs_and_unsafe_staged_ids() {
        let (dir, conn) = setup();
        assert!(stage(&conn, dir.path(), "a.pdf", b"<html>").is_err());
        assert!(match_staged(&conn, dir.path(), "../r", "x.pdf", "t").is_err());
        assert!(assign_staged(&conn, dir.path(), "../../etc/x", "x.pdf", "pa").is_err());
    }
}
