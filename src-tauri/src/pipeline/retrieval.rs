//! Full-text retrieval (§8.5) and report-to-study linking (§8.7).
//!
//! Order for each included record: the open-access PDF found at search time
//! (ERIC full text, CORE download, OpenAlex best OA location), then Unpaywall
//! (needs the user's email), CORE by DOI or title, OpenAlex OA locations.
//! Then, for DOIs, subscription access through the user's institution: publisher
//! TDM APIs where a key is set, and (opt-in) the publisher's declared PDF link
//! over the institution's network (see `fulltext_access`).
//! Otherwise the report waits in the upload queue; the user can open it
//! through their library proxy or browser and upload the PDF (or let the
//! Downloads watcher attach it), or mark it not retrieved.
//! Nothing is scraped. PDF text is extracted by pdf.js in the app window and
//! sent back with `store_page_texts`.

use std::time::Duration;

use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};

use crate::agents::protocol::review_question;
use crate::error::{Error, Result};
use crate::events::{self, ActorType, NewEvent};
use crate::jobs::{Job, JobContext, JobError, JobHandler};
use crate::records::{self, RecordRow};
use crate::reviews::new_id;
use crate::sources::openalex::OpenAlex;

use super::fulltext_access;

pub const RETRIEVE_JOB: &str = "retrieve";
pub const LINK_JOB: &str = "link_studies";
pub const MAX_PDF_BYTES: usize = 60 * 1024 * 1024;

fn db_err(e: Error) -> JobError {
    JobError::Fatal(e.to_string())
}

/// The report row for a record, created on first use.
pub fn ensure_report(conn: &Connection, record_id: &str) -> Result<String> {
    if let Some(r) = records::report_for_record(conn, record_id)? {
        return Ok(r.id);
    }
    let id = new_id();
    conn.execute(
        "INSERT INTO report (id, record_id, retrieval_status) VALUES (?1, ?2, 'pending')",
        params![id, record_id],
    )?;
    Ok(id)
}

fn retrieval_event(
    conn: &Connection,
    report_id: &str,
    actor: ActorType,
    decision: &str,
    rationale: &str,
) -> Result<()> {
    let (review_id, _) = review_question(conn)?;
    events::append(
        conn,
        &NewEvent {
            review_id,
            target_type: "report".into(),
            target_id: report_id.into(),
            stage: "retrieval".into(),
            actor_type: Some(actor),
            actor_id: if actor == ActorType::Human {
                "human".into()
            } else {
                "retrieval_agent".into()
            },
            decision: decision.into(),
            rationale: Some(rationale.into()),
            ..Default::default()
        },
    )?;
    Ok(())
}

/// Downloads a URL if it is a PDF. An open-access landing page (repository,
/// preprint server) is followed once to the PDF it declares in
/// `citation_pdf_url`, as a reader would click through.
async fn fetch_pdf(client: &reqwest::Client, url: &str) -> Option<Vec<u8>> {
    let (bytes, page_url) = get_bytes(client, url).await?;
    if bytes.starts_with(b"%PDF") {
        return Some(bytes);
    }
    let html = String::from_utf8_lossy(&bytes);
    let pdf = fulltext_access::citation_pdf_url(&html, &page_url)?;
    if pdf == url {
        return None;
    }
    let (bytes, _) = get_bytes(client, &pdf).await?;
    bytes.starts_with(b"%PDF").then_some(bytes)
}

/// One GET, tried up to three times (after 2 s, then 5 s): repository
/// servers are often slow or briefly fail (5xx, dropped redirects).
async fn get_bytes(client: &reqwest::Client, url: &str) -> Option<(Vec<u8>, String)> {
    for pause in [0, 2, 5] {
        if pause > 0 {
            tokio::time::sleep(Duration::from_secs(pause)).await;
        }
        if let Some(r) = get_once(client, url).await {
            return Some(r);
        }
    }
    None
}

async fn get_once(client: &reqwest::Client, url: &str) -> Option<(Vec<u8>, String)> {
    let resp = client
        .get(url)
        .timeout(Duration::from_secs(90))
        .header("accept", "*/*")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success()
        || resp
            .content_length()
            .is_some_and(|l| l as usize > MAX_PDF_BYTES)
    {
        return None;
    }
    let final_url = resp.url().to_string();
    let bytes = resp.bytes().await.ok()?;
    (bytes.len() <= MAX_PDF_BYTES).then(|| (bytes.to_vec(), final_url))
}

