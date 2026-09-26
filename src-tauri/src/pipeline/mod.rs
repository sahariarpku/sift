//! The orchestrator (§8): a state machine over stages.
//!
//! protocol -> [Gate 1] -> seed set -> search -> seed check (broaden up to 3x)
//! -> dedup -> ta_screen -> [Gate 2] -> human queue -> retrieval -> text
//! -> ft_screen -> human queue -> citation chasing (feeds back to ta_screen)
//! -> link -> extract -> appraise -> synth -> report -> [Gate 3]
//!
//! `advance` looks at the review file, finds the first unfinished step, and
//! either enqueues its jobs or stops at a gate. It never decides anything a
//! person should decide. Jobs are idempotent per (kind, payload), so calling
//! `advance` often is safe.

pub mod batch_upload;
pub mod bypass;
pub mod chasing;
pub mod downloads;
#[cfg(test)]
mod e2e_test;
pub mod env;
pub mod fulltext_access;
pub mod gate2;
pub mod gate3;
pub mod live;
pub mod retrieval;
pub mod skip;
pub mod zotero;

use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::json;

use crate::agents::{appraisal, extraction, protocol, screening, search, synthesis};
use crate::error::Result;
use crate::jobs::{self, JobContext, Orchestrator, Registry, RunPause};
use crate::local::laya;

pub fn register(r: Registry) -> Registry {
    r.register(protocol::DRAFT_JOB, Arc::new(protocol::DraftProtocol))
        .register(search::SEARCH_JOB, Arc::new(search::SearchSource))
        .register(search::SEED_SEARCH_JOB, Arc::new(search::SeedSearch))
        .register(search::SEED_CHECK_JOB, Arc::new(search::SeedCheck))
        .register(search::BROADEN_JOB, Arc::new(search::Broaden))
        .register(search::DEDUP_JOB, Arc::new(search::Dedup))
        .register(search::CITATION_JOB, Arc::new(search::CitationChasing))
        .register(
            crate::agents::prescreen::JOB,
            Arc::new(crate::agents::prescreen::Prescreen),
        )
        .register(screening::TA_JOB, Arc::new(screening::ScreenTa))
        .register(screening::FT_JOB, Arc::new(screening::ScreenFt))
        .register(screening::SEED_JOB, Arc::new(screening::ScreenSeed))
        .register(screening::DRIFT_JOB, Arc::new(screening::DriftCheck))
        .register(
            crate::report::manuscript::DRAFT_JOB,
            Arc::new(crate::report::manuscript::DraftManuscript),
        )
        .register(retrieval::RETRIEVE_JOB, Arc::new(retrieval::Retrieve))
        .register(retrieval::LINK_JOB, Arc::new(retrieval::LinkStudies))
        .register(extraction::EXTRACT_JOB, Arc::new(extraction::Extract))
        .register(appraisal::APPRAISE_JOB, Arc::new(appraisal::Appraise))
        .register(synthesis::SYNTH_JOB, Arc::new(synthesis::Synthesise))
        .with_orchestrator(Arc::new(Pipeline))
}

pub struct Pipeline;

impl Orchestrator for Pipeline {
    fn advance(&self, ctx: &JobContext) -> Result<()> {
        let conn = ctx.open()?;
        advance(&conn, &ctx.review_id).map(|_| ())
    }
}

fn max_event(conn: &Connection, stage: &str, decisions: &[&str]) -> Result<i64> {
    let list = decisions
        .iter()
        .map(|d| format!("'{d}'"))
        .collect::<Vec<_>>()
        .join(",");
    Ok(conn.query_row(
        &format!(
            "SELECT COALESCE(MAX(id), 0) FROM event WHERE stage = ?1 AND decision IN ({list})"
        ),
        [stage],
        |r| r.get(0),
    )?)
}

fn has_event(conn: &Connection, stage: &str, decision: &str) -> Result<bool> {
    Ok(max_event(conn, stage, &[decision])? > 0)
}

