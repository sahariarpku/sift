//! Full texts from the user's Zotero library (§8.5).
//!
//! Zotero finds PDFs with its own "Find Available PDF" and browser Connector,
//! under the user's control. Sift exports the reports still missing a PDF as
//! RIS (`exports::missing_ris`) for the user to import into Zotero, then reads
//! the PDFs Zotero stored and attaches them, matched by DOI or exact title.
//!
//! Zotero's database is opened read-only and immutable, so this works while
//! Zotero is running and never changes the library. Schema (Zotero 7, checked
//! 2026-09-25, zotero/zotero `resource/schema/userdata.sql`): `items(itemID, key)`,
//! `itemData(itemID, fieldID, valueID)`, `itemDataValues(valueID, value)`,
//! `fields(fieldID, fieldName)`, `itemAttachments(itemID, parentItemID, linkMode,
//! contentType, path)` with `path` = `storage:<file>` stored under
//! `<data dir>/storage/<attachment key>/`, and `deletedItems(itemID)`.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

use super::downloads::attach_with;
use super::retrieval::{attachable, MAX_PDF_BYTES};
use crate::error::{Error, Result};
use crate::records;

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ZoteroImport {
    pub attached: usize,
    pub still_missing: usize,
    pub library: String,
}

/// Zotero's default data directory.
pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| Path::new(&h).join("Zotero"))
}

fn open_library(dir: &Path) -> Result<Connection> {
    let db = dir.join("zotero.sqlite");
    if !db.is_file() {
        return Err(Error::Invalid(format!(
            "No Zotero library at {}. Install Zotero, or check its data folder in Zotero's settings.",
            dir.display()
        )));
    }
    let uri = format!("file:{}?immutable=1", db.display());
    Ok(Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?)
}

/// (parent itemID, lower-case DOI, title).
type LibraryItem = (i64, Option<String>, Option<String>);