/// Open-access copies of a record, from every source at once (like Zotero's
/// "Find Available PDF", but across more indexes). Each lookup gives up
/// quickly on a rate limit instead of holding the paper up.
async fn candidates(ctx: &JobContext, rec: &RecordRow) -> Vec<(String, String)> {
    let env = &ctx.env;
    let quick = &crate::sources::adapter::QUICK;
    let email = env
        .app_db()
        .ok()
        .and_then(|app| crate::settings::load(&app).ok())
        .and_then(|s| s.unpaywall_email);

    // Unpaywall: every OA location for the DOI (needs the user's email).
    let unpaywall = async {
        let (Some(doi), Some(email)) = (&rec.doi, &email) else {
            return Vec::new();
        };
        let base = env
            .source_base("unpaywall")
            .unwrap_or("https://api.unpaywall.org");
        let url = format!("{base}/v2/{doi}");
        let Ok(v) = crate::sources::adapter::get_json_with(quick, &env.http, &url, || {
            env.http.get(&url).query(&[("email", email)])
        })
        .await
        else {
            return Vec::new();
        };
        v["oa_locations"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|loc| {
                // A direct PDF when Unpaywall has one, else the repository page
                // (like the Unpaywall browser extension, which opens that page).
                loc["url_for_pdf"]
                    .as_str()
                    .map(|p| (p.to_owned(), "Unpaywall".to_owned()))
                    .or_else(|| {
                        loc["url"]
                            .as_str()
                            .map(|p| (p.to_owned(), "Unpaywall (repository page)".to_owned()))
                    })
            })
            .collect::<Vec<_>>()
    };

    // CORE: search by DOI, else exact title.
    let core = async {
        let Ok(Some(key)) = env.secrets.get(&crate::sources::key_ref("core")) else {
            return Vec::new();
        };
        let base = env.source_base("core").unwrap_or("https://api.core.ac.uk");
        let q = match (&rec.doi, &rec.title) {
            (Some(d), _) => format!("doi:\"{d}\""),
            (None, Some(t)) => format!("title:\"{}\"", t.replace('"', "")),
            _ => return Vec::new(),
        };
        let url = format!("{base}/v3/search/works/");
        let Ok(v) = crate::sources::adapter::get_json_with(quick, &env.http, &url, || {
            env.http
                .get(&url)
                .bearer_auth(&key)
                .query(&[("q", q.as_str()), ("limit", "3")])
        })
        .await
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for w in v["results"].as_array().cloned().unwrap_or_default() {
            let same = match (
                &rec.doi,
                w["doi"]
                    .as_str()
                    .and_then(crate::sources::adapter::clean_doi),
            ) {
                (Some(a), Some(b)) => *a == b,
                _ => w["title"]
                    .as_str()
                    .zip(rec.title.as_deref())
                    .is_some_and(|(a, b)| {
                        strsim::jaro_winkler(
                            &crate::dedup::normalise_title(a),
                            &crate::dedup::normalise_title(b),
                        ) >= 0.95
                    }),
            };
            if let (true, Some(d)) = (same, w["downloadUrl"].as_str().filter(|s| !s.is_empty())) {
                out.push((d.to_owned(), "CORE".to_owned()));
            }
        }
        out
    };

    // OpenAlex OA locations (repositories, preprint servers, publisher OA).
    let openalex = async {
        let Some(doi) = &rec.doi else {
            return Vec::new();
        };
        let key = env
            .secrets
            .get(&crate::sources::key_ref("openalex"))
            .ok()
            .flatten();
        let oa = OpenAlex::new(env.http.clone(), env.source_base("openalex"), key);
        let Ok(Some(w)) = oa.by_doi(doi).await else {
            return Vec::new();
        };
        w["locations"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|loc| {
                loc["pdf_url"]
                    .as_str()
                    .map(|p| (p.to_owned(), "OpenAlex".to_owned()))
                    .or_else(|| {
                        // Open-access repository pages without a direct PDF link.
                        (loc["is_oa"].as_bool() == Some(true))
                            .then(|| loc["landing_page_url"].as_str())
                            .flatten()
                            .map(|p| (p.to_owned(), "OpenAlex (repository page)".to_owned()))
                    })
            })
            .collect::<Vec<_>>()
    };

    // Semantic Scholar's open-access PDF, by DOI or exact title
    // (api.semanticscholar.org/api-docs, `openAccessPdf.url`, checked 2026-09-25).
    let s2 = async {
        let base = env
            .source_base("semantic_scholar")
            .unwrap_or("https://api.semanticscholar.org");
        let key = env
            .secrets
            .get(&crate::sources::key_ref("semantic_scholar"))
            .ok()
            .flatten();
        let (url, query): (String, Vec<(&str, String)>) = match (&rec.doi, &rec.title) {
            (Some(d), _) => (
                format!("{base}/graph/v1/paper/DOI:{d}"),
                vec![("fields", "openAccessPdf".into())],
            ),
            (None, Some(t)) => (
                format!("{base}/graph/v1/paper/search/match"),
                vec![
                    ("query", t.clone()),
                    ("fields", "openAccessPdf,title".into()),
                ],
            ),
            _ => return Vec::new(),
        };
        let Ok(v) = crate::sources::adapter::get_json_with(quick, &env.http, &url, || {
            let rb = env.http.get(&url).query(&query);
            match &key {
                Some(k) => rb.header("x-api-key", k),
                None => rb,
            }
        })
        .await
        else {
            return Vec::new();
        };
        // The title match endpoint returns {"data": [paper]}.
        let paper = v["data"].as_array().and_then(|a| a.first()).unwrap_or(&v);
        if rec.doi.is_none() {
            let same = paper["title"]
                .as_str()
                .zip(rec.title.as_deref())
                .is_some_and(|(a, b)| {
                    strsim::jaro_winkler(
                        &crate::dedup::normalise_title(a),
                        &crate::dedup::normalise_title(b),
                    ) >= 0.95
                });
            if !same {
                return Vec::new();
            }
        }
        paper["openAccessPdf"]["url"]
            .as_str()
            .filter(|u| !u.is_empty())
            .map(|u| vec![(u.to_owned(), "Semantic Scholar".to_owned())])
            .unwrap_or_default()
    };

    let (a, b, c, d) = tokio::join!(unpaywall, core, openalex, s2);
    let mut urls: Vec<(String, String)> = Vec::new();
    if let Some(u) = &rec.pdf_url {
        urls.push((
            u.clone(),
            format!("open-access PDF listed by {}", rec.source),
        ));
    }
    urls.extend(a.into_iter().chain(c).chain(b).chain(d));
    let mut seen = std::collections::HashSet::new();
    urls.retain(|(u, _)| seen.insert(u.clone()));
    urls
}

