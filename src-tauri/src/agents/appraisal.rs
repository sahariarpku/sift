//! Quality appraisal (§8.9). The tool follows the study design: RoB 2
//! (randomised trials), ROBINS-I (non-randomised interventions), JBI
//! (cross-sectional), CASP (qualitative), MMAT (mixed methods). Two
//! independent passes; disagreements per domain go to the arbiter, as in
//! screening. Every judgement needs a rationale, and a quote that is verbatim
//! in the text when one is given. A quote that is not verbatim voids that
//! domain's quote only (§8.8 treats extracted values the same way); a domain
//! left with no verified support is flagged for Gate 3.

use async_trait::async_trait;
use rusqlite::{params, Connection};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::protocol::{require_locked, ProtocolBody};
use super::{call, par, AgentError, CallMeta};
use crate::error::{Error, Result};
use crate::events::{self, ActorType, NewEvent};
use crate::jobs::{Job, JobContext, JobError, JobHandler};
use crate::llm::store::Task;
use crate::records;
use crate::reviews::new_id;
use crate::validate;

pub const APPRAISE_JOB: &str = "appraise";
/// v2 spells out the JSON shape and repeats it after the full text, which
/// long-context local models otherwise lose. v3 adds stricter rules for
/// copying quotes.
pub const PROMPT: &str = "appraisal.v3";
const QUOTE_VOIDED: &str = "Quote not found in the text.";
const MAX_CHARS: usize = 110_000;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Tool {
    pub id: String,
    pub name: String,
    pub judgements: Vec<String>,
    pub domains: Vec<String>,
}

#[derive(Deserialize)]
struct ToolsFile {
    tools: Vec<Tool>,
}

pub fn tools() -> Vec<Tool> {
    serde_json::from_str::<ToolsFile>(include_str!("appraisal_tools.json"))
        .map(|f| f.tools)
        .unwrap_or_default()
}

pub fn tool(id: &str) -> Option<Tool> {
    tools().into_iter().find(|t| t.id == id)
}

/// Chooses the tool from the extracted study design text (§8.9).
pub fn choose_tool(design: Option<&str>, protocol: &ProtocolBody) -> &'static str {
    let d = design.unwrap_or("").to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| d.contains(w));
    if has(&["mixed method", "mixed-method", "multi-method"]) {
        "MMAT"
    } else if has(&["randomised", "randomized", "rct"])
        && !has(&["non-random", "nonrandom", "quasi"])
    {
        "RoB2"
    } else if has(&[
        "quasi",
        "non-random",
        "nonrandom",
        "controlled before",
        "pre-post",
        "pre/post",
        "pretest",
        "pre-test",
        "intervention study",
    ]) {
        "ROBINS-I"
    } else if has(&[
        "qualitative",
        "interview",
        "focus group",
        "ethnograph",
        "phenomenolog",
        "narrative inquiry",
        "grounded theory",
        "case study",
        "thematic",
    ]) {
        "CASP"
    } else if has(&[
        "survey",
        "cross-sectional",
        "cross sectional",
        "questionnaire",
        "correlational",
    ]) {
        "JBI"
    } else if protocol.is_qualitative() {
        "CASP"
    } else {
        "JBI"
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DomainJudgement {
    pub domain: String,
    pub judgement: String,
    pub rationale: String,
    pub source_quote: Option<String>,
    pub page: Option<i64>,
    /// Set by `void_unfound_quotes`, never by the model.
    #[serde(skip)]
    pub quote_voided: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AppraisalOut {
    pub domains: Vec<DomainJudgement>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ReconcileOut {
    pub judgement: String,
    pub rationale: String,
    pub source_quote: Option<String>,
    pub page: Option<i64>,
}

fn check_quote(
    q: &Option<String>,
    page: Option<i64>,
    pages: &[String],
) -> std::result::Result<(), String> {
    match q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        Some(q) if validate::locate(q, pages, page).is_none() => {
            Err(format!("quote not found in the text: {q:.80}"))
        }
        _ => Ok(()),
    }
}

/// Structural check of one pass: every domain present with an allowed
/// judgement. Quotes are checked per domain by `void_unfound_quotes`, so one
/// loose quote does not discard the whole pass.
pub fn check(out: &AppraisalOut, tool: &Tool) -> std::result::Result<(), String> {
    for d in &tool.domains {
        let Some(j) = out.domains.iter().find(|x| x.domain.trim() == d) else {
            return Err(format!("missing domain: {d}"));
        };
        if !tool.judgements.contains(&j.judgement) {
            return Err(format!(
                "judgement {:?} for {d} must be one of {:?}",
                j.judgement, tool.judgements
            ));
        }
    }
    Ok(())
}

/// Removes quotes that are not verbatim in the text, noting it in the
/// rationale, so the judgement no longer claims support it does not have.
pub fn void_unfound_quotes(out: &mut AppraisalOut, pages: &[String]) {
    for d in &mut out.domains {
        if check_quote(&d.source_quote, d.page, pages).is_err() {
            d.source_quote = None;
            d.page = None;
            d.quote_voided = true;
            d.rationale = format!("{} ({QUOTE_VOIDED})", d.rationale.trim_end());
        }
    }
}

/// When both passes agree: the pass with verified support, or `None` when
/// neither pass has any.
fn agreed<'a>(a: &'a DomainJudgement, b: &'a DomainJudgement) -> Option<&'a DomainJudgement> {
    [a, b].into_iter().find(|d| !d.quote_voided)
}

/// Whether a study needs no further appraisal: it was appraised, or the
/// current prompt already failed on it and it waits for the user. A failure
/// under an older prompt version is retried once with the current one.
pub fn settled(conn: &Connection, study_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM appraisal WHERE study_id = ?1)
         OR EXISTS(SELECT 1 FROM v_current_event WHERE stage = 'appraise' AND target_type = 'study' AND target_id = ?1
                   AND decision = 'needs_human' AND prompt_version = ?2)",
        params![study_id, PROMPT],
        |r| r.get(0),
    )?)
}