/// Every regular (non-attachment, non-deleted) item.
fn library_items(z: &Connection) -> Result<Vec<LibraryItem>> {
    let mut stmt = z.prepare(
        "SELECT i.itemID,
                (SELECT LOWER(v.value) FROM itemData d JOIN fields f ON f.fieldID = d.fieldID
                   JOIN itemDataValues v ON v.valueID = d.valueID WHERE d.itemID = i.itemID AND f.fieldName = 'DOI'),
                (SELECT v.value FROM itemData d JOIN fields f ON f.fieldID = d.fieldID
                   JOIN itemDataValues v ON v.valueID = d.valueID WHERE d.itemID = i.itemID AND f.fieldName = 'title')
         FROM items i
         WHERE i.itemID NOT IN (SELECT itemID FROM deletedItems)
           AND i.itemID NOT IN (SELECT itemID FROM itemAttachments)",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Stored PDF files attached to an item.
fn pdfs_of(z: &Connection, dir: &Path, item: i64) -> Result<Vec<PathBuf>> {
    let mut stmt = z.prepare(
        "SELECT i.key, a.path, a.linkMode FROM itemAttachments a JOIN items i ON i.itemID = a.itemID
         WHERE a.parentItemID = ?1 AND a.contentType = 'application/pdf'
           AND a.itemID NOT IN (SELECT itemID FROM deletedItems)",
    )?;
    let rows = stmt.query_map([item], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (key, path, link_mode) = row?;
        let Some(path) = path else { continue };
        match path.strip_prefix("storage:") {
            Some(file) => out.push(dir.join("storage").join(key).join(file)),
            // Linked files (linkMode 2) keep an absolute path.
            None if link_mode == 2 => out.push(PathBuf::from(path)),
            None => {}
        }
    }
    Ok(out)
}

/// Attaches PDFs from the Zotero library in `dir` to waiting reports.
pub fn import(conn: &Connection, data_dir: &Path, dir: &Path) -> Result<ZoteroImport> {
    let z = open_library(dir)?;
    let items = library_items(&z)?;
    let mut attached = 0;
    for report_id in attachable(conn)? {
        let record_id: String = conn.query_row(
            "SELECT record_id FROM report WHERE id = ?1",
            [&report_id],
            |r| r.get(0),
        )?;
        let Some(rec) = records::record(conn, &record_id)? else {
            continue;
        };
        let doi = rec.doi.as_deref().map(str::to_ascii_lowercase);
        let title = rec.title.as_deref().map(crate::dedup::normalise_title);
        let hit = items.iter().find_map(|(id, zd, zt)| {
            let by_doi = doi.is_some()
                && zd
                    .as_deref()
                    .map(|d| d.trim_start_matches("https://doi.org/"))
                    == doi.as_deref();
            let by_title = title.as_ref().is_some_and(|t| {
                t.split(' ').count() >= 4
                    && zt.as_deref().is_some_and(|zt| {
                        strsim::jaro_winkler(&crate::dedup::normalise_title(zt), t) >= 0.97
                    })
            });
            (by_doi || by_title).then_some((*id, if by_doi { "DOI" } else { "title" }))
        });
        let Some((item, how)) = hit else { continue };
        for pdf in pdfs_of(&z, dir, item)? {
            let Ok(meta) = std::fs::metadata(&pdf) else {
                continue;
            };
            if meta.len() as usize > MAX_PDF_BYTES {
                continue;
            }
            let Ok(bytes) = std::fs::read(&pdf) else {
                continue;
            };
            if !bytes.starts_with(b"%PDF") {
                continue;
            }
            let name = pdf
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            attach_with(
                conn,
                data_dir,
                &report_id,
                &bytes,
                &format!("From the user's Zotero library ({name}), matched by {how}."),
            )?;
            attached += 1;
            break;
        }
    }
    Ok(ZoteroImport {
        attached,
        still_missing: attachable(conn)?.len(),
        library: dir.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{self, ActorType, NewEvent};

    /// A minimal Zotero library: one item with a DOI and a stored PDF, one
    /// matched by title, one deleted.
    fn zotero_fixture(dir: &Path) {
        let z = Connection::open(dir.join("zotero.sqlite")).unwrap();
        z.execute_batch(
            "CREATE TABLE items (itemID INTEGER PRIMARY KEY, key TEXT);
             CREATE TABLE fields (fieldID INTEGER PRIMARY KEY, fieldName TEXT);
             CREATE TABLE itemDataValues (valueID INTEGER PRIMARY KEY, value TEXT);
             CREATE TABLE itemData (itemID INTEGER, fieldID INTEGER, valueID INTEGER);
             CREATE TABLE itemAttachments (itemID INTEGER, parentItemID INTEGER, linkMode INTEGER, contentType TEXT, path TEXT);
             CREATE TABLE deletedItems (itemID INTEGER);
             INSERT INTO fields VALUES (1, 'title'), (2, 'DOI');
             INSERT INTO items VALUES (1, 'PARENT01'), (2, 'ATTACH01'), (3, 'PARENT02'), (4, 'ATTACH02');
             INSERT INTO itemDataValues VALUES (1, '10.1080/03075079.2010.536528'), (2, 'Anything'),
               (3, 'Belonging and loneliness among international postgraduates in England');
             INSERT INTO itemData VALUES (1, 2, 1), (1, 1, 2), (3, 1, 3);
             INSERT INTO itemAttachments VALUES (2, 1, 0, 'application/pdf', 'storage:paper.pdf'),
               (4, 3, 0, 'application/pdf', 'storage:other.pdf');",
        )
        .unwrap();
        for (key, file) in [("ATTACH01", "paper.pdf"), ("ATTACH02", "other.pdf")] {
            let d = dir.join("storage").join(key);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(file), b"%PDF-1.7 test").unwrap();
        }
    }

    #[test]
    fn imports_pdfs_by_doi_and_title_without_touching_the_library() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let review = crate::reviews::insert_review(&conn, "Q?").unwrap();
        conn.execute(
            "INSERT INTO search_run (id, review_id, source, query_string, query_ast, run_at, hits) VALUES ('s1', ?1, 'eric', 'q', '{}', 'now', 3)",
            [&review],
        )
        .unwrap();
        for (rec, doi, title) in [
            (
                "r1",
                Some("10.1080/03075079.2010.536528"),
                "Chinese students in UK higher education",
            ),
            (
                "r2",
                None,
                "Belonging and loneliness among international postgraduates in England",
            ),
            ("r3", None, "A paper Zotero does not have at all"),
        ] {
            conn.execute(
                "INSERT INTO record (id, search_run_id, doi, title) VALUES (?1, 's1', ?2, ?3)",
                rusqlite::params![rec, doi, title],
            )
            .unwrap();
            let rep = format!("p{rec}");
            conn.execute(
                "INSERT INTO report (id, record_id, retrieval_status) VALUES (?1, ?2, 'pending')",
                rusqlite::params![rep, rec],
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
                    decision: "queued_for_upload".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let zdir = dir.path().join("Zotero");
        std::fs::create_dir_all(&zdir).unwrap();
        zotero_fixture(&zdir);
        let before = std::fs::read(zdir.join("zotero.sqlite")).unwrap();

        let out = import(&conn, dir.path(), &zdir).unwrap();
        assert_eq!((out.attached, out.still_missing), (2, 1));
        assert_eq!(
            std::fs::read(zdir.join("zotero.sqlite")).unwrap(),
            before,
            "library unchanged"
        );
        assert!(import(&conn, dir.path(), &dir.path().join("nowhere")).is_err());
    }
}
