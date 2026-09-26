//! Dual screening with an arbiter (§8.4), at title/abstract and full-text stage.
//!
//! Screener A and B decide independently (B argues for inclusion first). Both
//! include -> include; both exclude -> exclude; anything else goes to the
//! arbiter, who sees both rationales. An arbiter "uncertain", or two invalid
//! responses from any agent, puts the item in the human queue.

use async_trait::async_trait;
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::protocol::{require_locked, ProtocolBody};
use super::{call, AgentError, CallMeta, Reply};
use crate::error::Result;
use crate::events::{self, ActorType, Event, NewEvent};
use crate::jobs::{Job, JobContext, JobError, JobHandler};
use crate::llm::store::Task;
use crate::local::laya;
use crate::records;
use crate::validate;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Include,
    Exclude,
    Uncertain,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Include => "include",
            Decision::Exclude => "exclude",
            Decision::Uncertain => "uncertain",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Judgement {
    Met,
    Failed,
    CannotJudge,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CriterionCheck {
    pub code: String,
    pub judgement: Judgement,
}

/// Screener A (title/abstract).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScreenA {
    /// Per-criterion reasoning aid, not recorded (may be omitted).
    #[serde(default)]
    pub criteria: Vec<CriterionCheck>,
    pub decision: Decision,
    pub reason_code: Option<String>,
    pub rationale: String,
    pub evidence_quote: String,
    pub confidence: f64,
}