fn ids(conn: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Where the pipeline stopped.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum Halt {
    /// Jobs are queued or running.
    Working(String),
    /// The user has to act.
    Waiting(String),
    Paused(String),
    Done,
}

/// Enqueues the next work. Returns where the pipeline stands. In bypass
/// mode, points that would wait for a person are settled by rule (see
/// `bypass`) and the pipeline carries on.
pub fn advance(conn: &Connection, review_id: &str) -> Result<Halt> {
    let mut halt = step(conn, review_id)?;
    if !bypass::on(conn)? {
        return Ok(halt);
    }
    // Each resolution clears one waiting point; a review has fewer than this.
    for _ in 0..12 {
        let Halt::Waiting(detail) = &halt else {
            break;
        };
        if !bypass::resolve(conn, review_id, detail)? {
            break;
        }
        halt = step(conn, review_id)?;
    }
    Ok(halt)
}

/// One pass of the state machine: enqueues the next work or stops.
fn step(conn: &Connection, review_id: &str) -> Result<Halt> {
    if let Some(p) = jobs::run_paused(conn)? {
        return Ok(Halt::Paused(p.reason));
    }
    let failed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM job WHERE status = 'failed'",
        [],
        |r| r.get(0),
    )?;
    if failed > 0 {
        return Ok(Halt::Waiting(format!(
            "{failed} jobs failed. Retry them from the Overview."
        )));
    }
    let enqueue = |kind: &str, payload: serde_json::Value| {
        jobs::enqueue_once(conn, review_id, kind, &payload)
    };
    let busy = |kinds: &[&str]| -> Result<bool> { Ok(jobs::outstanding(conn, kinds)? > 0) };

    // Protocol and Gate 1.
    if protocol::locked(conn)?.is_none() {
        if protocol::draft(conn)?.is_none() {
            enqueue(protocol::DRAFT_JOB, json!({}))?;
            return Ok(if busy(&[protocol::DRAFT_JOB])? {
                Halt::Working("Drafting the protocol".into())
            } else {
                Halt::Waiting("The protocol draft failed. See the job list.".into())
            });
        }
        return Ok(Halt::Waiting("Gate 1: review and lock the protocol".into()));
    }
    let opts = protocol::options(conn)?;
    let plan = search::plan(conn)?;
    // Stages the author skipped: what they finished is kept and the pipeline
    // carries on without them (see `skip`).
    let skips = skip::current(conn)?;
    let skipped = |id: &str| skips.contains(id);

    // Seed set (§8.2): narrow search, confirmed by both screeners.
    if opts.seed_check && !skipped("search") {
        if !has_event(conn, "search", "seed_candidates")? {
            enqueue(search::SEED_SEARCH_JOB, json!({}))?;
            if busy(&[search::SEED_SEARCH_JOB])? {
                return Ok(Halt::Working("Building the seed set".into()));
            }
        }
        if busy(&[search::SEED_SEARCH_JOB, screening::SEED_JOB])? {
            return Ok(Halt::Working(
                "Screeners are confirming the seed set".into(),
            ));
        }
    }

    // Search every chosen source for the current iteration.
    let sources: Vec<&String> = opts
        .sources
        .iter()
        .filter(|s| search::SEARCHABLE.contains(&s.as_str()))
        .collect();
    for s in sources.iter().filter(|_| !skipped("search")) {
        let done: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM search_run WHERE source = ?1 AND iteration = ?2)",
            rusqlite::params![s, plan.iteration],
            |r| r.get(0),
        )?;
        if !done {
            enqueue(
                search::SEARCH_JOB,
                json!({"source": s, "iteration": plan.iteration}),
            )?;
        }
    }
    if !skipped("search") && busy(&[search::SEARCH_JOB, search::BROADEN_JOB])? {
        return Ok(Halt::Working(format!(
            "Searching {} sources (iteration {})",
            sources.len(),
            plan.iteration
        )));
    }

    // Seed check and broadening, at most 3 iterations.
    if opts.seed_check && !skipped("search") && !search::seed_set(conn)?.is_empty() {
        let checked: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM event WHERE stage = 'search' AND decision LIKE 'seed_check_%' AND reason_code = ?1)",
            [plan.iteration.to_string()],
            |r| r.get(0),
        )?;
        if !checked {
            enqueue(search::SEED_CHECK_JOB, json!({"iteration": plan.iteration}))?;
            return Ok(Halt::Working("Checking the seed studies were found".into()));
        }
        if busy(&[search::SEED_CHECK_JOB, search::BROADEN_JOB])? {
            return Ok(Halt::Working("Broadening the search".into()));
        }
    }

    // Deduplicate after every batch of new records.
    let last_search = max_event(conn, "search", &["run", "citation_chasing", "import"])?;
    if max_event(conn, "dedup", &["complete"])? < last_search {
        enqueue(search::DEDUP_JOB, json!({"after": last_search}))?;
        return Ok(Halt::Working("Removing duplicates".into()));
    }

    // Optional local pre-screen (Laya). It runs side by side with the AI
    // screeners: a record goes to title/abstract screening as soon as it has
    // a score, so the local model never holds the screeners up.
    let manual = gate2::manual_ta(conn)?;
    let mut scoring = None;
    if !manual && !skipped("screening") && opts.prescreen.mode != laya::Mode::Off {
        let todo = laya::unscored(conn)?.len();
        // One scoring job at a time; the count in the payload lets a later
        // batch (citation chasing) enqueue a fresh job.
        if todo > 0 && !busy(&[crate::agents::prescreen::JOB])? {
            enqueue(
                crate::agents::prescreen::JOB,
                json!({"scored": laya::scored_count(conn)?}),
            )?;
        }
        if busy(&[crate::agents::prescreen::JOB])? {
            scoring = Some(todo);
        }
    }

    // Title/abstract screening, highest pre-screen score first when there is
    // one. While Laya is still scoring, only records it has scored are queued.
    let unscreened = ids(
        conn,
        &format!(
            "SELECT s.id FROM v_screenable s
             LEFT JOIN (SELECT target_id, MAX(confidence) AS score FROM event
                        WHERE stage = 'ta_prescreen' AND decision = 'score' GROUP BY target_id) p
               ON p.target_id = s.id
             WHERE s.id NOT IN (SELECT target_id FROM v_final_decision WHERE stage = 'ta_screen')
               AND NOT EXISTS (SELECT 1 FROM event e WHERE e.stage = 'ta_screen' AND e.target_id = s.id AND e.actor_id = 'arbiter')
               {}
             ORDER BY p.score DESC NULLS LAST, s.rec_order",
            if scoring.is_some() { "AND p.score IS NOT NULL" } else { "" }
        ),
    )?;
    if !manual && !skipped("screening") {
        for id in &unscreened {
            enqueue(screening::TA_JOB, json!({"record_id": id}))?;
        }
    }
    if !skipped("screening") && (busy(&[screening::TA_JOB])? || scoring.is_some()) {
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM v_screenable", [], |r| r.get(0))?;
        let done: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT target_id) FROM v_final_decision WHERE stage = 'ta_screen'",
            [],
            |r| r.get(0),
        )?;
        let screening = format!("Screening titles and abstracts: {done} of {total} decided");
        return Ok(Halt::Working(match scoring {
            Some(todo) => format!("{screening}; local pre-screen (Laya): {todo} left to score"),
            None => screening,
        }));
    }

    // Drift check once screening is done (§9); it does not hold up Gate 2.
    if !manual && !skipped("screening") {
        let screened: i64 = conn.query_row(
            "SELECT COUNT(*) FROM v_final_decision WHERE stage = 'ta_screen'",
            [],
            |r| r.get(0),
        )?;
        if screened >= 5 {
            enqueue(screening::DRIFT_JOB, json!({}))?;
        }
    }

    // Gate 2.
    match gate2::outcome(conn)?.as_deref() {
        None => {
            if gate2::draw_sample(conn)? > 0 {
                return Ok(Halt::Waiting(
                    "Gate 2: screen the calibration sample".into(),
                ));
            }
        }
        Some("fail") if !manual => {
            return Ok(Halt::Waiting(
                "Gate 2 failed: skip the gate or screen the remaining records yourself".into(),
            ));
        }
        _ => {}
    }
    if manual && !skipped("screening") && !gate2::manual_queue(conn)?.is_empty() {
        return Ok(Halt::Waiting(format!(
            "Screen the remaining {} records yourself",
            gate2::manual_queue(conn)?.len()
        )));
    }
    if !manual && protocol::options(conn)?.uncertain_to_full_text {
        screening::carry_uncertain_forward(conn, review_id)?;
    }
    // Once screening is stopped, records still in the queue count as not
    // screened; they do not hold the review up.
    let ta_queue = screening::human_queue(conn, "ta_screen")?;
    if !ta_queue.is_empty() && !skipped("screening") {
        return Ok(Halt::Waiting(format!(
            "{} records need your screening decision",
            ta_queue.len()
        )));
    }

    // Retrieval.
    let to_retrieve = ids(
        conn,
        "SELECT f.target_id FROM v_final_decision f
         WHERE f.stage = 'ta_screen' AND f.decision = 'include'
           AND f.target_id IN (SELECT id FROM v_screenable)
           AND f.target_id NOT IN (SELECT rep.record_id FROM report rep JOIN v_latest_retrieval l ON l.report_id = rep.id)",
    )?;
    let ft_skipped = skipped("fulltext");
    for id in to_retrieve.iter().filter(|_| !ft_skipped) {
        enqueue(retrieval::RETRIEVE_JOB, json!({"record_id": id}))?;
    }
    // Full-text screening starts on every text already read, while the rest
    // are still being retrieved, uploaded or read.
    let ft = ids(
        conn,
        "SELECT rep.id FROM report rep JOIN v_latest_retrieval l ON l.report_id = rep.id
         WHERE l.decision IN ('open_access','proxy','uploaded') AND rep.page_texts IS NOT NULL AND rep.page_texts != '[]'
           AND rep.id NOT IN (SELECT target_id FROM v_final_decision WHERE stage = 'ft_screen')
           AND NOT EXISTS (SELECT 1 FROM event e WHERE e.stage = 'ft_screen' AND e.target_id = rep.id AND e.actor_id = 'arbiter')",
    )?;
    for id in ft.iter().filter(|_| !ft_skipped) {
        enqueue(screening::FT_JOB, json!({"report_id": id}))?;
    }
    // Once full text is stopped, whatever is still being retrieved, uploaded,
    // read or screened is reported as not retrieved or awaiting classification.
    if !ft_skipped && busy(&[retrieval::RETRIEVE_JOB])? {
        return Ok(Halt::Working("Retrieving full texts".into()));
    }
    retrieval::expire_waiting(conn, opts.retrieval_deadline_hours)?;
    let uploads = if ft_skipped {
        Vec::new()
    } else {
        retrieval::upload_queue(conn)?
    };
    if !uploads.is_empty() && busy(&[screening::FT_JOB])? {
        return Ok(Halt::Working(format!(
            "Screening full texts; {} still to upload or mark not retrieved",
            uploads.len()
        )));
    }
    if !uploads.is_empty() {
        return Ok(Halt::Waiting(format!(
            "{} full texts to upload or mark not retrieved",
            uploads.len()
        )));
    }
    let pending_text = retrieval::pending_text(conn)?;
    if !pending_text.is_empty() && !ft_skipped {
        return Ok(Halt::Working(format!(
            "Reading {} PDFs (keep the app open)",
            pending_text.len()
        )));
    }

    if !ft_skipped && busy(&[screening::FT_JOB])? {
        return Ok(Halt::Working("Screening full texts".into()));
    }
    let ft_queue = screening::human_queue(conn, "ft_screen")?;
    if !ft_queue.is_empty() && !ft_skipped {
        return Ok(Halt::Waiting(format!(
            "{} full texts need your screening decision",
            ft_queue.len()
        )));
    }
    let included: i64 =
        conn.query_row("SELECT COUNT(*) FROM v_included_report", [], |r| r.get(0))?;

    // Citation chasing, once (§8.6). New records flow back through the steps
    // above, so it cannot run once screening or full text is stopped.
    let chase = chasing::skipped(conn)?.is_none() && !skipped("screening") && !ft_skipped;
    if opts.citation_chasing
        && chase
        && included > 0
        && !has_event(conn, "search", "citation_chasing")?
        && chasing::skipped(conn)?.is_none()
    {
        enqueue(search::CITATION_JOB, json!({}))?;
        if busy(&[search::CITATION_JOB])? {
            return Ok(Halt::Working(
                "Chasing citations of included studies".into(),
            ));
        }
    }
    if chase && busy(&[search::CITATION_JOB])? {
        return Ok(Halt::Working(
            "Chasing citations of included studies".into(),
        ));
    }
    if included == 0 {
        return Ok(Halt::Waiting(
            "No studies were included. Check the screening decisions or the protocol.".into(),
        ));
    }

    // Link reports to studies.
    let unlinked: i64 = conn.query_row(
        "SELECT COUNT(*) FROM v_included_report WHERE study_id IS NULL",
        [],
        |r| r.get(0),
    )?;
    if unlinked > 0 {
        let after = max_event(conn, "ft_screen", &["include", "exclude"])?;
        enqueue(retrieval::LINK_JOB, json!({"after": after}))?;
        return Ok(Halt::Working("Linking reports to studies".into()));
    }

    // Extraction and appraisal, per study. A study is appraised as soon as
    // its extraction is settled (appraisal reads the extracted design), so
    // the two run side by side across studies.
    let studies = ids(
        conn,
        "SELECT DISTINCT study_id FROM v_included_report WHERE study_id IS NOT NULL",
    )?;
    let (mut extracted_n, mut appraised_n) = (0, 0);
    for s in &studies {
        let extracted = extraction::settled(conn, s)?;
        extracted_n += usize::from(extracted);
        if !extracted && !skipped("extraction") {
            // The prompt version is part of the payload so a study that failed
            // under an older prompt gets a fresh job rather than a duplicate.
            enqueue(
                extraction::EXTRACT_JOB,
                json!({"study_id": s, "prompt": extraction::PROMPT}),
            )?;
        }
        let ready = extracted || skipped("extraction");
        let appraised = appraisal::settled(conn, s)?;
        appraised_n += usize::from(appraised);
        if ready && !appraised && !skipped("appraisal") {
            enqueue(
                appraisal::APPRAISE_JOB,
                json!({"study_id": s, "prompt": appraisal::PROMPT}),
            )?;
        }
    }
    let extract_busy = !skipped("extraction") && busy(&[extraction::EXTRACT_JOB])?;
    let appraise_busy = !skipped("appraisal") && busy(&[appraisal::APPRAISE_JOB])?;
    if extract_busy || appraise_busy {
        let total = studies.len();
        let mut parts = Vec::new();
        if extract_busy {
            parts.push(format!("Extracting data: {extracted_n} of {total} studies done"));
        }
        if appraise_busy {
            parts.push(format!("appraising: {appraised_n} of {total} studies done"));
        }
        let msg = parts.join("; ");
        return Ok(Halt::Working(match msg.strip_prefix("appraising") {
            Some(rest) => format!("Appraising{rest}"),
            None => msg,
        }));
    }

    // Synthesis, rerun whenever extraction changed.
    let last_extract = max_event(conn, "extract", &["extracted", "needs_human"])?;
    let synthesised: Option<i64> = conn
        .query_row(
            "SELECT MAX(id) FROM event WHERE stage = 'synth' AND decision = 'complete'",
            [],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    if !skipped("synthesis") && synthesised.unwrap_or(0) < last_extract {
        enqueue(synthesis::SYNTH_JOB, json!({"after": last_extract}))?;
        if busy(&[synthesis::SYNTH_JOB])? {
            return Ok(Halt::Working("Synthesising findings".into()));
        }
        return Ok(Halt::Waiting("Synthesis failed. See the job list.".into()));
    }

    // Draft the manuscript sections the author will edit. A synthesis skip
    // counts as a change, so the draft describes what was actually done.
    if !skipped("report") {
        let synth_skip = skip::list(conn)?
            .iter()
            .find(|k| k.stage == "synthesis")
            .map_or(0, |k| k.event_id);
        let after = max_event(conn, "synth", &["complete"])?.max(synth_skip);
        enqueue(crate::report::manuscript::DRAFT_JOB, json!({"after": after}))?;
        if busy(&[crate::report::manuscript::DRAFT_JOB])? {
            return Ok(Halt::Working("Drafting the manuscript".into()));
        }
    }

    // Report and Gate 3.
    match gate3::outcome(conn)?.as_deref() {
        Some("pass") | Some("skip") => Ok(Halt::Done),
        _ => Ok(Halt::Waiting(
            "Gate 3: resolve flags, write items 3, 25 and 26, then sign off".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Progress for the UI

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StageState {
    pub id: &'static str,
    /// done | running | waiting | pending | skipped
    pub status: &'static str,
    /// The author may skip it now (see `skip`).
    pub skippable: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub halt: Halt,
    pub stages: Vec<StageState>,
    pub pause: Option<RunPause>,
    pub queued: i64,
    pub running: i64,
    pub failed: i64,
    pub verification: Verification,
    pub chasing: chasing::ChasingState,
    pub skips: Vec<skip::Skip>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Verification {
    pub verified: bool,
    pub gates_passed: u8,
    pub skipped: Vec<u8>,
    /// Stages the author stopped early, by name.
    pub stopped: Vec<String>,
    pub label: String,
}

pub fn gate_outcome(conn: &Connection, stage: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT decision FROM v_current_event WHERE stage = ?1 AND decision IN ('pass','fail','skip') ORDER BY id DESC LIMIT 1",
            [stage],
            |r| r.get(0),
        )
        .optional()?)
}

/// Verification is derived from the gate events (§9): any skip leaves the
/// review unverified for good.
pub fn verification(conn: &Connection) -> Result<Verification> {
    let mut passed = 0;
    let mut skipped = Vec::new();
    for (n, stage) in [(1u8, "gate1"), (2, "gate2"), (3, "gate3")] {
        let any_skip: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM event WHERE stage = ?1 AND decision = 'skip')",
            [stage],
            |r| r.get(0),
        )?;
        if any_skip {
            skipped.push(n);
        } else if gate_outcome(conn, stage)?.as_deref() == Some("pass") {
            passed += 1;
        }
    }
    let verified = passed == 3 && skipped.is_empty();
    let stopped: Vec<String> = skip::list(conn)?
        .into_iter()
        .map(|k| k.name.to_owned())
        .collect();
    let label = if verified {
        "Verified · 3 of 3 gates passed".to_owned()
    } else if !skipped.is_empty() {
        format!(
            "Unverified · gate {} skipped",
            skipped
                .iter()
                .map(|g| g.to_string())
                .collect::<Vec<_>>()
                .join(" and ")
        )
    } else {
        format!("Unverified · {passed} of 3 gates passed")
    };
    let label = if stopped.is_empty() {
        label
    } else {
        format!("{label} · stopped early: {}", stopped.join(", "))
    };
    Ok(Verification {
        verified,
        gates_passed: passed,
        skipped,
        stopped,
        label,
    })
}

pub fn progress(conn: &Connection, review_id: &str) -> Result<Progress> {
    let halt = advance_readonly(conn, review_id)?;
    let count = |status: &str| -> Result<i64> {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM job WHERE status = ?1",
            [status],
            |r| r.get(0),
        )?)
    };
    let busy = |kinds: &[&str]| -> Result<bool> { Ok(jobs::outstanding(conn, kinds)? > 0) };
    let locked = protocol::locked(conn)?.is_some();
    let ta_final: i64 = conn.query_row(
        "SELECT COUNT(*) FROM v_final_decision WHERE stage = 'ta_screen'",
        [],
        |r| r.get(0),
    )?;
    let dedup_done = has_event(conn, "dedup", "complete")?;
    let g2 = gate2::outcome(conn)?;
    let reports: i64 = conn.query_row("SELECT COUNT(*) FROM report", [], |r| r.get(0))?;
    let ft_final: i64 = conn.query_row(
        "SELECT COUNT(*) FROM v_final_decision WHERE stage = 'ft_screen'",
        [],
        |r| r.get(0),
    )?;
    let extracted = has_event(conn, "extract", "extracted")?;
    let appraised: i64 = conn.query_row("SELECT COUNT(*) FROM appraisal", [], |r| r.get(0))?;
    let synthesised = has_event(conn, "synth", "complete")?;
    let g3 = gate3::outcome(conn)?;
    let waiting = matches!(halt, Halt::Waiting(_));

    let status = |done: bool, running: bool, started: bool| -> &'static str {
        if done {
            "done"
        } else if running {
            "running"
        } else if started && waiting {
            "waiting"
        } else {
            "pending"
        }
    };
    let stages = vec![
        StageState {
            id: "protocol",
            status: status(
                locked,
                busy(&[protocol::DRAFT_JOB])?,
                protocol::draft(conn)?.is_some(),
            ),
            skippable: false,
        },
        StageState {
            id: "search",
            status: status(
                dedup_done
                    && !busy(&[
                        search::SEARCH_JOB,
                        search::SEED_SEARCH_JOB,
                        search::DEDUP_JOB,
                        search::BROADEN_JOB,
                        screening::SEED_JOB,
                    ])?,
                busy(&[
                    search::SEARCH_JOB,
                    search::SEED_SEARCH_JOB,
                    search::DEDUP_JOB,
                    search::BROADEN_JOB,
                    screening::SEED_JOB,
                    search::SEED_CHECK_JOB,
                ])?,
                locked,
            ),
            skippable: false,
        },
        StageState {
            id: "screening",
            status: status(
                matches!(g2.as_deref(), Some("pass") | Some("skip"))
                    && screening::human_queue(conn, "ta_screen")?.is_empty()
                    && !busy(&[screening::TA_JOB])?,
                busy(&[screening::TA_JOB])?,
                ta_final > 0,
            ),
            skippable: false,
        },
        StageState {
            id: "fulltext",
            status: status(
                reports > 0
                    && ft_final > 0
                    && !busy(&[
                        retrieval::RETRIEVE_JOB,
                        screening::FT_JOB,
                        search::CITATION_JOB,
                    ])?
                    && screening::human_queue(conn, "ft_screen")?.is_empty()
                    && retrieval::upload_queue(conn)?.is_empty(),
                busy(&[
                    retrieval::RETRIEVE_JOB,
                    screening::FT_JOB,
                    search::CITATION_JOB,
                ])?,
                reports > 0,
            ),
            skippable: false,
        },
        StageState {
            id: "extraction",
            status: status(
                extracted && !busy(&[extraction::EXTRACT_JOB])?,
                busy(&[extraction::EXTRACT_JOB, retrieval::LINK_JOB])?,
                extracted,
            ),
            skippable: false,
        },
        StageState {
            id: "appraisal",
            status: status(
                appraised > 0 && !busy(&[appraisal::APPRAISE_JOB])?,
                busy(&[appraisal::APPRAISE_JOB])?,
                appraised > 0,
            ),
            skippable: false,
        },
        StageState {
            id: "synthesis",
            status: status(
                synthesised && !busy(&[synthesis::SYNTH_JOB])?,
                busy(&[synthesis::SYNTH_JOB])?,
                synthesised,
            ),
            skippable: false,
        },
        StageState {
            id: "report",
            status: status(
                matches!(g3.as_deref(), Some("pass") | Some("skip")),
                false,
                synthesised,
            ),
            skippable: false,
        },
    ];
    let skips = skip::list(conn)?;
    let found: i64 = conn.query_row("SELECT COUNT(*) FROM v_identified", [], |r| r.get(0))?;
    let stages = stages
        .into_iter()
        .map(|mut st| {
            let is_skipped = skips.iter().any(|k| k.stage == st.id);
            if is_skipped && st.status != "done" {
                st.status = "skipped";
            }
            st.skippable = locked
                && st.id != "protocol"
                && !is_skipped
                && st.status != "done"
                && (st.id != "search" || found > 0);
            st
        })
        .collect();
    Ok(Progress {
        halt,
        stages,
        pause: jobs::run_paused(conn)?,
        queued: count("queued")?,
        running: count("running")?,
        failed: count("failed")?,
        verification: verification(conn)?,
        chasing: chasing::state(conn, protocol::options(conn)?.citation_chasing)?,
        skips,
    })
}

