//! Commands for running a review: protocol, run control, screening, full
//! text, extraction, synthesis, report and exports. Human actions write
//! events, then wake the review's worker so the pipeline moves on.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tauri::ipc::{InvokeBody, Request, Response};
use tauri::{AppHandle, Manager, State};

use super::CmdResult;
use crate::agents::protocol::{self, ProtocolBody, ProtocolVersion, ReviewOptions};
use crate::agents::{appraisal, extraction, screening, search, synthesis};
use crate::db;
use crate::error::Error;
use crate::events::Event;
use crate::jobs;
use crate::pipeline::{self, batch_upload, gate2, gate3, retrieval, Progress};
use crate::query::Rendered;
use crate::records::{self, Prisma, RecordRow, SearchRunRow};
use crate::report::{exports, flow, manuscript, metrics};
use crate::reviews::{self, ReviewSummary};
use crate::state::AppState;

fn open(
    state: &AppState,
    review_id: &str,
) -> crate::error::Result<(Connection, std::path::PathBuf)> {
    let path = reviews::path_of(&*state.app_db()?, review_id)?;
    Ok((db::open_review(&path)?, path))
}

fn conn(state: &AppState, review_id: &str) -> crate::error::Result<Connection> {
    open(state, review_id).map(|(c, _)| c)
}

/// After a human action: let the worker advance the pipeline.
fn wake(state: &AppState, review_id: &str) -> crate::error::Result<()> {
    state.ensure_worker(review_id)
}

// ---------------------------------------------------------------------------
// Start and protocol