/// Screener B (title/abstract): argues for inclusion first.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScreenB {
    pub argument_for_inclusion: String,
    /// Per-criterion reasoning aid, not recorded (may be omitted).
    #[serde(default)]
    pub criteria: Vec<CriterionCheck>,
    pub decision: Decision,
    pub reason_code: Option<String>,
    pub rationale: String,
    pub evidence_quote: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ArbiterOut {
    pub decision: Decision,
    pub reason_code: Option<String>,
    pub rationale: String,
    pub evidence_quote: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FtScreenA {
    /// Per-criterion reasoning aid, not recorded (may be omitted).
    #[serde(default)]
    pub criteria: Vec<CriterionCheck>,
    pub decision: Decision,
    pub reason_code: Option<String>,
    pub rationale: String,
    pub evidence_quote: String,
    pub evidence_page: Option<i64>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FtScreenB {
    pub argument_for_inclusion: String,
    /// Per-criterion reasoning aid, not recorded (may be omitted).
    #[serde(default)]
    pub criteria: Vec<CriterionCheck>,
    pub decision: Decision,
    pub reason_code: Option<String>,
    pub rationale: String,
    pub evidence_quote: String,
    pub evidence_page: Option<i64>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FtArbiterOut {
    pub decision: Decision,
    pub reason_code: Option<String>,
    pub rationale: String,
    pub evidence_quote: String,
    pub evidence_page: Option<i64>,
    pub confidence: f64,
}

/// The fields every screening output shares, for validation and events.
#[derive(Debug, Clone)]
pub struct Verdict {
    pub decision: Decision,
    pub reason_code: Option<String>,
    pub rationale: String,
    pub evidence_quote: String,
    pub evidence_page: Option<i64>,
    pub confidence: f64,
}

/// A criterion code, or none. Models often write "none" or "n/a" instead of null.
pub fn clean_code(code: Option<&str>) -> Option<String> {
    let c = code?.trim();
    let placeholder = matches!(
        c.to_ascii_lowercase().as_str(),
        "" | "none" | "null" | "n/a" | "na" | "-" | "not applicable"
    );
    (!placeholder).then(|| c.to_owned())
}

macro_rules! verdict_from {
    ($t:ty, $page:expr) => {
        impl From<&$t> for Verdict {
            fn from(o: &$t) -> Self {
                #[allow(clippy::redundant_closure_call)]
                Verdict {
                    decision: o.decision,
                    reason_code: clean_code(o.reason_code.as_deref()),
                    rationale: o.rationale.clone(),
                    evidence_quote: o.evidence_quote.clone(),
                    evidence_page: ($page)(o),
                    confidence: o.confidence,
                }
            }
        }
    };
}
verdict_from!(ScreenA, |_o: &ScreenA| None);
verdict_from!(ScreenB, |_o: &ScreenB| None);
verdict_from!(ArbiterOut, |_o: &ArbiterOut| None);
verdict_from!(FtScreenA, |o: &FtScreenA| o.evidence_page);
verdict_from!(FtScreenB, |o: &FtScreenB| o.evidence_page);
verdict_from!(FtArbiterOut, |o: &FtArbiterOut| o.evidence_page);

/// Confidence on a 0-1 scale. Models sometimes answer in percent; code converts.
pub fn unit_confidence(c: f64) -> Option<f64> {
    if (0.0..=1.0).contains(&c) {
        Some(c)
    } else if c > 1.0 && c <= 100.0 {
        Some(c / 100.0)
    } else {
        None
    }
}

/// Checks a verdict against the protocol and the source text.
pub fn check(
    v: &Verdict,
    protocol: &ProtocolBody,
    source: Source<'_>,
    full_text: bool,
) -> std::result::Result<(), String> {
    if unit_confidence(v.confidence).is_none() {
        return Err(format!(
            "confidence {} is not between 0 and 1",
            v.confidence
        ));
    }
    let codes = protocol.codes();
    match (
        &v.decision,
        v.reason_code
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty()),
    ) {
        (Decision::Exclude, None) => return Err("an exclusion needs a reason_code".into()),
        (_, Some(code)) if !codes.contains(&code) => {
            return Err(format!(
                "reason_code {code} is not a criterion code in the protocol ({})",
                codes.join(", ")
            ))
        }
        _ => {}
    }
    if full_text && v.decision == Decision::Exclude && v.reason_code.is_none() {
        return Err("full-text exclusions need a reason code".into());
    }
    let q = v.evidence_quote.trim();
    if q.is_empty() {
        return Err("evidence_quote is empty".into());
    }
    let found = match source {
        Source::Text(t) => validate::quote_in(q, t),
        Source::Pages(p) => validate::locate(q, p, v.evidence_page).is_some(),
    };
    if !found {
        return Err("evidence_quote is not verbatim in the source text".into());
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub enum Source<'a> {
    Text(&'a str),
    Pages(&'a [String]),
}

fn verdict_event(
    meta: &CallMeta,
    review_id: &str,
    target_type: &str,
    target_id: &str,
    stage: &str,
    actor: &str,
    v: &Verdict,
) -> NewEvent {
    let mut e = meta.event(
        review_id,
        target_type,
        target_id,
        stage,
        actor,
        v.decision.as_str(),
    );
    e.reason_code = v.reason_code.clone().filter(|c| !c.trim().is_empty());
    e.rationale = Some(v.rationale.clone());
    e.evidence_quote = Some(v.evidence_quote.clone());
    e.confidence = unit_confidence(v.confidence);
    e
}

fn needs_human_event(
    review_id: &str,
    target_type: &str,
    target_id: &str,
    stage: &str,
    actor: &str,
    why: &str,
) -> NewEvent {
    NewEvent {
        review_id: review_id.into(),
        target_type: target_type.into(),
        target_id: target_id.into(),
        stage: stage.into(),
        actor_type: Some(ActorType::Agent),
        actor_id: actor.into(),
        decision: "needs_human".into(),
        rationale: Some(why.chars().take(500).collect()),
        ..Default::default()
    }
}

/// Latest current event by an actor on a target at a stage.
pub fn latest_by(
    conn: &Connection,
    stage: &str,
    target_id: &str,
    actor: &str,
) -> Result<Option<Event>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM v_current_event WHERE stage = ?1 AND target_id = ?2 AND actor_id = ?3 ORDER BY id DESC LIMIT 1",
    )?;
    let id: Option<i64> = stmt
        .query_row(rusqlite::params![stage, target_id, actor], |r| r.get(0))
        .ok();
    match id {
        Some(id) => Ok(conn
            .query_row("SELECT * FROM event WHERE id = ?1", [id], |r| {
                Ok(Event {
                    id: r.get("id")?,
                    review_id: r.get("review_id")?,
                    target_type: r.get("target_type")?,
                    target_id: r.get("target_id")?,
                    stage: r.get("stage")?,
                    actor_type: r.get("actor_type")?,
                    actor_id: r.get("actor_id")?,
                    provider: r.get("provider")?,
                    model_id: r.get("model_id")?,
                    prompt_version: r.get("prompt_version")?,
                    decision: r.get("decision")?,
                    reason_code: r.get("reason_code")?,
                    rationale: r.get("rationale")?,
                    evidence_quote: r.get("evidence_quote")?,
                    confidence: r.get("confidence")?,
                    blinded: r.get("blinded")?,
                    supersedes: r.get("supersedes")?,
                    tokens_in: r.get("tokens_in")?,
                    tokens_out: r.get("tokens_out")?,
                    cost_usd: r.get("cost_usd")?,
                    created_at: r.get("created_at")?,
                })
            })
            .ok()),
        None => Ok(None),
    }
}

fn summary(e: &Event) -> String {
    format!(
        "{} (reason {}, confidence {}): {}",
        e.decision,
        e.reason_code.as_deref().unwrap_or("none"),
        e.confidence
            .map(|c| format!("{c:.2}"))
            .unwrap_or_else(|| "n/a".into()),
        e.rationale.as_deref().unwrap_or("")
    )
}

fn db_err(e: crate::error::Error) -> JobError {
    JobError::Fatal(e.to_string())
}

/// Which prompts and output types a stage uses.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    TitleAbstract,
    FullText,
    /// Seed-set confirmation: both screeners, no arbiter, events at stage `search`.
    Seed,
}

impl Phase {
    fn stage(self) -> &'static str {
        match self {
            Phase::TitleAbstract => "ta_screen",
            Phase::FullText => "ft_screen",
            Phase::Seed => "search",
        }
    }
}

async fn screener(
    ctx: &JobContext,
    phase: Phase,
    which: char,
    protocol: &ProtocolBody,
    question: &str,
    text: &str,
    pages: &[String],
) -> std::result::Result<Reply<Verdict>, AgentError> {
    let vars = [
        ("question", question.to_owned()),
        ("criteria", protocol.criteria_text()),
        ("record", text.to_owned()),
        ("report", text.to_owned()),
    ];
    let ft = phase == Phase::FullText;
    let source = if ft {
        Source::Pages(pages)
    } else {
        Source::Text(text)
    };
    macro_rules! run {
        ($t:ty, $task:expr, $prompt:expr, $max:expr) => {{
            let r = call::<$t, _>(ctx, $task, $prompt, &vars, $max, |o| {
                check(&Verdict::from(o), protocol, source, ft)
            })
            .await?;
            Reply {
                value: Verdict::from(&r.value),
                meta: r.meta,
            }
        }};
    }
    Ok(match (ft, which) {
        (false, 'a') => run!(ScreenA, Task::ScreenerA, "screening_a.v1", 1500),
        (false, _) => run!(ScreenB, Task::ScreenerB, "screening_b.v1", 1800),
        (true, 'a') => run!(FtScreenA, Task::ScreenerA, "ft_screening_a.v1", 2000),
        (true, _) => run!(FtScreenB, Task::ScreenerB, "ft_screening_b.v1", 2200),
    })
}

/// Runs one screener unless it already has a current decision on this target
/// (so a retried job does not screen twice). Returns its latest event.
#[allow(clippy::too_many_arguments)]
async fn screen_once(
    ctx: &JobContext,
    phase: Phase,
    which: char,
    target_type: &str,
    target_id: &str,
    protocol: &ProtocolBody,
    question: &str,
    text: &str,
    pages: &[String],
    retry: bool,
) -> std::result::Result<Event, JobError> {
    let actor = if which == 'a' {
        "screener_a"
    } else {
        "screener_b"
    };
    let mut supersedes = None;
    {
        let conn = ctx.open().map_err(db_err)?;
        if let Some(e) = latest_by(&conn, phase.stage(), target_id, actor).map_err(db_err)? {
            // A retry re-asks only where the agent produced no usable answer.
            if !(retry && e.decision == "needs_human") {
                return Ok(e);
            }
            supersedes = Some(e.id);
        }
    }
    let result = screener(ctx, phase, which, protocol, question, text, pages).await;
    let conn = ctx.open().map_err(db_err)?;
    let mut ev = match result {
        Ok(r) => verdict_event(
            &r.meta,
            &ctx.review_id,
            target_type,
            target_id,
            phase.stage(),
            actor,
            &r.value,
        ),
        Err(AgentError::NeedsHuman(why)) => needs_human_event(
            &ctx.review_id,
            target_type,
            target_id,
            phase.stage(),
            actor,
            &why,
        ),
        Err(other) => return Err(other.into()),
    };
    ev.supersedes = supersedes;
    events::append(&conn, &ev).map_err(db_err)?;
    latest_by(&conn, phase.stage(), target_id, actor)
        .map_err(db_err)?
        .ok_or_else(|| JobError::Fatal("event lost".into()))
}

/// Screens one target: both screeners, then resolution or arbiter.
#[allow(clippy::too_many_arguments)]
async fn screen_target(
    ctx: &JobContext,
    phase: Phase,
    target_type: &str,
    target_id: &str,
    protocol: &ProtocolBody,
    question: &str,
    text: &str,
    pages: &[String],
    retry: bool,
) -> std::result::Result<(), JobError> {
    let stage = phase.stage();
    let live = {
        let conn = ctx.open().map_err(db_err)?;
        let title = target_title(&conn, target_type, target_id).map_err(db_err)?;
        let (screen, reading) = match phase {
            Phase::TitleAbstract => ("screening", "the title and abstract"),
            Phase::FullText => ("fulltext", "the full text"),
            Phase::Seed => ("search", "a seed paper"),
        };
        let t = ctx.env.live.track_in(&ctx.review_id, screen, target_id, &title);
        t.step(format!("Screener A and B reading {reading}"));
        t
    };
    // Optional local pre-screen (title/abstract only): see `local::laya`.
    let route = if phase == Phase::TitleAbstract {
        let conn = ctx.open().map_err(db_err)?;
        let opts = super::protocol::options(&conn).map_err(db_err)?.prescreen;
        laya::route(&conn, &opts, target_id).map_err(|e| JobError::Fatal(e.to_string()))?
    } else {
        None
    };
    let once = |which| {
        screen_once(
            ctx,
            phase,
            which,
            target_type,
            target_id,
            protocol,
            question,
            text,
            pages,
            retry,
        )
    };
    let (a, b) = if let Some(r) = route.as_ref().filter(|r| r.single) {
        let a = once('a').await?;
        if a.decision == "exclude" {
            let conn = ctx.open().map_err(db_err)?;
            if already_resolved(&conn, stage, target_id, retry)? {
                return Ok(());
            }
            let pct = (r.percentile * 100.0).round();
            prescreen_note(
                &conn,
                ctx,
                target_id,
                "single_screen",
                format!("Ranked in the lowest {pct}% by the local pre-screen (score {:.2}); Screener A only.", r.score),
            )?;
            let e = NewEvent {
                review_id: ctx.review_id.clone(),
                target_type: target_type.into(),
                target_id: target_id.into(),
                stage: stage.into(),
                actor_type: Some(ActorType::Agent),
                actor_id: "resolver".into(),
                decision: "exclude".into(),
                reason_code: a.reason_code.clone(),
                rationale: Some(format!(
                    "Single screening (speed mode): the local pre-screen ranked this record in the lowest {pct}% and Screener A excluded it. Screener B was not run."
                )),
                ..Default::default()
            };
            events::append(&conn, &e).map_err(db_err)?;
            return Ok(());
        }
        (a, once('b').await?)
    } else {
        let (a, b) = tokio::join!(once('a'), once('b'));
        (a?, b?)
    };
    if phase == Phase::Seed {
        return Ok(());
    }
    let conn = ctx.open().map_err(db_err)?;
    // On a retry, the arbiter rules again if its answer was unusable or it
    // ruled before a screener's answer was replaced (its inputs changed).
    let arbiter_retry = if retry {
        latest_by(&conn, stage, target_id, "arbiter")
            .map_err(db_err)?
            .filter(|e| arbiter_is_stale(e, &a, &b))
            .map(|e| e.id)
    } else {
        None
    };
    if already_resolved(&conn, stage, target_id, arbiter_retry.is_some())? {
        return Ok(());
    }
    let agreed = a.decision == b.decision && (a.decision == "include" || a.decision == "exclude");
    let escalate = agreed && a.decision == "exclude" && route.as_ref().is_some_and(|r| r.net);
    if escalate {
        if let Some(r) = &route {
            prescreen_note(
                &conn,
                ctx,
                target_id,
                "escalate",
                format!(
                    "Both screeners excluded, but the local pre-screen ranked this record in the top {}% (score {:.2}); sent to the arbiter.",
                    ((1.0 - r.percentile) * 100.0).round().max(1.0),
                    r.score
                ),
            )?;
        }
    }
    if agreed && !escalate {
        let e = NewEvent {
            review_id: ctx.review_id.clone(),
            target_type: target_type.into(),
            target_id: target_id.into(),
            stage: stage.into(),
            actor_type: Some(ActorType::Agent),
            actor_id: "resolver".into(),
            decision: a.decision.clone(),
            reason_code: a.reason_code.clone().or(b.reason_code.clone()),
            rationale: Some(format!("Screener A and B both decided {}.", a.decision)),
            ..Default::default()
        };
        if let Some(id) = arbiter_retry {
            // Retire the arbiter's unusable answer; the screeners now agree.
            let mut note = e.clone();
            note.actor_id = "arbiter".into();
            note.decision = "withdrawn".into();
            note.reason_code = None;
            note.rationale = Some("Superseded on retry: the screeners now agree.".into());
            note.supersedes = Some(id);
            events::append(&conn, &note).map_err(db_err)?;
        }
        events::append(&conn, &e).map_err(db_err)?;
        return Ok(());
    }
    drop(conn);
    live.step(format!(
        "Arbiter weighing both rationales (A: {}, B: {})",
        a.decision, b.decision
    ));

    let ft = phase == Phase::FullText;
    let vars = [
        ("question", question.to_owned()),
        ("criteria", protocol.criteria_text()),
        ("record", text.to_owned()),
        ("report", text.to_owned()),
        ("a", summary(&a)),
        ("b", summary(&b)),
    ];
    let source = if ft {
        Source::Pages(pages)
    } else {
        Source::Text(text)
    };
    let result = if ft {
        call::<FtArbiterOut, _>(ctx, Task::Arbiter, "ft_arbiter.v1", &vars, 2000, |o| {
            check(&Verdict::from(o), protocol, source, true)
        })
        .await
        .map(|r| Reply {
            value: Verdict::from(&r.value),
            meta: r.meta,
        })
    } else {
        call::<ArbiterOut, _>(ctx, Task::Arbiter, "arbiter.v1", &vars, 1500, |o| {
            check(&Verdict::from(o), protocol, source, false)
        })
        .await
        .map(|r| Reply {
            value: Verdict::from(&r.value),
            meta: r.meta,
        })
    };
    let conn = ctx.open().map_err(db_err)?;
    let ev = match result {
        Ok(r) => verdict_event(
            &r.meta,
            &ctx.review_id,
            target_type,
            target_id,
            stage,
            "arbiter",
            &r.value,
        ),
        Err(AgentError::NeedsHuman(why)) => needs_human_event(
            &ctx.review_id,
            target_type,
            target_id,
            stage,
            "arbiter",
            &why,
        ),
        Err(other) => return Err(other.into()),
    };
    let mut ev = ev;
    ev.supersedes = arbiter_retry;
    events::append(&conn, &ev).map_err(db_err)?;
    Ok(())
}

/// Whether a final decision already exists (a retried job must not decide twice).
/// Title of the record behind a screening target, for the live activity panel.
fn target_title(conn: &Connection, target_type: &str, target_id: &str) -> Result<String> {
    let sql = if target_type == "report" {
        "SELECT COALESCE(r.title, '') FROM report p JOIN record r ON r.id = p.record_id WHERE p.id = ?1"
    } else {
        "SELECT COALESCE(title, '') FROM record WHERE id = ?1"
    };
    let title: Option<String> = conn
        .query_row(sql, [target_id], |r| r.get(0))
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?;
    Ok(title.filter(|t| !t.is_empty()).unwrap_or_else(|| "Untitled record".into()))
}

fn already_resolved(
    conn: &Connection,
    stage: &str,
    target_id: &str,
    retry: bool,
) -> std::result::Result<bool, JobError> {
    for actor in ["resolver", "arbiter", "human"] {
        if latest_by(conn, stage, target_id, actor)
            .map_err(db_err)?
            .is_some()
        {
            // `retry` is set only when the arbiter's decision is stale.
            if retry && actor == "arbiter" {
                continue;
            }
            return Ok(true);
        }
    }
    Ok(false)
}

/// An arbiter decision is stale when it was unusable or older than a screener answer.
fn arbiter_is_stale(arbiter: &Event, a: &Event, b: &Event) -> bool {
    arbiter.decision == "needs_human" || arbiter.id < a.id.max(b.id)
}

/// Logs how the local pre-screen changed a record's screening path.
fn prescreen_note(
    conn: &Connection,
    ctx: &JobContext,
    target_id: &str,
    decision: &str,
    rationale: String,
) -> std::result::Result<(), JobError> {
    events::append(
        conn,
        &NewEvent {
            review_id: ctx.review_id.clone(),
            target_type: "record".into(),
            target_id: target_id.into(),
            stage: laya::STAGE.into(),
            actor_type: Some(ActorType::Agent),
            actor_id: laya::ACTOR.into(),
            decision: decision.into(),
            rationale: Some(rationale),
            ..Default::default()
        },
    )
    .map_err(db_err)?;
    Ok(())
}

pub const TA_JOB: &str = "screen_ta";
pub const FT_JOB: &str = "screen_ft";
pub const SEED_JOB: &str = "screen_seed";

pub struct ScreenTa;
pub struct ScreenFt;
pub struct ScreenSeed;

fn record_id(job: &Job) -> std::result::Result<String, JobError> {
    job.payload["record_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| JobError::Fatal("payload needs record_id".into()))
}

async fn screen_record(
    ctx: &JobContext,
    job: &Job,
    phase: Phase,
) -> std::result::Result<(), JobError> {
    let rid = record_id(job)?;
    let (protocol, question, text) = {
        let conn = ctx.open().map_err(db_err)?;
        if phase == Phase::TitleAbstract && !records::screenable(&conn, &rid).map_err(db_err)? {
            // A duplicate, or from a run the author skipped since this was queued.
            return Ok(());
        }
        let p = require_locked(&conn).map_err(db_err)?.body;
        let (_, q) = super::protocol::review_question(&conn).map_err(db_err)?;
        let rec = records::record(&conn, &rid)
            .map_err(db_err)?
            .ok_or_else(|| JobError::Fatal(format!("record {rid} not found")))?;
        (p, q, rec.screening_text())
    };
    let retry = job.payload["retry"].as_bool().unwrap_or(false);
    screen_target(
        ctx,
        phase,
        "record",
        &rid,
        &protocol,
        &question,
        &text,
        &[],
        retry,
    )
    .await
}

#[async_trait]
impl JobHandler for ScreenTa {
    async fn run(&self, job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        screen_record(ctx, job, Phase::TitleAbstract).await
    }
}

#[async_trait]
impl JobHandler for ScreenSeed {
    async fn run(&self, job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        screen_record(ctx, job, Phase::Seed).await
    }
}

/// Full texts longer than this are cut on a page boundary (stated in the prompt text).
pub const FT_MAX_CHARS: usize = 90_000;

#[async_trait]
impl JobHandler for ScreenFt {
    async fn run(&self, job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        let report_id = job.payload["report_id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| JobError::Fatal("payload needs report_id".into()))?;
        let (protocol, question, pages) = {
            let conn = ctx.open().map_err(db_err)?;
            let p = require_locked(&conn).map_err(db_err)?.body;
            let (_, q) = super::protocol::review_question(&conn).map_err(db_err)?;
            (
                p,
                q,
                records::page_texts(&conn, &report_id).map_err(db_err)?,
            )
        };
        if pages.iter().all(|p| p.trim().is_empty()) {
            return Err(JobError::Fatal("the report has no extracted text".into()));
        }
        let text = records::paged_text(&pages, FT_MAX_CHARS);
        screen_target(
            ctx,
            Phase::FullText,
            "report",
            &report_id,
            &protocol,
            &question,
            &text,
            &pages,
            job.payload["retry"].as_bool().unwrap_or(false),
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Drift check (§9): re-screen a random 5% with Screener A and compare.

pub const DRIFT_JOB: &str = "drift_check";

pub struct DriftCheck;

#[async_trait]
impl JobHandler for DriftCheck {
    async fn run(&self, _job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        let (protocol, question, sample) = {
            let conn = ctx.open().map_err(db_err)?;
            let p = require_locked(&conn).map_err(db_err)?.body;
            let (rid, q) = super::protocol::review_question(&conn).map_err(db_err)?;
            let mut stmt = conn
                .prepare(
                    "SELECT target_id, decision FROM v_current_event WHERE stage = 'ta_screen' AND actor_id = 'screener_a'
                     AND decision IN ('include','exclude','uncertain') ORDER BY id",
                )
                .map_err(|e| JobError::Fatal(e.to_string()))?;
            let mut rows: Vec<(String, String)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| JobError::Fatal(e.to_string()))?
                .collect::<rusqlite::Result<_>>()
                .map_err(|e| JobError::Fatal(e.to_string()))?;
            let seed = rid
                .bytes()
                .fold(7u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64))
                ^ 0xD1F7;
            crate::stats::Rng::new(seed).shuffle(&mut rows);
            let n = (rows.len() / 20).clamp(rows.len().min(5), 50);
            rows.truncate(n);
            (p, q, rows)
        };
        let mut changed = 0;
        let mut done = 0;
        for (record_id, original) in &sample {
            let text = {
                let conn = ctx.open().map_err(db_err)?;
                match records::record(&conn, record_id).map_err(db_err)? {
                    Some(r) => r.screening_text(),
                    None => continue,
                }
            };
            match screener(
                ctx,
                Phase::TitleAbstract,
                'a',
                &protocol,
                &question,
                &text,
                &[],
            )
            .await
            {
                Ok(r) => {
                    done += 1;
                    if r.value.decision.as_str() != original {
                        changed += 1;
                    }
                }
                Err(AgentError::NeedsHuman(_)) => {}
                Err(e) => return Err(e.into()),
            }
        }
        let conn = ctx.open().map_err(db_err)?;
        let (review_id, _) = super::protocol::review_question(&conn).map_err(db_err)?;
        let divergence = if done > 0 {
            changed as f64 / done as f64
        } else {
            0.0
        };
        events::append(
            &conn,
            &NewEvent {
                review_id: review_id.clone(),
                target_type: "protocol".into(),
                target_id: review_id,
                stage: "ta_screen".into(),
                actor_type: Some(ActorType::Agent),
                actor_id: "drift_check".into(),
                decision: "drift_result".into(),
                reason_code: Some(done.to_string()),
                confidence: Some(divergence),
                rationale: Some(format!(
                    "Re-screened {done} records with Screener A; {changed} decisions changed ({:.0}%).{}",
                    divergence * 100.0,
                    if divergence > 0.10 { " Above the 10% alert threshold." } else { "" }
                )),
                ..Default::default()
            },
        )
        .map_err(db_err)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Queues and human decisions

/// Targets with no final decision whose arbiter said "uncertain" or failed: the human queue.
pub fn human_queue(conn: &Connection, stage: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT e.target_id FROM v_current_event e
         WHERE e.stage = ?1 AND e.actor_id = 'arbiter' AND e.decision IN ('uncertain', 'needs_human')
           AND e.target_id NOT IN (SELECT target_id FROM v_final_decision WHERE stage = ?1)
           -- Records set aside by a skip (e.g. citation searching) leave the queue.
           AND e.target_id NOT IN (SELECT rec.id FROM record rec
                                   WHERE rec.search_run_id NOT IN (SELECT id FROM v_current_run))
         ORDER BY e.id",
    )?;
    let ids = stmt
        .query_map([stage], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(ids)
}

/// Queue items where an agent gave no usable answer (invalid output twice),
/// or where the arbiter ruled before a screener's answer was replaced.
/// These can be sent back to the agents, e.g. after a parsing fix.
pub fn retryable_items(conn: &Connection, stage: &str) -> Result<Vec<String>> {
    let queue = human_queue(conn, stage)?;
    let mut stmt = conn.prepare(
        "SELECT 1 WHERE EXISTS (SELECT 1 FROM v_current_event WHERE stage = ?1 AND target_id = ?2
                                  AND actor_type = 'agent' AND decision = 'needs_human')
                   OR (SELECT MAX(id) FROM v_current_event WHERE stage = ?1 AND target_id = ?2 AND actor_id = 'arbiter')
                    < (SELECT MAX(id) FROM v_current_event WHERE stage = ?1 AND target_id = ?2
                         AND actor_id IN ('screener_a', 'screener_b'))",
    )?;
    let mut out = Vec::new();
    for id in queue {
        if stmt.exists(rusqlite::params![stage, id])? {
            out.push(id);
        }
    }
    Ok(out)
}

/// Enqueues an agent retry for every retryable queue item. Returns how many.
pub fn retry_with_agents(conn: &Connection, review_id: &str, stage: &str) -> Result<usize> {
    let (kind, key) = match stage {
        "ta_screen" => (TA_JOB, "record_id"),
        "ft_screen" => (FT_JOB, "report_id"),
        _ => {
            return Err(crate::error::Error::Invalid(format!(
                "cannot retry stage {stage}"
            )))
        }
    };
    let items = retryable_items(conn, stage)?;
    let attempt: i64 = conn.query_row(
        "SELECT COUNT(*) FROM job WHERE kind = ?1 AND json_extract(payload, '$.retry') IS NOT NULL",
        [kind],
        |r| r.get(0),
    )?;
    for id in &items {
        crate::jobs::enqueue_once(
            conn,
            review_id,
            kind,
            &serde_json::json!({ key: id, "retry": true, "attempt": attempt }),
        )?;
    }
    Ok(items.len())
}

/// Rationale that marks records carried forward by the review option.
pub const CARRIED_FORWARD: &str = "Still uncertain after the arbiter; carried forward to full-text screening (review option: unresolved title/abstract records go to full text).";

/// Carries title/abstract records the arbiter judged uncertain on to full
/// text, as an automated, disclosed rule. Unusable agent answers stay in the
/// human queue (they can be retried). Returns how many were carried.
pub fn carry_uncertain_forward(conn: &Connection, review_id: &str) -> Result<usize> {
    let mut n = 0;
    for id in human_queue(conn, "ta_screen")? {
        let arbiter = latest_by(conn, "ta_screen", &id, "arbiter")?;
        if arbiter.as_ref().map(|e| e.decision.as_str()) != Some("uncertain") {
            continue;
        }
        events::append(
            conn,
            &NewEvent {
                review_id: review_id.into(),
                target_type: "record".into(),
                target_id: id,
                stage: "ta_screen".into(),
                actor_type: Some(ActorType::Agent),
                actor_id: "resolver".into(),
                decision: "include".into(),
                rationale: Some(CARRIED_FORWARD.into()),
                ..Default::default()
            },
        )?;
        n += 1;
    }
    Ok(n)
}

/// Records the user's final decision on a screening target.
pub fn human_decide(
    conn: &Connection,
    stage: &str,
    target_type: &str,
    target_id: &str,
    decision: &str,
    reason_code: Option<&str>,
    note: Option<&str>,
) -> Result<()> {
    if !matches!(decision, "include" | "exclude") {
        return Err(crate::error::Error::Invalid(
            "Decide include or exclude.".into(),
        ));
    }
    let protocol = require_locked(conn)?.body;
    if decision == "exclude" {
        let code = reason_code
            .filter(|c| !c.is_empty())
            .ok_or_else(|| crate::error::Error::Invalid("Choose an exclusion reason.".into()))?;
        if !protocol.codes().contains(&code) {
            return Err(crate::error::Error::Invalid(format!(
                "{code} is not a criterion code."
            )));
        }
    }
    let (review_id, _) = super::protocol::review_question(conn)?;
    events::append(
        conn,
        &NewEvent {
            review_id,
            target_type: target_type.into(),
            target_id: target_id.into(),
            stage: stage.into(),
            actor_type: Some(ActorType::Human),
            actor_id: "human".into(),
            decision: decision.into(),
            reason_code: reason_code.map(str::to_owned).filter(|c| !c.is_empty()),
            rationale: note.map(str::to_owned),
            ..Default::default()
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {

    #[test]
    fn placeholder_reason_codes_mean_none() {
        assert_eq!(clean_code(Some("none")), None);
        assert_eq!(clean_code(Some(" N/A ")), None);
        assert_eq!(clean_code(None), None);
        assert_eq!(clean_code(Some("EX-POP")).as_deref(), Some("EX-POP"));
    }
    use super::*;

    #[test]
    fn only_unusable_agent_answers_are_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let review = crate::reviews::insert_review(&conn, "Q?").unwrap();
        let ev = |target: &str, actor: &str, decision: &str| NewEvent {
            review_id: review.clone(),
            target_type: "record".into(),
            target_id: target.into(),
            stage: "ta_screen".into(),
            actor_type: Some(ActorType::Agent),
            actor_id: actor.into(),
            decision: decision.into(),
            ..Default::default()
        };
        // r1: the arbiter's output was invalid twice. r2: the arbiter was genuinely unsure.
        events::append(&conn, &ev("r1", "screener_a", "include")).unwrap();
        events::append(&conn, &ev("r1", "arbiter", "needs_human")).unwrap();
        events::append(&conn, &ev("r2", "arbiter", "uncertain")).unwrap();
        assert_eq!(human_queue(&conn, "ta_screen").unwrap().len(), 2);
        assert_eq!(
            retryable_items(&conn, "ta_screen").unwrap(),
            vec!["r1".to_owned()]
        );
        // r3: the arbiter ruled, then a screener's unusable answer was replaced.
        events::append(&conn, &ev("r3", "arbiter", "uncertain")).unwrap();
        let old_b = events::append(&conn, &ev("r3", "screener_b", "needs_human")).unwrap();
        let mut fixed = ev("r3", "screener_b", "include");
        fixed.supersedes = Some(old_b);
        events::append(&conn, &fixed).unwrap();
        assert_eq!(
            retryable_items(&conn, "ta_screen").unwrap(),
            vec!["r1".to_owned(), "r3".to_owned()]
        );
        assert_eq!(retry_with_agents(&conn, &review, "ta_screen").unwrap(), 2);
        let payload: String = conn
            .query_row(
                "SELECT payload FROM job WHERE kind = 'screen_ta'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(payload.contains("\"retry\":true") && payload.contains("r1"));
        assert!(retry_with_agents(&conn, &review, "report").is_err());
    }

    #[test]
    fn uncertain_records_are_carried_forward_but_unusable_ones_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let review = crate::reviews::insert_review(&conn, "Q?").unwrap();
        for (t, d) in [("u1", "uncertain"), ("n1", "needs_human")] {
            events::append(
                &conn,
                &NewEvent {
                    review_id: review.clone(),
                    target_type: "record".into(),
                    target_id: t.into(),
                    stage: "ta_screen".into(),
                    actor_type: Some(ActorType::Agent),
                    actor_id: "arbiter".into(),
                    decision: d.into(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        assert_eq!(carry_uncertain_forward(&conn, &review).unwrap(), 1);
        assert_eq!(
            human_queue(&conn, "ta_screen").unwrap(),
            vec!["n1".to_owned()]
        );
        let (d, why): (String, String) = conn
            .query_row(
                "SELECT f.decision, e.rationale FROM v_final_decision f JOIN event e ON e.id = f.event_id WHERE f.target_id = 'u1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((d.as_str(), why.as_str()), ("include", CARRIED_FORWARD));
        assert_eq!(
            carry_uncertain_forward(&conn, &review).unwrap(),
            0,
            "idempotent"
        );
    }

    #[test]
    fn screener_criteria_list_is_requested_but_optional_locally() {
        let s = crate::agents::schema_for::<ScreenA>("screening_a");
        let desc = s.schema["properties"]["criteria"]["description"]
            .as_str()
            .unwrap_or("");
        assert!(desc.contains(crate::llm::MAY_BE_OMITTED));
        let text = r#"{"decision":"exclude","reason_code":"EX-SETTING","rationale":"US setting.",
                       "evidence_quote":"a public university in the Midwest","confidence":0.9}"#;
        let v = crate::llm::parse_and_validate(text, &s).unwrap();
        let parsed: ScreenA = serde_json::from_value(v).unwrap();
        assert!(parsed.criteria.is_empty());
    }
    use crate::agents::protocol::tests::sample_protocol;

    fn v(decision: Decision, code: Option<&str>, quote: &str, conf: f64) -> Verdict {
        Verdict {
            decision,
            reason_code: code.map(str::to_owned),
            rationale: "r".into(),
            evidence_quote: quote.into(),
            evidence_page: None,
            confidence: conf,
        }
    }

    #[test]
    fn verdict_checks() {
        let p = sample_protocol();
        let text = "Title: Belonging among Chinese students in England\nAbstract: We interviewed 20 students.";
        let src = Source::Text(text);
        assert!(check(
            &v(Decision::Include, None, "Chinese students in England", 0.9),
            &p,
            src,
            false
        )
        .is_ok());
        assert!(
            check(
                &v(Decision::Include, None, "Chinese students in England", 90.0),
                &p,
                src,
                false
            )
            .is_ok(),
            "percent accepted"
        );
        assert!(check(
            &v(Decision::Exclude, None, "We interviewed 20", 0.9),
            &p,
            src,
            false
        )
        .unwrap_err()
        .contains("reason_code"));
        assert!(check(
            &v(Decision::Exclude, Some("EX-NOPE"), "We interviewed 20", 0.9),
            &p,
            src,
            false
        )
        .unwrap_err()
        .contains("not a criterion"));
        assert!(check(
            &v(Decision::Include, None, "made up sentence", 0.9),
            &p,
            src,
            false
        )
        .unwrap_err()
        .contains("verbatim"));
        assert!(check(
            &v(Decision::Include, None, "Chinese students", 250.0),
            &p,
            src,
            false
        )
        .is_err());
    }

    #[test]
    fn unit_confidence_conversion() {
        assert_eq!(unit_confidence(0.4), Some(0.4));
        assert_eq!(unit_confidence(95.0), Some(0.95));
        assert_eq!(unit_confidence(-1.0), None);
    }
}
