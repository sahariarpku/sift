//! Browser-assisted full-text retrieval (§8.5).
//!
//! For reports still waiting for a copy, Sift opens the DOI (through the
//! library proxy when one is set) in the user's own browser; the user clicks
//! the publisher's download button. Sift watches the Downloads folder and
//! attaches each new PDF to the waiting report it belongs to, matched by DOI
//! (file name or the PDF's embedded metadata) or, from the text pdf.js reads
//! in the app window, by DOI or exact title. The user downloads every file
//! themselves, so publisher terms on automated access are respected.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::retrieval::{attachable, MAX_PDF_BYTES};
use crate::error::{Error, Result};
use crate::events::{self, ActorType, NewEvent};
use crate::records;

const KEY: &str = "downloads_watch";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Watch {
    /// Unix seconds; only files saved after this are considered.
    since: u64,
    /// Files already attached.
    attached: Vec<String>,
    /// Reports already opened in the browser.
    opened: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DownloadFile {
    pub path: String,
    pub name: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScanOutcome {
    /// (file name, report title) attached in this scan.
    pub attached: Vec<(String, String)>,
    /// New PDFs not matched by DOI yet (the app reads their text next).
    pub unmatched: Vec<DownloadFile>,
    pub waiting: usize,
}

pub fn downloads_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| Path::new(&h).join("Downloads"))
}

fn load(conn: &Connection) -> Result<Watch> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM review_setting WHERE key = ?1",
            [KEY],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or_default())
}