/// Downloads the candidates in parallel and returns the first real PDF, in
/// candidate order of preference among those that succeed.
async fn first_pdf(
    client: &reqwest::Client,
    candidates: Vec<(String, String)>,
) -> Option<(Vec<u8>, String, String)> {
    let mut set = tokio::task::JoinSet::new();
    for (i, (url, via)) in candidates.into_iter().enumerate() {
        let client = client.clone();
        set.spawn(async move { (i, fetch_pdf(&client, &url).await, url, via) });
    }
    let mut best: Option<(usize, Vec<u8>, String, String)> = None;
    while let Some(Ok((i, bytes, url, via))) = set.join_next().await {
        if let Some(b) = bytes {
            if best.as_ref().is_none_or(|(j, ..)| i < *j) {
                best = Some((i, b, url, via));
            }
            // The most preferred source answered: stop waiting for the rest.
            if best.as_ref().is_some_and(|(j, ..)| *j == 0) {
                break;
            }
        }
    }
    set.abort_all();
    best.map(|(_, b, url, via)| (b, url, via))
}

pub struct Retrieve;

#[async_trait]
impl JobHandler for Retrieve {
    async fn run(&self, job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        let record_id = job.payload["record_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let (mut rec, report_id) = {
            let conn = ctx.open().map_err(db_err)?;
            let rec = records::record(&conn, &record_id)
                .map_err(db_err)?
                .ok_or_else(|| JobError::Fatal("record not found".into()))?;
            (rec, ensure_report(&conn, &record_id).map_err(db_err)?)
        };
        let dir = ctx.data_dir().join("pdfs");
        std::fs::create_dir_all(&dir).map_err(|e| JobError::Fatal(e.to_string()))?;
        let save = |bytes: &[u8],
                    status: &str,
                    rationale: String|
         -> std::result::Result<(), JobError> {
            let path = dir.join(format!("{report_id}.pdf"));
            std::fs::write(&path, bytes).map_err(|e| JobError::Fatal(e.to_string()))?;
            let conn = ctx.open().map_err(db_err)?;
            conn.execute(
                "UPDATE report SET pdf_path = ?1, retrieval_status = ?2, page_texts = NULL WHERE id = ?3",
                params![path.to_string_lossy(), status, report_id],
            )
            .map_err(|e| JobError::Fatal(e.to_string()))?;
            retrieval_event(&conn, &report_id, ActorType::Agent, status, &rationale).map_err(db_err)
        };
        let retry = job.payload["retry"].as_bool().unwrap_or(false);
        if retry {
            // Only reports still waiting for a copy are tried again.
            let conn = ctx.open().map_err(db_err)?;
            if !attachable(&conn).map_err(db_err)?.contains(&report_id) {
                return Ok(());
            }
        }
        let live = ctx.env.live.track(
            &ctx.review_id,
            &report_id,
            rec.title.as_deref().unwrap_or("Untitled record"),
        );
        // Records from ERIC and CORE often lack a DOI; look it up by exact title.
        let mut doi_note = String::new();
        if rec.doi.is_none() {
            live.step("Looking up the DOI by title (OpenAlex, then Crossref)");
            if let Some(title) = rec.title.clone() {
                let key = ctx
                    .env
                    .secrets
                    .get(&crate::sources::key_ref("openalex"))
                    .ok()
                    .flatten();
                let oa = OpenAlex::new(ctx.env.http.clone(), ctx.env.source_base("openalex"), key);
                if let Ok(Some(doi)) = oa.doi_by_title(&title, rec.year).await {
                    doi_note = format!(" (DOI {doi} found in OpenAlex by exact title)");
                    rec.doi = Some(doi);
                } else if let Some(doi) =
                    fulltext_access::crossref_doi(&ctx.env, &title, rec.year).await
                {
                    doi_note = format!(" (DOI {doi} found in Crossref by exact title)");
                    rec.doi = Some(doi);
                }
            }
        }
        let institutional = ctx
            .env
            .app_db()
            .ok()
            .and_then(|app| crate::settings::load(&app).ok())
            .is_some_and(|s| s.institutional_access);
        // Lookups run in parallel and fail fast, so a retry repeats them all:
        // sources change (new keys, new indexes) between rounds.
        let try_open = true;
        let found = if try_open {
            live.step("Checking Unpaywall, CORE, OpenAlex and Semantic Scholar");
            let cands = candidates(ctx, &rec).await;
            if !cands.is_empty() {
                live.step(format!(
                    "Downloading from {} open-access link(s)",
                    cands.len()
                ));
            }
            first_pdf(&ctx.env.http, cands).await
        } else {
            None
        };
        if let Some((bytes, url, via)) = found {
            live.done(true, format!("Found open access via {via}"));
            return save(&bytes, "open_access", format!("{via}: {url}{doi_note}"));
        }
        // Subscription access through the user's institution (see fulltext_access).
        if let Some(doi) = &rec.doi {
            if let Some(api) = fulltext_access::publisher_api_for(doi) {
                live.step(format!(
                    "Trying the {} download API",
                    if api == "elsevier" {
                        "Elsevier"
                    } else {
                        "Wiley"
                    }
                ));
            }
            if let Some((bytes, via)) = fulltext_access::via_publisher_api(&ctx.env, doi).await {
                live.done(true, "Found via the publisher's API");
                return save(&bytes, "proxy", format!("{via}{doi_note}"));
            }
            if institutional {
                live.step("Trying the publisher's site through your institution");
                if let Some((bytes, via)) = fulltext_access::via_institution(&ctx.env, doi).await {
                    live.done(true, "Found through your institution");
                    return save(&bytes, "proxy", format!("{via}{doi_note}"));
                }
            }
        }
        live.done(
            false,
            match (&rec.doi, institutional) {
                (None, _) => "No DOI and no open-access copy found".to_owned(),
                (Some(_), true) => {
                    "No open-access copy; the publisher did not give Sift the PDF".to_owned()
                }
                (Some(_), false) => "No open-access copy found".to_owned(),
            },
        );
        if retry {
            // Still waiting; the earlier queued_for_upload event stands.
            return Ok(());
        }
        let conn = ctx.open().map_err(db_err)?;
        retrieval_event(
            &conn,
            &report_id,
            ActorType::Agent,
            "queued_for_upload",
            if institutional {
                "No PDF found (record link, Unpaywall, CORE, OpenAlex, publisher APIs, institutional access). Waiting for an upload."
            } else {
                "No open-access PDF found (record link, Unpaywall, CORE, OpenAlex). Waiting for an upload."
            },
        )
        .map_err(db_err)?;
        Ok(())
    }
}