/// The study's open "needs your review" appraisal event, which a new attempt
/// supersedes.
fn open_failure(conn: &Connection, study_id: &str) -> Result<Option<i64>> {
    Ok(conn.query_row(
        "SELECT MAX(id) FROM v_current_event
         WHERE stage = 'appraise' AND target_type = 'study' AND target_id = ?1 AND decision = 'needs_human'",
        [study_id],
        |r| r.get(0),
    )?)
}

fn domain_list(tool: &Tool) -> String {
    let mut s = String::new();
    for d in &tool.domains {
        s.push_str(&format!("- {d}\n"));
    }
    s.push_str(&format!(
        "Allowed judgements: {}\n",
        tool.judgements.join(", ")
    ));
    s.push_str("Use each domain's text exactly as written above in the domain field.");
    s
}

fn study_design(conn: &Connection, study_id: &str) -> Result<Option<String>> {
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM extraction_value WHERE study_id = ?1 AND field = 'design' AND validation = 'verified'",
            [study_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    Ok(v.and_then(|s| serde_json::from_str::<String>(&s).ok()))
}

fn domain_event(
    meta: &CallMeta,
    review_id: &str,
    study_id: &str,
    actor: &str,
    d: &DomainJudgement,
) -> NewEvent {
    let mut e = meta.event(
        review_id,
        "study",
        study_id,
        "appraise",
        actor,
        &d.judgement,
    );
    e.reason_code = Some(d.domain.chars().take(120).collect());
    e.rationale = Some(d.rationale.clone());
    e.evidence_quote = d.source_quote.clone();
    e
}

pub struct Appraise;

#[async_trait]
impl JobHandler for Appraise {
    async fn run(&self, job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        let study_id = job.payload["study_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let err = |e: Error| JobError::Fatal(e.to_string());
        let (review_id, tool, pages, label) = {
            let conn = ctx.open().map_err(err)?;
            // An older queued job for a study already settled under the
            // current prompt has nothing left to do.
            if settled(&conn, &study_id).map_err(err)? {
                return Ok(());
            }
            let (rid, _) = super::protocol::review_question(&conn).map_err(err)?;
            let p = require_locked(&conn).map_err(err)?.body;
            let design = study_design(&conn, &study_id).map_err(err)?;
            let tool = tool(choose_tool(design.as_deref(), &p))
                .ok_or_else(|| JobError::Fatal("appraisal tools missing".into()))?;
            let rep = super::extraction::primary_report(&conn, &study_id)
                .map_err(err)?
                .ok_or_else(|| JobError::Fatal("no report".into()))?;
            let label = super::extraction::study_label(&conn, &study_id).map_err(err)?;
            (rid, tool, records::page_texts(&conn, &rep).map_err(err)?, label)
        };
        let live = ctx
            .env
            .live
            .track_in(&review_id, "appraisal", &study_id, &label);
        live.step(format!(
            "Two independent {} assessments across {} domains",
            tool.name,
            tool.domains.len()
        ));
        let vars = [
            ("tool", tool.name.clone()),
            ("domains", domain_list(&tool)),
            ("report", records::paged_text(&pages, MAX_CHARS)),
        ];
        let run =
            |task| call::<AppraisalOut, _>(ctx, task, PROMPT, &vars, 6000, |o| check(o, &tool));
        let (a, b) = tokio::join!(run(Task::Appraisal), run(Task::ScreenerB));
        let (mut a, mut b) = match (a, b) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(AgentError::NeedsHuman(w)), _) | (_, Err(AgentError::NeedsHuman(w))) => {
                let conn = ctx.open().map_err(err)?;
                let supersedes = open_failure(&conn, &study_id).map_err(err)?;
                events::append(
                    &conn,
                    &NewEvent {
                        review_id,
                        target_type: "study".into(),
                        target_id: study_id,
                        stage: "appraise".into(),
                        actor_type: Some(ActorType::Agent),
                        actor_id: "appraiser".into(),
                        decision: "needs_human".into(),
                        rationale: Some(w),
                        prompt_version: Some(PROMPT.into()),
                        supersedes,
                        ..Default::default()
                    },
                )
                .map_err(err)?;
                return Ok(());
            }
            (Err(e), _) | (_, Err(e)) => return Err(e.into()),
        };
        void_unfound_quotes(&mut a.value, &pages);
        void_unfound_quotes(&mut b.value, &pages);
        {
            let conn = ctx.open().map_err(err)?;
            for d in &a.value.domains {
                events::append(
                    &conn,
                    &domain_event(&a.meta, &review_id, &study_id, "appraiser_a", d),
                )
                .map_err(err)?;
            }
            for d in &b.value.domains {
                events::append(
                    &conn,
                    &domain_event(&b.meta, &review_id, &study_id, "appraiser_b", d),
                )
                .map_err(err)?;
            }
        }
        // Agreed domains settle directly; disagreements go to the arbiter,
        // all at once, and the results are put back in domain order.
        enum Slot<'a> {
            Settled(DomainJudgement, &'static str),
            Disputed(&'a DomainJudgement, &'a DomainJudgement),
        }
        let mut slots: Vec<(&String, Slot)> = Vec::new();
        for domain in &tool.domains {
            let (Some(da), Some(db)) = (
                a.value.domains.iter().find(|x| x.domain.trim() == domain),
                b.value.domains.iter().find(|x| x.domain.trim() == domain),
            ) else {
                continue;
            };
            if da.judgement != db.judgement {
                slots.push((domain, Slot::Disputed(da, db)));
                continue;
            }
            slots.push((
                domain,
                match agreed(da, db) {
                    Some(d) => Slot::Settled(d.clone(), "resolver"),
                    None => Slot::Settled(
                        DomainJudgement {
                            rationale: format!(
                                "Neither pass quoted the text verbatim; check at sign-off. {}",
                                da.rationale
                            ),
                            ..da.clone()
                        },
                        "needs_human",
                    ),
                },
            ));
        }
        let disputed: Vec<(&String, &DomainJudgement, &DomainJudgement)> = slots
            .iter()
            .filter_map(|(domain, s)| match s {
                Slot::Disputed(da, db) => Some((*domain, *da, *db)),
                Slot::Settled(..) => None,
            })
            .collect();
        if !disputed.is_empty() {
            live.step(format!(
                "Reconciling {} of {} domains where the two assessments disagree",
                disputed.len(),
                slots.len()
            ));
        }
        let mut rulings = par::bounded(disputed, par::LIMIT, |(domain, da, db)| {
            let (tool, pages) = (&tool, &pages);
            async move {
                let rvars = [
                    ("tool", tool.name.clone()),
                    ("domain", domain.clone()),
                    ("allowed", tool.judgements.join(", ")),
                    ("a", serde_json::to_string(da).unwrap_or_default()),
                    ("b", serde_json::to_string(db).unwrap_or_default()),
                ];
                call::<ReconcileOut, _>(
                    ctx,
                    Task::Arbiter,
                    "appraisal_reconcile.v1",
                    &rvars,
                    1200,
                    |o| {
                        if !tool.judgements.contains(&o.judgement) {
                            return Err(format!("judgement must be one of {:?}", tool.judgements));
                        }
                        check_quote(&o.source_quote, o.page, pages)
                    },
                )
                .await
            }
        })
        .await
        .into_iter();
        let mut finals: Vec<(DomainJudgement, &'static str, Option<CallMeta>)> = Vec::new();
        for (domain, slot) in slots {
            let (da, r) = match slot {
                Slot::Settled(d, actor) => {
                    finals.push((d, actor, None));
                    continue;
                }
                Slot::Disputed(da, _) => (
                    da,
                    rulings
                        .next()
                        .ok_or_else(|| JobError::Fatal("reconcile result missing".into()))?,
                ),
            };
            match r {
                Ok(r) => finals.push((
                    DomainJudgement {
                        domain: domain.clone(),
                        judgement: r.value.judgement,
                        rationale: r.value.rationale,
                        source_quote: r.value.source_quote,
                        page: r.value.page,
                        // The arbiter's quote is checked strictly before it is accepted.
                        quote_voided: false,
                    },
                    "arbiter",
                    Some(r.meta),
                )),
                // Unresolved: keep the more cautious-looking first pass and flag it for Gate 3.
                Err(AgentError::NeedsHuman(w)) => finals.push((
                    DomainJudgement {
                        rationale: format!("Unresolved disagreement; check at sign-off. {w}"),
                        ..da.clone()
                    },
                    "needs_human",
                    None,
                )),
                Err(e) => return Err(e.into()),
            }
        }
        let conn = ctx.open().map_err(err)?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| JobError::Fatal(e.to_string()))?;
        tx.execute("DELETE FROM appraisal WHERE study_id = ?1", [&study_id])
            .map_err(|e| JobError::Fatal(e.to_string()))?;
        // A successful retry closes the study's earlier "needs your review" event.
        let mut failure = open_failure(&tx, &study_id).map_err(err)?;
        for (d, actor, meta) in finals {
            let page = d
                .source_quote
                .as_deref()
                .and_then(|q| validate::locate(q, &pages, d.page));
            let id = new_id();
            tx.execute(
                "INSERT INTO appraisal (id, study_id, tool, domain, judgement, rationale, source_quote, page) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![id, study_id, tool.id, d.domain, d.judgement, d.rationale, d.source_quote, page],
            )
            .map_err(|e| JobError::Fatal(e.to_string()))?;
            let mut e = match &meta {
                Some(m) => m.event(
                    &review_id,
                    "appraisal",
                    &id,
                    "appraise",
                    actor,
                    &d.judgement,
                ),
                None => NewEvent {
                    review_id: review_id.clone(),
                    target_type: "appraisal".into(),
                    target_id: id.clone(),
                    stage: "appraise".into(),
                    actor_type: Some(ActorType::Agent),
                    actor_id: actor.into(),
                    decision: d.judgement.clone(),
                    ..Default::default()
                },
            };
            e.reason_code = Some(tool.id.clone());
            e.rationale = Some(format!("{}: {}", d.domain, d.rationale));
            e.evidence_quote = d.source_quote.clone();
            e.supersedes = failure.take();
            events::append(&tx, &e).map_err(err)?;
        }
        tx.commit().map_err(|e| JobError::Fatal(e.to_string()))?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppraisalRow {
    pub study_id: String,
    pub tool: String,
    pub domain: String,
    pub judgement: String,
    pub rationale: String,
    pub source_quote: Option<String>,
    pub page: Option<i64>,
}

pub fn rows(conn: &Connection) -> Result<Vec<AppraisalRow>> {
    let mut stmt = conn.prepare("SELECT study_id, tool, domain, judgement, rationale, source_quote, page FROM appraisal ORDER BY study_id, rowid")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(AppraisalRow {
                study_id: r.get(0)?,
                tool: r.get(1)?,
                domain: r.get(2)?,
                judgement: r.get(3)?,
                rationale: r.get(4)?,
                source_quote: r.get(5)?,
                page: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// One-line summary per study, for CERQual and GRADE drafting.
pub fn summary(conn: &Connection, study_id: &str) -> Result<String> {
    let rows: Vec<AppraisalRow> = rows(conn)?
        .into_iter()
        .filter(|r| r.study_id == study_id)
        .collect();
    if rows.is_empty() {
        return Ok("not appraised".into());
    }
    let tool = rows[0].tool.clone();
    let parts: Vec<String> = rows
        .iter()
        .map(|r| format!("{}: {}", r.domain, r.judgement))
        .collect();
    Ok(format!("{tool}. {}", parts.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::protocol::tests::sample_protocol;

    #[test]
    fn tools_load_and_choice_follows_design() {
        assert_eq!(tools().len(), 5);
        assert_eq!(tool("CASP").unwrap().domains.len(), 10);
        let p = sample_protocol();
        assert_eq!(
            choose_tool(Some("Semi-structured interviews, thematic analysis"), &p),
            "CASP"
        );
        assert_eq!(
            choose_tool(Some("Cluster randomised controlled trial"), &p),
            "RoB2"
        );
        assert_eq!(
            choose_tool(Some("Quasi-experimental pre-post design"), &p),
            "ROBINS-I"
        );
        assert_eq!(
            choose_tool(Some("Cross-sectional online survey"), &p),
            "JBI"
        );
        assert_eq!(
            choose_tool(Some("Explanatory sequential mixed methods"), &p),
            "MMAT"
        );
        assert_eq!(
            choose_tool(None, &p),
            "CASP",
            "falls back to the protocol's question type"
        );
    }

    #[test]
    fn check_requires_every_domain_and_valid_judgements() {
        let t = tool("MMAT").unwrap();
        let mk = |judgement: &str| AppraisalOut {
            domains: t
                .domains
                .iter()
                .map(|d| DomainJudgement {
                    domain: d.clone(),
                    judgement: judgement.into(),
                    rationale: "r".into(),
                    source_quote: None,
                    page: None,
                    quote_voided: false,
                })
                .collect(),
        };
        assert!(check(&mk("Yes"), &t).is_ok());
        assert!(check(&mk("Maybe"), &t).is_err());
        let mut missing = mk("Yes");
        missing.domains.pop();
        assert!(check(&missing, &t).unwrap_err().contains("missing domain"));
    }

    #[test]
    fn an_unfound_quote_voids_only_its_own_domain() {
        let pages = vec!["We integrated survey and interview data in a joint display.".to_owned()];
        let d = |quote: &str| DomainJudgement {
            domain: "d".into(),
            judgement: "Yes".into(),
            rationale: "r".into(),
            source_quote: Some(quote.into()),
            page: Some(1),
            quote_voided: false,
        };
        let mut out = AppraisalOut {
            domains: vec![
                d("integrated survey and interview data"),
                d("a quote that is not there"),
            ],
        };
        void_unfound_quotes(&mut out, &pages);
        let (good, bad) = (&out.domains[0], &out.domains[1]);
        assert!(!good.quote_voided);
        assert!(good.source_quote.is_some());
        assert!(bad.quote_voided);
        assert_eq!((bad.source_quote.as_deref(), bad.page), (None, None));
        assert!(bad.rationale.contains(QUOTE_VOIDED));
        // Agreeing passes keep the one with verified support, and none when
        // neither has any.
        assert!(!agreed(bad, good).unwrap().quote_voided);
        assert!(agreed(bad, bad).is_none());
        // The flag is never read from, or offered to, the model.
        let json = serde_json::to_string(bad).unwrap();
        assert!(!json.contains("quote_voided"));
        let schema = serde_json::to_string(&schemars::schema_for!(DomainJudgement)).unwrap();
        assert!(!schema.contains("quote_voided"));
    }

    #[test]
    fn a_failure_under_an_older_prompt_is_retried_once() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let review = crate::reviews::insert_review(&conn, "Q?").unwrap();
        let fail = |prompt: &str, supersedes: Option<i64>| {
            events::append(
                &conn,
                &NewEvent {
                    review_id: review.clone(),
                    target_type: "study".into(),
                    target_id: "s".into(),
                    stage: "appraise".into(),
                    actor_type: Some(ActorType::Agent),
                    actor_id: "appraiser".into(),
                    decision: "needs_human".into(),
                    prompt_version: Some(prompt.into()),
                    supersedes,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        assert!(!settled(&conn, "s").unwrap());
        let old = fail("appraisal.v2", None);
        assert!(!settled(&conn, "s").unwrap());
        assert_eq!(open_failure(&conn, "s").unwrap(), Some(old));
        fail(PROMPT, open_failure(&conn, "s").unwrap());
        assert!(settled(&conn, "s").unwrap());
    }
}