/// `advance` without enqueueing: runs it inside a transaction that is rolled back.
fn advance_readonly(conn: &Connection, review_id: &str) -> Result<Halt> {
    let tx = conn.unchecked_transaction()?;
    let h = advance(&tx, review_id);
    tx.rollback()?;
    h
}

#[cfg(test)]
mod prescreen_tests {
    use super::*;
    use crate::events::{self, ActorType, NewEvent};

    fn queued_records(conn: &Connection) -> Vec<String> {
        ids(
            conn,
            "SELECT json_extract(payload, '$.record_id') FROM job WHERE kind = 'screen_ta' ORDER BY 1",
        )
        .unwrap()
    }

    #[test]
    fn screening_starts_on_scored_records_while_laya_is_still_scoring() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let review = crate::reviews::insert_review(&conn, "Q?").unwrap();
        let body = serde_json::to_string(&protocol::tests::sample_protocol()).unwrap();
        conn.execute(
            "INSERT INTO protocol_version (id, review_id, version, body) VALUES ('p', ?1, 0, ?2)",
            rusqlite::params![review, body],
        )
        .unwrap();
        protocol::lock(&conn, true).unwrap();
        let mut o = protocol::options(&conn).unwrap();
        o.sources.clear();
        o.seed_check = false;
        o.citation_chasing = false;
        o.prescreen.mode = laya::Mode::Speed;
        protocol::save_options(&conn, &o).unwrap();
        conn.execute(
            "INSERT INTO search_run (id, review_id, source, query_string, query_ast, run_at, hits) VALUES ('s', ?1, 'openalex', 'q', '{}', 'now', 3)",
            [&review],
        )
        .unwrap();
        for r in ["r0", "r1", "r2"] {
            conn.execute(
                "INSERT INTO record (id, search_run_id, title) VALUES (?1, 's', 't')",
                [r],
            )
            .unwrap();
        }
        let score = |r: &str| {
            events::append(
                &conn,
                &NewEvent {
                    review_id: review.clone(),
                    target_type: "record".into(),
                    target_id: r.into(),
                    stage: laya::STAGE.into(),
                    actor_type: Some(ActorType::Agent),
                    actor_id: laya::ACTOR.into(),
                    decision: "score".into(),
                    confidence: Some(0.5),
                    ..Default::default()
                },
            )
            .unwrap();
        };

        // Laya is scoring and has scored r0: r0 goes to the screeners now.
        score("r0");
        let halt = advance(&conn, &review).unwrap();
        assert!(
            matches!(&halt, Halt::Working(m) if m.contains("Laya): 2 left")),
            "{halt:?}"
        );
        assert_eq!(queued_records(&conn), ["r0"]);
        assert_eq!(
            jobs::outstanding(&conn, &[crate::agents::prescreen::JOB]).unwrap(),
            1
        );

        // Even with no screening job left, the stage waits for Laya.
        conn.execute(
            "UPDATE job SET status = 'done' WHERE kind = 'screen_ta'",
            [],
        )
        .unwrap();
        assert!(matches!(advance(&conn, &review).unwrap(), Halt::Working(_)));

        // Scoring finishes: the rest are queued.
        score("r1");
        score("r2");
        conn.execute(
            "UPDATE job SET status = 'done' WHERE kind = 'prescreen_ta'",
            [],
        )
        .unwrap();
        advance(&conn, &review).unwrap();
        assert_eq!(queued_records(&conn), ["r0", "r1", "r2"]);
    }
}