/// Tries the institutional routes again for every report waiting for an upload
/// (after a key was added or the VPN connected). Returns how many were queued.
pub fn retry_waiting(conn: &Connection, review_id: &str) -> Result<usize> {
    // Papers with a lookup still queued or running are not queued twice.
    let busy: std::collections::HashSet<String> = {
        let mut stmt = conn.prepare(
            "SELECT json_extract(payload, '$.record_id') FROM job WHERE kind = ?1 AND status IN ('queued', 'running')",
        )?;
        let rows = stmt.query_map([RETRIEVE_JOB], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let waiting = attachable(conn)?;
    let round: i64 = conn.query_row(
        "SELECT COUNT(*) FROM job WHERE kind = ?1 AND json_extract(payload, '$.retry') IS NOT NULL",
        [RETRIEVE_JOB],
        |r| r.get(0),
    )?;
    let mut queued = 0;
    for report_id in &waiting {
        let record_id: String = conn.query_row(
            "SELECT record_id FROM report WHERE id = ?1",
            [report_id],
            |r| r.get(0),
        )?;
        if busy.contains(&record_id) {
            continue;
        }
        queued += 1;
        crate::jobs::enqueue_once(
            conn,
            review_id,
            RETRIEVE_JOB,
            &serde_json::json!({"record_id": record_id, "retry": true, "round": round}),
        )?;
    }
    Ok(queued)
}

// ---------------------------------------------------------------------------
// Upload queue and text extraction

/// Reports a PDF can still be attached to: those waiting for one, and those
/// the retrieval deadline marked not retrieved. Reports the user marked not
/// retrieved are left alone.
pub fn attachable(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT rep.id FROM report rep JOIN v_latest_retrieval l ON l.report_id = rep.id
         WHERE l.decision = 'queued_for_upload'
            OR (l.decision = 'not_retrieved' AND NOT EXISTS (
                  SELECT 1 FROM event e WHERE e.stage = 'retrieval' AND e.target_id = rep.id
                    AND e.decision = 'not_retrieved' AND e.actor_type = 'human'))
         ORDER BY rep.rowid",
    )?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Reports waiting for the user: no open-access PDF found.
pub fn upload_queue(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT rep.id FROM report rep JOIN v_latest_retrieval l ON l.report_id = rep.id
         WHERE l.decision = 'queued_for_upload' ORDER BY rep.rowid",
    )?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Retrieved PDFs whose text the app window has not extracted yet.
pub fn pending_text(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM report WHERE pdf_path IS NOT NULL AND page_texts IS NULL AND retrieval_status IN ('open_access','proxy','uploaded') ORDER BY rowid",
    )?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn pdf_path(conn: &Connection, report_id: &str) -> Result<std::path::PathBuf> {
    let p: Option<String> = conn
        .query_row(
            "SELECT pdf_path FROM report WHERE id = ?1",
            [report_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    p.map(Into::into)
        .ok_or_else(|| Error::Invalid("This report has no PDF.".into()))
}

/// Stores pdf.js page texts. A PDF with no text layer goes back to the upload queue.
pub fn store_page_texts(conn: &Connection, report_id: &str, pages: &[String]) -> Result<bool> {
    let chars: usize = pages.iter().map(|p| p.trim().len()).sum();
    if chars < 200 {
        conn.execute(
            "UPDATE report SET page_texts = '[]', retrieval_status = 'pending' WHERE id = ?1",
            [report_id],
        )?;
        retrieval_event(
            conn,
            report_id,
            ActorType::Agent,
            "queued_for_upload",
            "The PDF has no usable text layer (scanned image?). Upload a text PDF or mark it not retrieved.",
        )?;
        return Ok(false);
    }
    conn.execute(
        "UPDATE report SET page_texts = ?1 WHERE id = ?2",
        params![serde_json::to_string(pages)?, report_id],
    )?;
    Ok(true)
}

/// The user uploads a PDF for a report.
pub fn upload(
    conn: &Connection,
    data_dir: &std::path::Path,
    report_id: &str,
    bytes: &[u8],
) -> Result<()> {
    if !bytes.starts_with(b"%PDF") {
        return Err(Error::Invalid("That file is not a PDF.".into()));
    }
    if bytes.len() > MAX_PDF_BYTES {
        return Err(Error::Invalid("The PDF is larger than 60 MB.".into()));
    }
    let dir = data_dir.join("pdfs");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{report_id}.pdf"));
    std::fs::write(&path, bytes)?;
    conn.execute(
        "UPDATE report SET pdf_path = ?1, retrieval_status = 'uploaded', page_texts = NULL WHERE id = ?2",
        params![path.to_string_lossy(), report_id],
    )?;
    retrieval_event(
        conn,
        report_id,
        ActorType::Human,
        "uploaded",
        "PDF uploaded by the user.",
    )
}

/// Reports with no PDF that the user can still supply one for, including
/// those marked not retrieved (by the deadline or by the user).
pub fn without_pdf(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT rep.id FROM report rep JOIN v_latest_retrieval l ON l.report_id = rep.id
         WHERE l.decision IN ('queued_for_upload', 'not_retrieved') ORDER BY rep.rowid",
    )?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Marks reports that have waited longer than `hours` for a PDF as not
/// retrieved (an automated, disclosed rule). Returns how many.
pub fn expire_waiting(conn: &Connection, hours: u32) -> Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT l.report_id FROM v_latest_retrieval l
         JOIN event e ON e.target_id = l.report_id AND e.stage = 'retrieval'
         WHERE l.decision = 'queued_for_upload'
         GROUP BY l.report_id
         HAVING (julianday('now') - julianday(MIN(e.created_at))) * 24 >= ?1",
    )?;
    let ids: Vec<String> = stmt
        .query_map([hours], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for id in &ids {
        conn.execute(
            "UPDATE report SET retrieval_status = 'not_retrieved' WHERE id = ?1",
            [id],
        )?;
        retrieval_event(
            conn,
            id,
            ActorType::Agent,
            "not_retrieved",
            &format!(
                "No full text found within the retrieval deadline ({hours} h) by any route tried: open-access indexes, publisher APIs, institutional access and uploads."
            ),
        )?;
    }
    Ok(ids.len())
}

/// The user gives up on a report (or on all waiting reports).
pub fn mark_not_retrieved(conn: &Connection, report_ids: &[String]) -> Result<usize> {
    mark_not_retrieved_by(
        conn,
        report_ids,
        ActorType::Human,
        "Marked not retrieved by the user.",
    )
}

/// `mark_not_retrieved` for a given actor; bypass mode uses an agent actor
/// with its own disclosed rationale.
pub fn mark_not_retrieved_by(
    conn: &Connection,
    report_ids: &[String],
    actor: ActorType,
    rationale: &str,
) -> Result<usize> {
    for id in report_ids {
        conn.execute(
            "UPDATE report SET retrieval_status = 'not_retrieved' WHERE id = ?1",
            [id],
        )?;
        retrieval_event(conn, id, actor, "not_retrieved", rationale)?;
    }
    Ok(report_ids.len())
}

/// URL to open a report through the user's library proxy, if one is set.
pub fn proxy_url(conn: &Connection, prefix: &str, report_id: &str) -> Result<String> {
    let record_id: String = conn.query_row(
        "SELECT record_id FROM report WHERE id = ?1",
        [report_id],
        |r| r.get(0),
    )?;
    let rec = records::record(conn, &record_id)?
        .ok_or_else(|| Error::Invalid("record not found".into()))?;
    let target = match (&rec.doi, &rec.landing_url) {
        (Some(d), _) => format!("https://doi.org/{d}"),
        (None, Some(u)) => u.clone(),
        _ => {
            return Err(Error::Invalid(
                "This record has no DOI or link to open.".into(),
            ))
        }
    };
    Ok(format!("{prefix}{target}"))
}

// ---------------------------------------------------------------------------
// Report-to-study linking (§8.7)

/// Trial and review registration identifiers found in text.
pub fn registration_ids(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let words: Vec<&str> = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    for w in words {
        let upper = w.to_ascii_uppercase();
        let digits = |p: &str, n: usize| {
            upper
                .strip_prefix(p)
                .is_some_and(|rest| rest.len() >= n && rest.chars().all(|c| c.is_ascii_digit()))
        };
        let registered = digits("NCT", 8)
            || digits("ISRCTN", 6)
            || digits("ACTRN", 10)
            || digits("CRD42", 6)
            || digits("CHICTR", 6);
        if registered && !out.contains(&upper) {
            out.push(upper);
        }
    }
    out
}

pub struct LinkStudies;

/// Groups included reports into studies: shared DOI, shared registration ID,
/// or same first-author surname, year within 1 and similar title. Every link is
/// an agent event; the user confirms links at Gate 3.
pub fn link(conn: &Connection) -> Result<usize> {
    let (review_id, _) = review_question(conn)?;
    let reps = {
        let mut stmt = conn.prepare(
            "SELECT id, record_id FROM v_included_report WHERE study_id IS NULL ORDER BY rep_order",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let existing = {
        let mut stmt = conn.prepare(
            "SELECT id, record_id, study_id FROM v_included_report WHERE study_id IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    struct Item {
        report: String,
        rec: RecordRow,
        regs: Vec<String>,
        study: Option<String>,
    }
    let mut items: Vec<Item> = Vec::new();
    for (rep, rid, sid) in existing
        .iter()
        .map(|(a, b, c)| (a, b, Some(c.clone())))
        .chain(reps.iter().map(|(a, b)| (a, b, None)))
    {
        let Some(rec) = records::record(conn, rid)? else {
            continue;
        };
        let pages = records::page_texts(conn, rep)?;
        let head: String = pages.iter().take(3).cloned().collect::<Vec<_>>().join(" ");
        items.push(Item {
            report: rep.clone(),
            rec,
            regs: registration_ids(&head),
            study: sid,
        });
    }
    let tx = conn.unchecked_transaction()?;
    let mut linked = 0;
    for i in 0..items.len() {
        if items[i].study.is_some() {
            continue;
        }
        let mut found: Option<(String, String)> = None;
        for j in 0..items.len() {
            if i == j {
                continue;
            }
            let Some(sid) = items[j].study.clone() else {
                continue;
            };
            let (a, b) = (&items[i], &items[j]);
            let reason = if a.rec.doi.is_some() && a.rec.doi == b.rec.doi {
                Some("same DOI".to_owned())
            } else if let Some(r) = a.regs.iter().find(|r| b.regs.contains(r)) {
                Some(format!("shared registration {r}"))
            } else {
                let same_author = match (a.rec.authors.first(), b.rec.authors.first()) {
                    (Some(x), Some(y)) => crate::dedup::surname(x) == crate::dedup::surname(y),
                    _ => false,
                };
                let year_close =
                    matches!((a.rec.year, b.rec.year), (Some(p), Some(q)) if (p - q).abs() <= 1);
                let title_sim = match (&a.rec.title, &b.rec.title) {
                    (Some(x), Some(y)) => strsim::jaro_winkler(
                        &crate::dedup::normalise_title(x),
                        &crate::dedup::normalise_title(y),
                    ),
                    _ => 0.0,
                };
                (same_author && year_close && title_sim >= 0.85)
                    .then(|| format!("same first author, year and similar title ({title_sim:.2})"))
            };
            if let Some(r) = reason {
                found = Some((sid, r));
                break;
            }
        }
        let (study_id, rationale) = match found {
            Some((sid, r)) => (sid, format!("Linked to an existing study: {r}.")),
            None => {
                let sid = new_id();
                tx.execute(
                    "INSERT INTO study (id, review_id, label) VALUES (?1, ?2, ?3)",
                    params![sid, review_id, items[i].rec.short_label()],
                )?;
                (sid, "New study.".to_owned())
            }
        };
        tx.execute(
            "UPDATE report SET study_id = ?1 WHERE id = ?2",
            params![study_id, items[i].report],
        )?;
        events::append(
            &tx,
            &NewEvent {
                review_id: review_id.clone(),
                target_type: "report".into(),
                target_id: items[i].report.clone(),
                stage: "link".into(),
                actor_type: Some(ActorType::Agent),
                actor_id: "linker".into(),
                decision: "link".into(),
                reason_code: Some(study_id.clone()),
                rationale: Some(rationale),
                ..Default::default()
            },
        )?;
        items[i].study = Some(study_id);
        linked += 1;
    }
    tx.commit()?;
    Ok(linked)
}

#[async_trait]
impl JobHandler for LinkStudies {
    async fn run(&self, _job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        let conn = ctx.open().map_err(db_err)?;
        link(&conn).map_err(db_err)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    #[tokio::test]
    #[ignore = "runs the real job against a copy of a live review (SIFT_REVIEW_COPY)"]
    async fn live_job_on_review_copy() {
        let Ok(src) = std::env::var("SIFT_REVIEW_COPY") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("copy.sift");
        std::fs::copy(&src, &db).unwrap();
        let app = dir.path().join("app.db");
        let conn = crate::db::open_app(&app).unwrap();
        crate::settings::save(
            &conn,
            crate::settings::AppSettings {
                unpaywall_email: Some("researcher@example.org".into()),
                institutional_access: true,
                ..Default::default()
            },
        )
        .unwrap();
        let review: String = crate::db::open_review(&db)
            .unwrap()
            .query_row("SELECT id FROM review", [], |r| r.get(0))
            .unwrap();
        let ctx = JobContext {
            db_path: db.clone(),
            review_id: review,
            env: crate::pipeline::env::Env::detached(app),
        };
        let job = Job {
            id: 1,
            review_id: ctx.review_id.clone(),
            kind: RETRIEVE_JOB.into(),
            payload: serde_json::json!({"record_id": std::env::var("SIFT_RECORD").unwrap(), "retry": true}),
            status: crate::jobs::JobStatus::Running,
            attempts: 1,
            last_error: None,
            updated_at: String::new(),
        };
        let t = std::time::Instant::now();
        let r = Retrieve.run(&job, &ctx).await;
        println!(
            "job result {:?} after {:?}",
            r.err().map(|e| format!("{e:?}")),
            t.elapsed()
        );
        println!("live: {:?}", ctx.env.live.snapshot(&ctx.review_id).recent);
    }

    #[tokio::test]
    #[ignore = "hits live repository sites"]
    async fn live_repository_page_to_pdf() {
        let client = reqwest::Client::builder()
            .user_agent(concat!("Sift/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap();
        if std::env::var("SIFT_DIRECT").is_ok() {
            let got = fetch_pdf(&client, "http://hdl.handle.net/10072/346274").await;
            println!("repository page -> {:?} bytes", got.as_ref().map(Vec::len));
        }
        let v: serde_json::Value = client
            .get("https://api.unpaywall.org/v2/10.1016/j.nedt.2017.05.019?email=researcher@example.org")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        println!("unpaywall locations: {}", v["oa_locations"]);

        // The full lookup, as the job runs it.
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("app.db");
        let conn = crate::db::open_app(&app).unwrap();
        crate::settings::save(
            &conn,
            crate::settings::AppSettings {
                unpaywall_email: Some("researcher@example.org".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let env = crate::pipeline::env::Env::detached(app);
        let ctx = JobContext {
            db_path: dir.path().join("r.sift"),
            review_id: "r".into(),
            env,
        };
        let rec = RecordRow {
            id: "x".into(),
            search_run_id: "s".into(),
            source: "openalex".into(),
            source_id: None,
            doi: Some("10.1016/j.nedt.2017.05.019".into()),
            title: Some("t".into()),
            abstract_: None,
            authors: vec![],
            year: Some(2017),
            venue: None,
            pdf_url: None,
            landing_url: None,
        };
        let t = std::time::Instant::now();
        let cands = candidates(&ctx, &rec).await;
        println!("candidates after {:?}: {cands:?}", t.elapsed());
        let found = first_pdf(&ctx.env.http, cands).await;
        println!(
            "first_pdf after {:?}: {:?}",
            t.elapsed(),
            found
                .as_ref()
                .map(|(b, u, v)| (b.len(), u.clone(), v.clone()))
        );
    }

    #[test]
    fn waiting_reports_expire_after_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let review = crate::reviews::insert_review(&conn, "Q?").unwrap();
        conn.execute(
            "INSERT INTO search_run (id, review_id, source, query_string, query_ast, run_at, hits) VALUES ('s1', ?1, 'eric', 'q', '{}', 'now', 1)",
            [&review],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO record (id, search_run_id, title) VALUES ('r1', 's1', 'T')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO report (id, record_id, retrieval_status) VALUES ('p1', 'r1', 'pending')",
            [],
        )
        .unwrap();
        retrieval_event(
            &conn,
            "p1",
            ActorType::Agent,
            "queued_for_upload",
            "waiting",
        )
        .unwrap();
        assert_eq!(
            expire_waiting(&conn, 168).unwrap(),
            0,
            "a fresh report keeps waiting"
        );
        assert_eq!(expire_waiting(&conn, 0).unwrap(), 1);
        assert!(upload_queue(&conn).unwrap().is_empty());
        // Deadline-expired reports can still take a PDF; ones the user gave up on cannot.
        assert_eq!(attachable(&conn).unwrap(), vec!["p1".to_owned()]);
        mark_not_retrieved(&conn, &["p1".to_owned()]).unwrap();
        assert!(attachable(&conn).unwrap().is_empty());
        let (actor, decision): (String, String) = conn
            .query_row(
                "SELECT actor_type, decision FROM event WHERE stage = 'retrieval' AND actor_type = 'agent' ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (actor.as_str(), decision.as_str()),
            ("agent", "not_retrieved")
        );
    }
    use super::*;

    #[test]
    fn finds_registration_ids() {
        let t = "Registered as ISRCTN12345678 and NCT01234567; review CRD42021234567. Not NCT123.";
        assert_eq!(
            registration_ids(t),
            vec!["ISRCTN12345678", "NCT01234567", "CRD42021234567"]
        );
    }
}