#[tauri::command]
pub async fn start_review(
    state: State<'_, AppState>,
    question: String,
    options: ReviewOptions,
) -> CmdResult<ReviewSummary> {
    let summary = reviews::create(&*state.app_db()?, &state.reviews_dir, &question)?;
    let c = db::open_review(std::path::Path::new(&summary.path))?;
    let mut options = options;
    options
        .sources
        .retain(|s| search::SEARCHABLE.contains(&s.as_str()));
    if options.sources.is_empty() {
        return Err(Error::Invalid("Choose at least one database.".into()).into());
    }
    protocol::save_options(&c, &options)?;
    let settings = crate::settings::load(&*state.app_db()?)?;
    if settings.bypass_by_default {
        pipeline::bypass::set(&c, &summary.id, true)?;
    }
    // The author's standing funding and competing-interests statements
    // (PRISMA items 25 and 26) start each review; the author can edit them.
    for (key, text) in [
        ("item25_support", &settings.funding_statement),
        ("item26_competing", &settings.competing_interests),
    ] {
        if let Some(t) = text.as_deref().filter(|t| !t.trim().is_empty()) {
            gate3::set_author_text(&c, key, t)?;
        }
    }
    wake(&state, &summary.id)?;
    Ok(summary)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolView {
    pub question: String,
    pub draft: Option<ProtocolVersion>,
    pub locked: Option<ProtocolVersion>,
    pub options: ReviewOptions,
    pub previews: Vec<(String, Rendered)>,
}

#[tauri::command]
pub async fn get_protocol(
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<ProtocolView> {
    let c = conn(&state, &review_id)?;
    let (_, question) = protocol::review_question(&c)?;
    Ok(ProtocolView {
        question,
        draft: protocol::draft(&c)?,
        locked: protocol::locked(&c)?,
        options: protocol::options(&c)?,
        previews: search::preview(&c, &state.http).unwrap_or_default(),
    })
}

#[tauri::command]
pub async fn save_protocol(
    state: State<'_, AppState>,
    review_id: String,
    body: ProtocolBody,
) -> CmdResult<ProtocolVersion> {
    Ok(protocol::save_draft_edit(
        &conn(&state, &review_id)?,
        &body,
    )?)
}

#[tauri::command]
pub async fn set_review_options(
    state: State<'_, AppState>,
    review_id: String,
    options: ReviewOptions,
) -> CmdResult<()> {
    let c = conn(&state, &review_id)?;
    if protocol::locked(&c)?.is_some() {
        return Err(Error::Invalid(
            "The protocol is locked; search options can no longer change.".into(),
        )
        .into());
    }
    Ok(protocol::save_options(&c, &options)?)
}

#[tauri::command]
pub async fn lock_protocol(
    state: State<'_, AppState>,
    review_id: String,
    reviewed: bool,
) -> CmdResult<ProtocolVersion> {
    let v = protocol::lock(&conn(&state, &review_id)?, reviewed)?;
    wake(&state, &review_id)?;
    Ok(v)
}

// ---------------------------------------------------------------------------
// Run control and overview

#[tauri::command]
pub async fn review_progress(state: State<'_, AppState>, review_id: String) -> CmdResult<Progress> {
    Ok(pipeline::progress(&conn(&state, &review_id)?, &review_id)?)
}

#[tauri::command]
pub async fn pause_review(state: State<'_, AppState>, review_id: String) -> CmdResult<()> {
    Ok(jobs::pause_run(
        &conn(&state, &review_id)?,
        "Paused by you.",
        false,
    )?)
}

#[tauri::command]
pub async fn resume_review(state: State<'_, AppState>, review_id: String) -> CmdResult<()> {
    jobs::resume_run(&conn(&state, &review_id)?)?;
    Ok(wake(&state, &review_id)?)
}

#[tauri::command]
pub async fn skip_citation_chasing(
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<pipeline::chasing::Skipped> {
    let s = pipeline::chasing::skip(&conn(&state, &review_id)?, &review_id)?;
    wake(&state, &review_id)?;
    Ok(s)
}

/// Stops a stage and keeps what it already did (see `pipeline::skip`).
#[tauri::command]
pub async fn skip_stage(
    state: State<'_, AppState>,
    review_id: String,
    stage: String,
) -> CmdResult<pipeline::skip::Skip> {
    let s = pipeline::skip::skip(&conn(&state, &review_id)?, &review_id, &stage)?;
    wake(&state, &review_id)?;
    Ok(s)
}

#[tauri::command]
pub async fn resume_stage(
    state: State<'_, AppState>,
    review_id: String,
    stage: String,
) -> CmdResult<()> {
    pipeline::skip::resume(&conn(&state, &review_id)?, &review_id, &stage)?;
    wake(&state, &review_id)?;
    Ok(())
}

#[tauri::command]
pub async fn retry_failed_jobs(state: State<'_, AppState>, review_id: String) -> CmdResult<usize> {
    let n = jobs::retry_failed(&conn(&state, &review_id)?)?;
    wake(&state, &review_id)?;
    Ok(n)
}

#[tauri::command]
pub async fn dismiss_failed_jobs(
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<usize> {
    let n = jobs::dismiss_failed(&conn(&state, &review_id)?)?;
    wake(&state, &review_id)?;
    Ok(n)
}

#[tauri::command]
pub async fn get_prisma(state: State<'_, AppState>, review_id: String) -> CmdResult<Prisma> {
    Ok(records::prisma(&conn(&state, &review_id)?)?)
}

#[tauri::command]
pub async fn prisma_svg(state: State<'_, AppState>, review_id: String) -> CmdResult<String> {
    Ok(flow::svg(&records::prisma(&conn(&state, &review_id)?)?))
}

#[tauri::command]
pub async fn quality_checks(
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<metrics::Quality> {
    Ok(metrics::quality(&conn(&state, &review_id)?)?)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub id: i64,
    pub time: String,
    pub actor: String,
    pub human: bool,
    pub stage: String,
    pub decision: String,
    pub target: String,
    pub detail: String,
}

fn target_label(c: &Connection, e: &Event) -> String {
    let q = |sql: &str| -> Option<String> {
        c.query_row(sql, [&e.target_id], |r| r.get::<_, Option<String>>(0))
            .ok()
            .flatten()
    };
    match e.target_type.as_str() {
        "record" => q("SELECT title FROM record WHERE id = ?1"),
        "report" => q("SELECT r.title FROM report rep JOIN record r ON r.id = rep.record_id WHERE rep.id = ?1"),
        "study" => q("SELECT label FROM study WHERE id = ?1"),
        "search_run" => q("SELECT source FROM search_run WHERE id = ?1"),
        "extraction_value" => q("SELECT s.label || ' · ' || ev.field FROM extraction_value ev JOIN study s ON s.id = ev.study_id WHERE ev.id = ?1"),
        "synthesis" => q("SELECT label FROM synthesis_item WHERE id = ?1"),
        _ => None,
    }
    .unwrap_or_default()
}

fn actor_label(e: &Event) -> String {
    match e.actor_id.as_str() {
        "human" => "You".into(),
        "screener_a" => "Screener A".into(),
        "screener_b" => "Screener B".into(),
        "arbiter" => "Arbiter".into(),
        "resolver" => "Agreement".into(),
        "protocol_agent" => "Protocol agent".into(),
        "search_agent" => "Search agent".into(),
        "dedup" => "Deduplication".into(),
        "retrieval_agent" => "Retrieval".into(),
        "linker" => "Linker".into(),
        "extraction_agent" => "Extraction agent".into(),
        "appraiser_a" => "Appraiser A".into(),
        "appraiser_b" => "Appraiser B".into(),
        "synthesis_agent" => "Synthesis agent".into(),
        "calibration" => "Calibration".into(),
        "drift_check" => "Drift check".into(),
        "laya" => "Local pre-screen (Laya)".into(),
        other => other.into(),
    }
}

#[tauri::command]
pub async fn decision_log(
    state: State<'_, AppState>,
    review_id: String,
    limit: i64,
    stages: Option<Vec<String>>,
) -> CmdResult<Vec<LogEntry>> {
    let c = conn(&state, &review_id)?;
    // Blinded events (the Gate 2 sample and the user's calibration answers) stay out of the feed.
    // `stages` narrows the feed to one screen's event stages; None means all.
    let mut stmt = c.prepare(
        "SELECT * FROM event WHERE blinded = 0
           AND (?2 IS NULL OR stage IN (SELECT value FROM json_each(?2)))
         ORDER BY id DESC LIMIT ?1",
    )?;
    let stages = stages
        .map(|s| serde_json::to_string(&s))
        .transpose()
        .map_err(|e| Error::Invalid(e.to_string()))?;
    let events: Vec<Event> = stmt
        .query_map(rusqlite::params![limit.clamp(1, 500), stages], Event::from_row)?
        .collect::<rusqlite::Result<_>>()
        .map_err(Error::from)?;
    Ok(events
        .iter()
        .map(|ev| LogEntry {
            id: ev.id,
            time: ev.created_at.clone(),
            actor: actor_label(ev),
            human: ev.actor_type == "human",
            stage: ev.stage.clone(),
            decision: ev.decision.clone(),
            target: target_label(&c, ev),
            detail: [ev.reason_code.clone(), ev.rationale.clone()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · "),
        })
        .collect())
}

#[tauri::command]
pub async fn list_search_runs(
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<Vec<SearchRunRow>> {
    Ok(records::runs(&conn(&state, &review_id)?)?)
}

#[tauri::command]
pub async fn import_records(
    state: State<'_, AppState>,
    review_id: String,
    label: String,
    text: String,
) -> CmdResult<usize> {
    let c = conn(&state, &review_id)?;
    if protocol::locked(&c)?.is_none() {
        return Err(Error::Invalid("Lock the protocol before importing records.".into()).into());
    }
    let format = crate::sources::import::Format::detect(&text).ok_or_else(|| {
        Error::Invalid(
            "Could not recognise the file. Use RIS, BibTeX or PubMed (NBIB) format.".into(),
        )
    })?;
    let n = search::import(&c, &label, format, &text)?;
    wake(&state, &review_id)?;
    Ok(n)
}

// ---------------------------------------------------------------------------
// Screening and Gate 2

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Calibration {
    pub sample: Vec<(String, Option<String>)>,
    pub outcome: Option<String>,
    pub metrics: Option<gate2::Metrics>,
    pub manual: bool,
}

#[tauri::command]
pub async fn calibration(state: State<'_, AppState>, review_id: String) -> CmdResult<Calibration> {
    let c = conn(&state, &review_id)?;
    let sample = gate2::sample(&c)?;
    let mut judged: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    {
        let mut stmt = c.prepare(
            "SELECT target_id, decision FROM v_current_event WHERE stage = 'gate2' AND actor_id = 'human' AND decision IN ('include','exclude','uncertain')",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (id, d) = row.map_err(Error::from)?;
            judged.insert(id, d);
        }
    }
    let outcome = gate2::outcome(&c)?;
    // Agent decisions stay hidden until every sampled record is judged (§10.4).
    let all_done = !sample.is_empty() && sample.iter().all(|(id, _)| judged.contains_key(id));
    Ok(Calibration {
        sample: sample
            .iter()
            .map(|(id, _)| (id.clone(), judged.get(id).cloned()))
            .collect(),
        metrics: if all_done || outcome.is_some() {
            Some(gate2::metrics(&c)?)
        } else {
            None
        },
        outcome,
        manual: gate2::manual_ta(&c)?,
    })
}

/// A record without any agent decision (for blind screening).
#[tauri::command]
pub async fn get_record(
    state: State<'_, AppState>,
    review_id: String,
    record_id: String,
) -> CmdResult<RecordRow> {
    Ok(gate2::blind_record(&conn(&state, &review_id)?, &record_id)?)
}

#[tauri::command]
pub async fn calibration_judge(
    state: State<'_, AppState>,
    review_id: String,
    record_id: String,
    decision: String,
    reason_code: Option<String>,
) -> CmdResult<()> {
    Ok(gate2::judge(
        &conn(&state, &review_id)?,
        &record_id,
        &decision,
        reason_code.as_deref(),
    )?)
}

#[tauri::command]
pub async fn calibration_finish(
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<gate2::Metrics> {
    let m = gate2::finish(&conn(&state, &review_id)?)?;
    wake(&state, &review_id)?;
    Ok(m)
}

#[tauri::command]
pub async fn skip_gate(state: State<'_, AppState>, review_id: String, gate: u8) -> CmdResult<()> {
    let c = conn(&state, &review_id)?;
    match gate {
        1 => {
            protocol::lock(&c, false)?;
        }
        2 => gate2::skip(&c)?,
        3 => gate3::skip(&c)?,
        _ => return Err(Error::Invalid("There are three gates.".into()).into()),
    }
    wake(&state, &review_id)?;
    Ok(())
}

/// Bypass mode: the pipeline settles every point that would wait for the
/// user, and the review stays unverified.
#[tauri::command]
pub async fn set_bypass(state: State<'_, AppState>, review_id: String, on: bool) -> CmdResult<()> {
    pipeline::bypass::set(&conn(&state, &review_id)?, &review_id, on)?;
    Ok(wake(&state, &review_id)?)
}

#[tauri::command]
pub async fn get_bypass(state: State<'_, AppState>, review_id: String) -> CmdResult<bool> {
    Ok(pipeline::bypass::on(&conn(&state, &review_id)?)?)
}

#[tauri::command]
pub async fn hand_screening_to_me(state: State<'_, AppState>, review_id: String) -> CmdResult<()> {
    gate2::hand_screening_to_user(&conn(&state, &review_id)?)?;
    Ok(wake(&state, &review_id)?)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub target_id: String,
    pub record: RecordRow,
    pub screener_a: Option<Event>,
    pub screener_b: Option<Event>,
    pub arbiter: Option<Event>,
}

/// Sends queue items where an agent gave no usable answer back to the agents.
#[tauri::command]
pub async fn retry_queue_with_agents(
    state: State<'_, AppState>,
    review_id: String,
    stage: String,
) -> CmdResult<usize> {
    let n = screening::retry_with_agents(&conn(&state, &review_id)?, &review_id, &stage)?;
    wake(&state, &review_id)?;
    Ok(n)
}

/// How many queue items can be sent back to the agents.
#[tauri::command]
pub async fn retryable_count(
    state: State<'_, AppState>,
    review_id: String,
    stage: String,
) -> CmdResult<usize> {
    Ok(screening::retryable_items(&conn(&state, &review_id)?, &stage)?.len())
}

#[tauri::command]
pub async fn human_queue(
    state: State<'_, AppState>,
    review_id: String,
    stage: String,
) -> CmdResult<Vec<QueueItem>> {
    let c = conn(&state, &review_id)?;
    let ids = if stage == "ta_screen" && gate2::manual_ta(&c)? {
        gate2::manual_queue(&c)?
    } else {
        screening::human_queue(&c, &stage)?
    };
    let mut out = Vec::new();
    for id in ids {
        let record_id = if stage == "ft_screen" {
            c.query_row("SELECT record_id FROM report WHERE id = ?1", [&id], |r| {
                r.get::<_, String>(0)
            })
            .map_err(Error::from)?
        } else {
            id.clone()
        };
        let Some(record) = records::record(&c, &record_id)? else {
            continue;
        };
        out.push(QueueItem {
            screener_a: screening::latest_by(&c, &stage, &id, "screener_a")?,
            screener_b: screening::latest_by(&c, &stage, &id, "screener_b")?,
            arbiter: screening::latest_by(&c, &stage, &id, "arbiter")?,
            target_id: id,
            record,
        });
    }
    Ok(out)
}

#[tauri::command]
pub async fn human_decide(
    state: State<'_, AppState>,
    review_id: String,
    stage: String,
    target_id: String,
    decision: String,
    reason_code: Option<String>,
    note: Option<String>,
) -> CmdResult<()> {
    let target_type = if stage == "ft_screen" {
        "report"
    } else {
        "record"
    };
    screening::human_decide(
        &conn(&state, &review_id)?,
        &stage,
        target_type,
        &target_id,
        &decision,
        reason_code.as_deref(),
        note.as_deref(),
    )?;
    Ok(wake(&state, &review_id)?)
}

// ---------------------------------------------------------------------------
// Full text

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportView {
    pub report_id: String,
    pub record: RecordRow,
    pub retrieval: Option<String>,
    pub retrieval_note: Option<String>,
    pub pages: i64,
    pub ft_decision: Option<String>,
    pub ft_reason: Option<String>,
    pub study_id: Option<String>,
    /// The agents could not decide; the full-text decision is the user's.
    pub needs_you: bool,
}

#[tauri::command]
pub async fn list_reports(
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<Vec<ReportView>> {
    let c = conn(&state, &review_id)?;
    let queue: std::collections::HashSet<String> = screening::human_queue(&c, "ft_screen")?
        .into_iter()
        .collect();
    let mut out = Vec::new();
    for r in records::reports(&c)? {
        let Some(record) = records::record(&c, &r.record_id)? else {
            continue;
        };
        let latest: Option<(String, Option<String>)> = c
            .query_row(
                "SELECT decision, rationale FROM event WHERE stage = 'retrieval' AND target_id = ?1 ORDER BY id DESC LIMIT 1",
                [&r.id],
                |x| Ok((x.get(0)?, x.get(1)?)),
            )
            .ok();
        let ft: Option<(String, Option<String>)> = c
            .query_row("SELECT decision, reason_code FROM v_final_decision WHERE stage = 'ft_screen' AND target_id = ?1", [&r.id], |x| Ok((x.get(0)?, x.get(1)?)))
            .ok();
        let needs_you = queue.contains(&r.id);
        out.push(ReportView {
            needs_you,
            report_id: r.id,
            record,
            retrieval: latest.as_ref().map(|l| l.0.clone()),
            retrieval_note: latest.and_then(|l| l.1),
            pages: r.pages,
            ft_decision: ft.as_ref().map(|f| f.0.clone()),
            ft_reason: ft.and_then(|f| f.1),
            study_id: r.study_id,
        });
    }
    Ok(out)
}

/// Binary upload: body is the PDF, headers carry the review and report ids.
#[tauri::command]
pub async fn upload_report_pdf(state: State<'_, AppState>, request: Request<'_>) -> CmdResult<()> {
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let (Some(review_id), Some(report_id)) = (header("x-review-id"), header("x-report-id")) else {
        return Err(Error::Invalid("missing review or report id".into()).into());
    };
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err(Error::Invalid("expected the PDF bytes".into()).into());
    };
    let (c, path) = open(&state, &review_id)?;
    let data_dir = path.with_file_name(format!(
        "{}.files",
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    retrieval::upload(&c, &data_dir, &report_id, bytes)?;
    Ok(wake(&state, &review_id)?)
}

/// Batch upload, step one. Body is the PDF; headers carry the review id and
/// the file name (percent-encoded, so any name survives the header). Attaches
/// by DOI straight away, or stages the file for a text match.
#[tauri::command]
pub async fn batch_upload_match(
    state: State<'_, AppState>,
    request: Request<'_>,
) -> CmdResult<BatchUploadResult> {
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let (Some(review_id), Some(filename)) = (header("x-review-id"), header("x-filename")) else {
        return Err(Error::Invalid("missing review id or file name".into()).into());
    };
    let filename = percent_decode(&filename);
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err(Error::Invalid("expected the PDF bytes".into()).into());
    };
    let (c, path) = open(&state, &review_id)?;
    Ok(
        match batch_upload::stage(&c, &data_dir_of(&path), &filename, bytes)? {
            batch_upload::Staged::Matched(report_id, title) => {
                wake(&state, &review_id)?;
                BatchUploadResult::matched(report_id, title)
            }
            batch_upload::Staged::Waiting(id) => BatchUploadResult {
                staged_id: Some(id),
                ..Default::default()
            },
        },
    )
}

/// Batch upload, step two: the text pdf.js read from a staged file's first pages.
#[tauri::command]
pub async fn batch_match_staged(
    state: State<'_, AppState>,
    review_id: String,
    staged_id: String,
    filename: String,
    text: String,
) -> CmdResult<BatchUploadResult> {
    let (c, path) = open(&state, &review_id)?;
    Ok(
        match batch_upload::match_staged(&c, &data_dir_of(&path), &staged_id, &filename, &text)? {
            Some((report_id, title)) => {
                wake(&state, &review_id)?;
                BatchUploadResult::matched(report_id, title)
            }
            None => BatchUploadResult {
                staged_id: Some(staged_id),
                ..Default::default()
            },
        },
    )
}

/// Batch upload: the user picks the record for a file that did not match.
#[tauri::command]
pub async fn batch_assign_staged(
    state: State<'_, AppState>,
    review_id: String,
    staged_id: String,
    filename: String,
    report_id: String,
) -> CmdResult<BatchUploadResult> {
    let (c, path) = open(&state, &review_id)?;
    let title =
        batch_upload::assign_staged(&c, &data_dir_of(&path), &staged_id, &filename, &report_id)?;
    wake(&state, &review_id)?;
    Ok(BatchUploadResult::matched(report_id, title))
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BatchUploadResult {
    pub matched: bool,
    pub report_id: Option<String>,
    pub title: Option<String>,
    /// Set while the file waits for a text match or the user's choice.
    pub staged_id: Option<String>,
}

impl BatchUploadResult {
    fn matched(report_id: String, title: String) -> Self {
        Self {
            matched: true,
            report_id: Some(report_id),
            title: Some(title),
            staged_id: None,
        }
    }
}

/// Decodes `encodeURIComponent` output; invalid sequences are kept as typed.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[tauri::command]
pub async fn mark_not_retrieved(
    state: State<'_, AppState>,
    review_id: String,
    report_ids: Vec<String>,
) -> CmdResult<usize> {
    let n = retrieval::mark_not_retrieved(&conn(&state, &review_id)?, &report_ids)?;
    wake(&state, &review_id)?;
    Ok(n)
}

#[tauri::command]
pub async fn open_via_proxy(
    state: State<'_, AppState>,
    review_id: String,
    report_id: String,
) -> CmdResult<()> {
    let prefix = crate::settings::load(&*state.app_db()?)?
        .library_proxy_prefix
        .ok_or_else(|| Error::Invalid("Set your library proxy prefix in Settings first.".into()))?;
    let url = retrieval::proxy_url(&conn(&state, &review_id)?, &prefix, &report_id)?;
    super::settings::open_url_unchecked(&url).map_err(|e| Error::Io(e).into())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingText {
    pub review_id: String,
    pub report_id: String,
}

/// PDFs waiting for text extraction, across all reviews.
#[tauri::command]
pub async fn pending_pdf_texts(state: State<'_, AppState>) -> CmdResult<Vec<PendingText>> {
    let mut out = Vec::new();
    for r in reviews::list(&*state.app_db()?)? {
        let c = db::open_review(std::path::Path::new(&r.path))?;
        for report_id in retrieval::pending_text(&c)? {
            out.push(PendingText {
                review_id: r.id.clone(),
                report_id,
            });
        }
    }
    Ok(out)
}

#[tauri::command]
pub async fn read_report_pdf(
    state: State<'_, AppState>,
    review_id: String,
    report_id: String,
) -> CmdResult<Response> {
    let path = retrieval::pdf_path(&conn(&state, &review_id)?, &report_id)?;
    let bytes = std::fs::read(path).map_err(Error::from)?;
    Ok(Response::new(bytes))
}

#[tauri::command]
pub async fn store_page_texts(
    state: State<'_, AppState>,
    review_id: String,
    report_id: String,
    pages: Vec<String>,
) -> CmdResult<bool> {
    let ok = retrieval::store_page_texts(&conn(&state, &review_id)?, &report_id, &pages)?;
    wake(&state, &review_id)?;
    Ok(ok)
}

#[tauri::command]
pub async fn report_pages(
    state: State<'_, AppState>,
    review_id: String,
    report_id: String,
) -> CmdResult<Vec<String>> {
    Ok(records::page_texts(&conn(&state, &review_id)?, &report_id)?)
}

// ---------------------------------------------------------------------------
// Extraction, appraisal, synthesis

#[tauri::command]
pub async fn list_studies(
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<Vec<manuscript::StudyRow>> {
    Ok(manuscript::build(&conn(&state, &review_id)?)?.studies)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StudyExtraction {
    pub values: Vec<extraction::ValueRow>,
    pub report_id: Option<String>,
    pub fields: Vec<extraction::FieldSpec>,
}

#[tauri::command]
pub async fn study_extraction(
    state: State<'_, AppState>,
    review_id: String,
    study_id: String,
) -> CmdResult<StudyExtraction> {
    let c = conn(&state, &review_id)?;
    let p = protocol::require_locked(&c)?.body;
    Ok(StudyExtraction {
        values: extraction::values(&c, Some(&study_id))?,
        report_id: extraction::primary_report(&c, &study_id)?,
        fields: extraction::form(&p),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveInput {
    value_id: String,
    value: Option<String>,
    quote: Option<String>,
    page: Option<i64>,
}

#[tauri::command]
pub async fn resolve_value(
    state: State<'_, AppState>,
    review_id: String,
    input: ResolveInput,
) -> CmdResult<extraction::ValueRow> {
    let v = extraction::resolve(
        &conn(&state, &review_id)?,
        &input.value_id,
        input.value.as_deref(),
        input.quote.as_deref(),
        input.page,
    )?;
    wake(&state, &review_id)?;
    Ok(v)
}

#[tauri::command]
pub async fn list_appraisals(
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<Vec<appraisal::AppraisalRow>> {
    Ok(appraisal::rows(&conn(&state, &review_id)?)?)
}

#[tauri::command]
pub async fn synthesis_items(
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<Vec<synthesis::Item>> {
    Ok(synthesis::items(&conn(&state, &review_id)?)?)
}

#[tauri::command]
pub async fn confirm_certainty(
    state: State<'_, AppState>,
    review_id: String,
    item_id: String,
    overall: String,
    note: Option<String>,
) -> CmdResult<()> {
    Ok(synthesis::confirm_certainty(
        &conn(&state, &review_id)?,
        &item_id,
        &overall,
        note.as_deref(),
    )?)
}

// ---------------------------------------------------------------------------
// Report and Gate 3

#[tauri::command]
pub async fn get_manuscript(
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<manuscript::Manuscript> {
    let mut m = manuscript::build(&conn(&state, &review_id)?)?;
    manuscript::apply_settings(&mut m, &crate::settings::load(&*state.app_db()?)?);
    Ok(m)
}

#[tauri::command]
pub async fn get_checklist(
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<manuscript::ChecklistSummary> {
    let mut m = manuscript::build(&conn(&state, &review_id)?)?;
    manuscript::apply_settings(&mut m, &crate::settings::load(&*state.app_db()?)?);
    Ok(manuscript::checklist(&m))
}

#[tauri::command]
pub async fn get_disclosure(
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<metrics::Disclosure> {
    Ok(metrics::disclosure(&conn(&state, &review_id)?)?)
}

#[tauri::command]
pub async fn gate3_requirements(
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<gate3::Requirements> {
    Ok(gate3::requirements(&conn(&state, &review_id)?)?)
}

#[tauri::command]
pub async fn set_author_text(
    state: State<'_, AppState>,
    review_id: String,
    key: String,
    text: String,
) -> CmdResult<()> {
    let allowed = gate3::AUTHOR_ITEMS.iter().any(|(k, _)| *k == key) || key.starts_with("item");
    if !allowed {
        return Err(Error::Invalid("unknown section".into()).into());
    }
    Ok(gate3::set_author_text(
        &conn(&state, &review_id)?,
        &key,
        &text,
    )?)
}

#[tauri::command]
pub async fn confirm_links(state: State<'_, AppState>, review_id: String) -> CmdResult<()> {
    Ok(gate3::confirm_links(&conn(&state, &review_id)?)?)
}

#[tauri::command]
pub async fn sign_off(state: State<'_, AppState>, review_id: String) -> CmdResult<()> {
    gate3::sign_off(&conn(&state, &review_id)?)?;
    Ok(wake(&state, &review_id)?)
}

fn export_dir(app: &AppHandle, review: &ReviewSummary) -> crate::error::Result<std::path::PathBuf> {
    let docs = app
        .path()
        .document_dir()
        .map_err(|e| Error::Invalid(e.to_string()))?;
    let short: String = review
        .question
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .take(8)
        .collect::<Vec<_>>()
        .join(" ");
    let dir = docs
        .join("Sift exports")
        .join(format!("{short} ({})", &review.id[..8]));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn review(state: &AppState, review_id: &str) -> crate::error::Result<ReviewSummary> {
    reviews::read_summary(&reviews::path_of(&*state.app_db()?, review_id)?)
}

/// Writes a Rust-generated export (ris, csv, svg) and returns its path.
#[tauri::command]
pub async fn export_file(
    app: AppHandle,
    state: State<'_, AppState>,
    review_id: String,
    kind: String,
) -> CmdResult<String> {
    let c = conn(&state, &review_id)?;
    let status = pipeline::verification(&c)?;
    let (name, content) = match kind.as_str() {
        "ris" => ("included-studies.ris", exports::ris(&c)?),
        "missing_ris" => ("papers-missing-pdfs.ris", exports::missing_ris(&c)?),
        "csv" => ("decision-log.csv", exports::events_csv(&c)?),
        "svg" => (
            "prisma-flow-diagram.svg",
            flow::svg(&records::prisma(&c)?).replace(
                "PRISMA 2020 flow diagram</text>",
                &format!("PRISMA 2020 flow diagram · {}</text>", status.label),
            ),
        ),
        _ => return Err(Error::Invalid("unknown export".into()).into()),
    };
    let path = export_dir(&app, &review(&state, &review_id)?)?.join(name);
    std::fs::write(&path, content).map_err(Error::from)?;
    Ok(path.to_string_lossy().into_owned())
}

/// Binary save for files made in the window (DOCX). Header `x-file-name`.
#[tauri::command]
pub async fn save_export(
    app: AppHandle,
    state: State<'_, AppState>,
    request: Request<'_>,
) -> CmdResult<String> {
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let (Some(review_id), Some(name)) = (header("x-review-id"), header("x-file-name")) else {
        return Err(Error::Invalid("missing review id or file name".into()).into());
    };
    let safe: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    if safe.is_empty() || safe.starts_with('.') {
        return Err(Error::Invalid("bad file name".into()).into());
    }
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err(Error::Invalid("expected file bytes".into()).into());
    };
    let path = export_dir(&app, &review(&state, &review_id)?)?.join(safe);
    std::fs::write(&path, bytes).map_err(Error::from)?;
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn reveal_exports(
    app: AppHandle,
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<()> {
    let dir = export_dir(&app, &review(&state, &review_id)?)?;
    super::settings::open_url_unchecked(&dir.to_string_lossy()).map_err(|e| Error::Io(e).into())
}

// ---------------------------------------------------------------------------
// Institutional and browser-assisted retrieval

fn data_dir_of(path: &std::path::Path) -> std::path::PathBuf {
    path.with_file_name(format!(
        "{}.files",
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    ))
}

/// Tries publisher APIs and institutional access again for waiting reports.
#[tauri::command]
pub async fn retry_retrieval(state: State<'_, AppState>, review_id: String) -> CmdResult<usize> {
    let n = retrieval::retry_waiting(&conn(&state, &review_id)?, &review_id)?;
    wake(&state, &review_id)?;
    Ok(n)
}

/// Opens the next `count` waiting reports in the user's browser (through the
/// library proxy when one is set). Returns how many were opened.
#[tauri::command]
pub async fn open_upload_batch(
    state: State<'_, AppState>,
    review_id: String,
    count: usize,
) -> CmdResult<usize> {
    let proxy = crate::settings::load(&*state.app_db()?)?.library_proxy_prefix;
    let next = crate::pipeline::downloads::next_to_open(
        &conn(&state, &review_id)?,
        proxy.as_deref(),
        count.clamp(1, 10),
    )?;
    for (_, url) in &next {
        super::settings::open_url_unchecked(url).map_err(Error::Io)?;
    }
    Ok(next.len())
}

/// Attaches new PDFs from the Downloads folder to the waiting reports they match.
#[tauri::command]
pub async fn scan_downloads(
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<crate::pipeline::downloads::ScanOutcome> {
    let dir = crate::pipeline::downloads::downloads_dir()
        .ok_or_else(|| Error::Invalid("No Downloads folder.".into()))?;
    let (c, path) = open(&state, &review_id)?;
    let out = crate::pipeline::downloads::scan(&c, &data_dir_of(&path), &dir)?;
    if !out.attached.is_empty() {
        wake(&state, &review_id)?;
    }
    Ok(out)
}

/// The bytes of a PDF in the Downloads folder, for pdf.js to read its first pages.
#[tauri::command]
pub async fn read_download(path: String) -> CmdResult<Response> {
    let p = crate::pipeline::downloads::checked_download(&path)?;
    Ok(Response::new(std::fs::read(p).map_err(Error::from)?))
}

/// Matches a download by the text of its first pages; returns the report title if attached.
#[tauri::command]
pub async fn match_download_text(
    state: State<'_, AppState>,
    review_id: String,
    path: String,
    text: String,
) -> CmdResult<Option<String>> {
    let p = crate::pipeline::downloads::checked_download(&path)?;
    let (c, db) = open(&state, &review_id)?;
    let title = crate::pipeline::downloads::match_by_text(&c, &data_dir_of(&db), &p, &text)?;
    if title.is_some() {
        wake(&state, &review_id)?;
    }
    Ok(title)
}

/// The user assigns a download to a waiting report.
#[tauri::command]
pub async fn assign_download(
    state: State<'_, AppState>,
    review_id: String,
    path: String,
    report_id: String,
) -> CmdResult<String> {
    let p = crate::pipeline::downloads::checked_download(&path)?;
    let (c, db) = open(&state, &review_id)?;
    let title = crate::pipeline::downloads::assign(&c, &data_dir_of(&db), &p, &report_id)?;
    wake(&state, &review_id)?;
    Ok(title)
}

/// Attaches PDFs from the user's Zotero library to waiting reports.
#[tauri::command]
pub async fn import_from_zotero(
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<crate::pipeline::zotero::ZoteroImport> {
    let dir = crate::pipeline::zotero::default_dir()
        .ok_or_else(|| Error::Invalid("No home folder.".into()))?;
    let (c, db) = open(&state, &review_id)?;
    let out = crate::pipeline::zotero::import(&c, &data_dir_of(&db), &dir)?;
    wake(&state, &review_id)?;
    Ok(out)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    /// Changes whenever anything in the review changes (new event or job update).
    pub token: String,
    /// (job kind, running, queued), busiest first.
    pub jobs: Vec<(String, i64, i64)>,
    /// What the workers are doing this moment, per item (in memory; empty after a restart).
    pub now: Vec<crate::pipeline::live::ActiveItem>,
}

/// A cheap change signal for live screens, plus what is running right now.
#[tauri::command]
pub async fn review_activity(state: State<'_, AppState>, review_id: String) -> CmdResult<Activity> {
    let c = conn(&state, &review_id)?;
    let token: String = c.query_row(
        "SELECT COALESCE((SELECT MAX(id) FROM event), 0) || ':' || COALESCE((SELECT MAX(updated_at) FROM job), '')
            || ':' || (SELECT COUNT(*) FROM job WHERE status IN ('queued','running'))
            || ':' || (SELECT COUNT(*) FROM report WHERE page_texts IS NOT NULL)",
        [],
        |r| r.get(0),
    )?;
    let mut stmt = c.prepare(
        "SELECT kind, SUM(status = 'running'), SUM(status = 'queued') FROM job
         WHERE status IN ('queued','running') GROUP BY kind ORDER BY 2 DESC, 3 DESC",
    )?;
    let jobs = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let now = state.env.live.active(&review_id);
    Ok(Activity { token, jobs, now })
}

/// Opens a report's PDF in the system viewer (Preview on macOS).
#[tauri::command]
pub async fn open_report_pdf(
    state: State<'_, AppState>,
    review_id: String,
    report_id: String,
) -> CmdResult<()> {
    let path = retrieval::pdf_path(&conn(&state, &review_id)?, &report_id)?;
    if !path.is_file() {
        return Err(Error::Invalid("The PDF file is missing.".into()).into());
    }
    super::settings::open_url_unchecked(&path.to_string_lossy()).map_err(|e| Error::Io(e).into())
}

/// What full-text retrieval is doing right now, and its latest outcomes.
#[tauri::command]
pub async fn retrieval_live(
    state: State<'_, AppState>,
    review_id: String,
) -> CmdResult<crate::pipeline::live::Snapshot> {
    Ok(state.env.live.snapshot(&review_id))
}

#[cfg(test)]
mod tests {
    use super::percent_decode;

    #[test]
    fn decodes_encoded_file_names() {
        assert_eq!(
            percent_decode("M%C3%BCller%202019%20(1).pdf"),
            "Müller 2019 (1).pdf"
        );
        assert_eq!(percent_decode("100%.pdf"), "100%.pdf");
        assert_eq!(percent_decode("a%zzb%4"), "a%zzb%4");
    }
}