fn save(conn: &Connection, w: &Watch) -> Result<()> {
    conn.execute(
        "INSERT INTO review_setting (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![KEY, serde_json::to_string(w)?],
    )?;
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The waiting reports to open next (not opened yet), up to `n`, with the URL to open.
pub fn next_to_open(
    conn: &Connection,
    proxy: Option<&str>,
    n: usize,
) -> Result<Vec<(String, String)>> {
    let mut w = load(conn)?;
    if w.since == 0 {
        // Start watching a minute back, in case a download finished just before.
        w.since = now_secs().saturating_sub(60);
    }
    let mut out = Vec::new();
    for report_id in attachable(conn)? {
        if out.len() >= n {
            break;
        }
        if w.opened.contains(&report_id) {
            continue;
        }
        let url = match proxy {
            Some(p) => super::retrieval::proxy_url(conn, p, &report_id),
            None => super::retrieval::proxy_url(conn, "", &report_id),
        };
        if let Ok(url) = url {
            w.opened.push(report_id.clone());
            out.push((report_id, url));
        }
    }
    save(conn, &w)?;
    Ok(out)
}

/// DOIs written in a file name or in raw PDF bytes (XMP metadata and the
/// document info are usually uncompressed). Lower-cased, trailing punctuation trimmed.
pub fn find_dois(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 8 < bytes.len() {
        if bytes[i] == b'1' && bytes[i + 1] == b'0' && bytes[i + 2] == b'.' {
            let mut j = i + 3;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            let digits = j - (i + 3);
            // A registrant code of 4 to 9 digits, then "/" (or "_" / "@" in file names).
            if (4..=9).contains(&digits)
                && j < bytes.len()
                && matches!(bytes[j], b'/' | b'_' | b'@')
            {
                let mut k = j + 1;
                while k < bytes.len()
                    && !bytes[k].is_ascii_whitespace()
                    && !matches!(
                        bytes[k],
                        b'"' | b'\'' | b'<' | b'>' | b')' | b']' | b'}' | b',' | b';'
                    )
                {
                    k += 1;
                }
                if k > j + 1 {
                    let mut doi =
                        format!("{}/{}", &text[i..j], &text[j + 1..k]).to_ascii_lowercase();
                    while doi.ends_with(['.', ':']) {
                        doi.pop();
                    }
                    if let Some(stem) = doi.strip_suffix(".pdf") {
                        doi = stem.to_owned();
                    }
                    if !out.contains(&doi) {
                        out.push(doi);
                    }
                }
                i = k;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// The report among `candidates` that a set of DOIs or a page of text belongs to.
pub(super) fn match_report(
    conn: &Connection,
    candidates: &[String],
    dois: &[String],
    text: Option<&str>,
) -> Result<Option<(String, String)>> {
    let norm_text = text.map(crate::dedup::normalise_title);
    for report_id in candidates {
        let record_id: String = conn.query_row(
            "SELECT record_id FROM report WHERE id = ?1",
            [report_id],
            |r| r.get(0),
        )?;
        let Some(rec) = records::record(conn, &record_id)? else {
            continue;
        };
        let title = rec.title.clone().unwrap_or_default();
        if let Some(doi) = rec.doi.as_deref().map(str::to_ascii_lowercase) {
            // File names replace "/" in the suffix too (e.g. 10.1080_03075079.2010.536528).
            let flat = doi.replace('/', "_");
            if dois
                .iter()
                .any(|d| *d == doi || d.replace('/', "_") == flat)
            {
                return Ok(Some((report_id.clone(), format!("DOI {doi}"))));
            }
        }
        if let Some(t) = &norm_text {
            let nt = crate::dedup::normalise_title(&title);
            // Exact title (at least five words) in the first pages.
            if nt.split(' ').count() >= 5 && t.contains(&nt) {
                return Ok(Some((report_id.clone(), "its title".into())));
            }
        }
    }
    Ok(None)
}

fn attach(
    conn: &Connection,
    data_dir: &Path,
    report_id: &str,
    bytes: &[u8],
    file: &str,
    how: &str,
) -> Result<String> {
    attach_with(
        conn,
        data_dir,
        report_id,
        bytes,
        &format!("Downloaded by the user in their browser ({file}); attached automatically, matched by {how}."),
    )
}

/// Stores a PDF the user supplied (browser download, reference manager) for a
/// waiting report and logs it. Returns the report's title.
pub(super) fn attach_with(
    conn: &Connection,
    data_dir: &Path,
    report_id: &str,
    bytes: &[u8],
    rationale: &str,
) -> Result<String> {
    let dir = data_dir.join("pdfs");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{report_id}.pdf"));
    std::fs::write(&path, bytes)?;
    conn.execute(
        "UPDATE report SET pdf_path = ?1, retrieval_status = 'uploaded', page_texts = NULL WHERE id = ?2",
        params![path.to_string_lossy(), report_id],
    )?;
    let (review_id, _) = crate::agents::protocol::review_question(conn)?;
    events::append(
        conn,
        &NewEvent {
            review_id,
            target_type: "report".into(),
            target_id: report_id.into(),
            stage: "retrieval".into(),
            actor_type: Some(ActorType::Human),
            actor_id: "human".into(),
            decision: "uploaded".into(),
            rationale: Some(rationale.to_owned()),
            ..Default::default()
        },
    )?;
    let record_id: String = conn.query_row(
        "SELECT record_id FROM report WHERE id = ?1",
        [report_id],
        |r| r.get(0),
    )?;
    Ok(records::record(conn, &record_id)?
        .and_then(|r| r.title)
        .unwrap_or_default())
}

fn read_pdf(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() as usize > MAX_PDF_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    bytes.starts_with(b"%PDF").then_some(bytes)
}

/// Looks for new PDFs in `dir`, attaching those whose DOI identifies a waiting report.
pub fn scan(conn: &Connection, data_dir: &Path, dir: &Path) -> Result<ScanOutcome> {
    let mut w = load(conn)?;
    if w.since == 0 {
        w.since = now_secs().saturating_sub(60);
        save(conn, &w)?;
    }
    let mut out = ScanOutcome::default();
    let mut entries: Vec<(PathBuf, u64)> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            let is_pdf = p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pdf"));
            let m = e.metadata().ok()?;
            let t = m
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_secs();
            (is_pdf && m.is_file() && t >= w.since).then_some((p, m.len()))
        })
        .collect();
    entries.sort();
    for (path, size) in entries {
        let key = path.to_string_lossy().into_owned();
        if w.attached.contains(&key) {
            continue;
        }
        let Some(bytes) = read_pdf(&path) else {
            continue;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut dois = find_dois(&name);
        dois.extend(find_dois(&String::from_utf8_lossy(&bytes)));
        let found = match_report(conn, &attachable(conn)?, &dois, None)?;
        if let Some((report_id, how)) = found {
            let title = attach(conn, data_dir, &report_id, &bytes, &name, &how)?;
            w.attached.push(key);
            out.attached.push((name, title));
        } else {
            out.unmatched.push(DownloadFile {
                path: key,
                name,
                size,
            });
        }
    }
    save(conn, &w)?;
    out.waiting = attachable(conn)?.len();
    Ok(out)
}

/// A PDF path inside the Downloads folder (never elsewhere).
pub fn checked_download(path: &str) -> Result<PathBuf> {
    let dir = downloads_dir()
        .and_then(|d| d.canonicalize().ok())
        .ok_or_else(|| Error::Invalid("No Downloads folder.".into()))?;
    let p = Path::new(path)
        .canonicalize()
        .map_err(|_| Error::Invalid("That file is gone.".into()))?;
    if !p.starts_with(&dir) || !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pdf")) {
        return Err(Error::Invalid(
            "Only PDFs in your Downloads folder can be attached.".into(),
        ));
    }
    Ok(p)
}

/// Matches a download by the text pdf.js read from its first pages.
pub fn match_by_text(
    conn: &Connection,
    data_dir: &Path,
    path: &Path,
    text: &str,
) -> Result<Option<String>> {
    let Some(bytes) = read_pdf(path) else {
        return Ok(None);
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dois = find_dois(text);
    let Some((report_id, how)) = match_report(conn, &attachable(conn)?, &dois, Some(text))? else {
        return Ok(None);
    };
    let title = attach(conn, data_dir, &report_id, &bytes, &name, &how)?;
    let mut w = load(conn)?;
    w.attached.push(path.to_string_lossy().into_owned());
    save(conn, &w)?;
    Ok(Some(title))
}

/// The user assigns a download to a waiting report by hand.
pub fn assign(conn: &Connection, data_dir: &Path, path: &Path, report_id: &str) -> Result<String> {
    if !attachable(conn)?.iter().any(|r| r == report_id) {
        return Err(Error::Invalid(
            "That report is not waiting for a PDF.".into(),
        ));
    }
    let bytes = read_pdf(path).ok_or_else(|| Error::Invalid("That file is not a PDF.".into()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let title = attach(
        conn,
        data_dir,
        report_id,
        &bytes,
        &name,
        "the user's choice",
    )?;
    let mut w = load(conn)?;
    w.attached.push(path.to_string_lossy().into_owned());
    save(conn, &w)?;
    Ok(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_dois_in_file_names_and_metadata() {
        assert_eq!(
            find_dois("10.1177_1028315318773137.pdf"),
            vec!["10.1177/1028315318773137"]
        );
        assert_eq!(
            find_dois("<prism:doi>10.1080/03075079.2010.536528</prism:doi>"),
            vec!["10.1080/03075079.2010.536528"]
        );
        assert_eq!(
            find_dois("See https://doi.org/10.1016/J.NEDT.2017.05.019."),
            vec!["10.1016/j.nedt.2017.05.019"]
        );
        assert!(find_dois("version 10.2 of the report").is_empty());
    }

    fn setup() -> (tempfile::TempDir, Connection, String) {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let review = crate::reviews::insert_review(&conn, "Q?").unwrap();
        conn.execute(
            "INSERT INTO search_run (id, review_id, source, query_string, query_ast, run_at, hits) VALUES ('s1', ?1, 'openalex', 'q', '{}', 'now', 2)",
            [&review],
        )
        .unwrap();
        for (rec, doi, title) in [
            (
                "r1",
                "10.1080/03075079.2010.536528",
                "Chinese students and their experiences of UK higher education",
            ),
            (
                "r2",
                "10.1177/1028315318773137",
                "Belonging and loneliness among international postgraduates in England",
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
                    decision: "queued_for_upload".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        (dir, conn, review)
    }

    #[test]
    fn attaches_new_downloads_by_doi_and_by_title() {
        let (dir, conn, _) = setup();
        let downloads = dir.path().join("Downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        // Matched by the DOI in its (flattened) file name.
        std::fs::write(
            downloads.join("10.1080_03075079.2010.536528.pdf"),
            b"%PDF-1.7 body",
        )
        .unwrap();
        // No DOI anywhere: left for the text match.
        std::fs::write(downloads.join("fulltext.pdf"), b"%PDF-1.7 compressed").unwrap();
        // Not a PDF: ignored.
        std::fs::write(downloads.join("notes.pdf"), b"hello").unwrap();

        let out = scan(&conn, dir.path(), &downloads).unwrap();
        assert_eq!(out.attached.len(), 1);
        assert_eq!(out.unmatched.len(), 1);
        assert_eq!(out.waiting, 1);
        let status: String = conn
            .query_row(
                "SELECT retrieval_status FROM report WHERE id = 'pr1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "uploaded");

        // A second scan does not attach the same file again.
        assert!(scan(&conn, dir.path(), &downloads)
            .unwrap()
            .attached
            .is_empty());

        // The text pdf.js read from the unmatched file carries the title.
        let text = "Journal of Studies\nBelonging and loneliness among international\npostgraduates in England\nAbstract ...";
        let title =
            match_by_text(&conn, dir.path(), &downloads.join("fulltext.pdf"), text).unwrap();
        assert!(title.unwrap().starts_with("Belonging"));
        assert_eq!(attachable(&conn).unwrap().len(), 0);
    }

    #[test]
    fn opens_each_waiting_report_once() {
        let (_d, conn, _) = setup();
        let first = next_to_open(&conn, None, 1).unwrap();
        assert_eq!(first.len(), 1);
        assert!(first[0].1.starts_with("https://doi.org/10."));
        let second = next_to_open(&conn, Some("https://proxy.example.edu/login?url="), 5).unwrap();
        assert_eq!(second.len(), 1);
        assert!(second[0]
            .1
            .starts_with("https://proxy.example.edu/login?url=https://doi.org/"));
        assert!(next_to_open(&conn, None, 5).unwrap().is_empty());
    }
}
