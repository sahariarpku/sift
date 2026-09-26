//! Skipping a stage and keeping what it already did.
//!
//! The author may stop any stage after Gate 1. The skip is one human
//! `skipped` event (reason code `stage_skip`) on the review, at the stage's
//! event stage. Nothing already decided is undone: the pipeline carries on
//! with the work the stage finished, and the flow diagram counts the rest as
//! not screened, awaiting classification or not retrieved (see migration
//! 0005). Queued work for the stage is set aside, not deleted, so `resume`
//! can put it back. Every skip is disclosed in the report and leaves the
//! review marked as stopped early.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use crate::agents::{appraisal, extraction, prescreen, screening, search, synthesis};
use crate::error::{Error, Result};
use crate::events::{self, ActorType, NewEvent};
use crate::jobs;
use crate::report::manuscript;

use super::retrieval;

pub const REASON: &str = "stage_skip";

/// A stage the author can skip: its id in the stage list, the event stage the
/// skip is recorded at, a name for sentences, and the job kinds it sets aside.
pub struct Stage {
    pub id: &'static str,
    pub event_stage: &'static str,
    pub name: &'static str,
    pub jobs: &'static [&'static str],
}

pub const STAGES: &[Stage] = &[
    Stage {
        id: "search",
        event_stage: "search",
        name: "searching",
        jobs: &[
            search::SEARCH_JOB,
            search::SEED_SEARCH_JOB,
            search::SEED_CHECK_JOB,
            search::BROADEN_JOB,
            screening::SEED_JOB,
        ],
    },
    Stage {
        id: "screening",
        event_stage: "ta_screen",
        name: "title and abstract screening",
        jobs: &[screening::TA_JOB, prescreen::JOB, screening::DRIFT_JOB],
    },
    Stage {
        id: "fulltext",
        event_stage: "ft_screen",
        name: "full-text retrieval and screening",
        jobs: &[retrieval::RETRIEVE_JOB, screening::FT_JOB, search::CITATION_JOB],
    },
    Stage {
        id: "extraction",
        event_stage: "extract",
        name: "data extraction",
        jobs: &[extraction::EXTRACT_JOB],
    },
    Stage {
        id: "appraisal",
        event_stage: "appraise",
        name: "risk of bias and quality appraisal",
        jobs: &[appraisal::APPRAISE_JOB],
    },
    Stage {
        id: "synthesis",
        event_stage: "synth",
        name: "synthesis",
        jobs: &[synthesis::SYNTH_JOB],
    },
    Stage {
        id: "report",
        event_stage: "report",
        name: "manuscript drafting",
        jobs: &[manuscript::DRAFT_JOB],
    },
];

pub fn stage(id: &str) -> Result<&'static Stage> {
    STAGES
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| Error::Invalid(format!("{id} is not a stage that can be skipped.")))
}

/// The `last_error` given to jobs a skip set aside, so `resume` finds them.
pub fn set_aside_reason(s: &Stage) -> String {
    format!("set aside: {} skipped by the user", s.name)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Skip {
    pub stage: &'static str,
    pub name: &'static str,
    pub event_id: i64,
    pub at: String,
    pub rationale: String,
}

fn current_skip(conn: &Connection, s: &'static Stage) -> Result<Option<Skip>> {
    Ok(conn
        .query_row(
            "SELECT id, created_at, COALESCE(rationale, '') FROM v_current_event
             WHERE target_type = 'review' AND stage = ?1 AND decision = 'skipped' AND reason_code = ?2
             ORDER BY id DESC LIMIT 1",
            rusqlite::params![s.event_stage, REASON],
            |r| {
                Ok(Skip {
                    stage: s.id,
                    name: s.name,
                    event_id: r.get(0)?,
                    at: r.get(1)?,
                    rationale: r.get(2)?,
                })
            },
        )
        .optional()?)
}

/// Every stage skipped now, in pipeline order.
pub fn list(conn: &Connection) -> Result<Vec<Skip>> {
    let mut out = Vec::new();
    for s in STAGES {
        if let Some(k) = current_skip(conn, s)? {
            out.push(k);
        }
    }
    Ok(out)
}

/// Ids of the stages skipped now.
pub fn current(conn: &Connection) -> Result<HashSet<&'static str>> {
    Ok(list(conn)?.into_iter().map(|k| k.stage).collect())
}

/// What the skip keeps, in words, for the event rationale and the report.
fn kept(conn: &Connection, s: &Stage) -> Result<String> {
    let n = |sql: &str| -> Result<i64> { Ok(conn.query_row(sql, [], |r| r.get(0))?) };
    Ok(match s.id {
        "search" => format!(
            "Searching was stopped by the review author; the {} records already found were kept.",
            n("SELECT COUNT(*) FROM v_identified")?
        ),
        "screening" => format!(
            "Title and abstract screening was stopped by the review author after {} of {} records had a final decision. The rest were not screened and do not go forward.",
            n("SELECT COUNT(*) FROM v_final_decision f JOIN v_screenable s ON s.id = f.target_id WHERE f.stage = 'ta_screen'")?,
            n("SELECT COUNT(*) FROM v_screenable")?
        ),
        "fulltext" => format!(
            "Full-text retrieval and screening were stopped by the review author after {} reports had a final decision. Reports without one are reported as awaiting classification or not retrieved.",
            n("SELECT COUNT(*) FROM v_final_decision WHERE stage = 'ft_screen'")?
        ),
        "extraction" => format!(
            "Data extraction was stopped by the review author after {} studies were extracted. Only those studies contribute extracted data.",
            n("SELECT COUNT(DISTINCT target_id) FROM v_current_event WHERE stage = 'extract' AND target_type = 'study' AND decision = 'extracted'")?
        ),
        "appraisal" => format!(
            "Appraisal was stopped by the review author after {} studies were appraised. The others are reported as not appraised.",
            n("SELECT COUNT(DISTINCT study_id) FROM appraisal")?
        ),
        "synthesis" => "Automated synthesis was stopped by the review author; the manuscript uses whatever synthesis had finished.".to_owned(),
        _ => "The review author chose to write the manuscript sections without automated drafts.".to_owned(),
    })
}

/// Skips a stage. Work already finished stays; queued work is set aside.
pub fn skip(conn: &Connection, review_id: &str, id: &str) -> Result<Skip> {
    let s = stage(id)?;
    if let Some(k) = current_skip(conn, s)? {
        return Ok(k);
    }
    if crate::agents::protocol::locked(conn)?.is_none() {
        return Err(Error::Invalid(
            "Lock the protocol first. Stages can be skipped once the review is running.".into(),
        ));
    }
    if s.id == "search" {
        let found: i64 = conn.query_row("SELECT COUNT(*) FROM v_identified", [], |r| r.get(0))?;
        if found == 0 {
            return Err(Error::Invalid(
                "Searching cannot be skipped before any records are found.".into(),
            ));
        }
    }
    let rationale = kept(conn, s)?;
    events::append(
        conn,
        &NewEvent {
            review_id: review_id.to_owned(),
            target_type: "review".into(),
            target_id: review_id.to_owned(),
            stage: s.event_stage.into(),
            actor_type: Some(ActorType::Human),
            actor_id: "human".into(),
            decision: "skipped".into(),
            reason_code: Some(REASON.into()),
            rationale: Some(rationale),
            ..Default::default()
        },
    )?;
    let reason = set_aside_reason(s);
    for kind in s.jobs {
        jobs::set_aside(conn, kind, None, &reason)?;
    }
    current_skip(conn, s)?.ok_or_else(|| Error::Invalid("The skip was not recorded.".into()))
}

/// Undoes a skip: a `resumed` event supersedes it and the set-aside jobs are
/// queued again. The pipeline then picks up where the stage stopped.
pub fn resume(conn: &Connection, review_id: &str, id: &str) -> Result<()> {
    let s = stage(id)?;
    let Some(k) = current_skip(conn, s)? else {
        return Ok(());
    };
    events::append(
        conn,
        &NewEvent {
            review_id: review_id.to_owned(),
            target_type: "review".into(),
            target_id: review_id.to_owned(),
            stage: s.event_stage.into(),
            actor_type: Some(ActorType::Human),
            actor_id: "human".into(),
            decision: "resumed".into(),
            reason_code: Some(REASON.into()),
            rationale: Some(format!("The review author resumed {}.", s.name)),
            supersedes: Some(k.event_id),
            ..Default::default()
        },
    )?;
    conn.execute(
        "UPDATE job SET status = 'queued', last_error = NULL, updated_at = ?1
         WHERE status = 'done' AND last_error = ?2",
        rusqlite::params![crate::time::now_iso(), set_aside_reason(s)],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use serde_json::json;

    fn lock(conn: &Connection, review: &str) {
        let body =
            serde_json::to_string(&crate::agents::protocol::tests::sample_protocol()).unwrap();
        conn.execute(
            "INSERT INTO protocol_version (id, review_id, version, body) VALUES ('p', ?1, 0, ?2)",
            params![review, body],
        )
        .unwrap();
        crate::agents::protocol::lock(conn, true).unwrap();
    }

    fn setup() -> (tempfile::TempDir, Connection, String) {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let review = crate::reviews::insert_review(&conn, "Q?").unwrap();
        lock(&conn, &review);
        conn.execute(
            "INSERT INTO search_run (id, review_id, source, query_string, query_ast, run_at, hits) VALUES ('s1', ?1, 'openalex', 'q', '{}', '2026-09-25T10:00:00Z', 4)",
            [&review],
        )
        .unwrap();
        for rec in ["a", "b", "c", "d"] {
            conn.execute(
                "INSERT INTO record (id, search_run_id, title) VALUES (?1, 's1', ?1)",
                [rec],
            )
            .unwrap();
        }
        (dir, conn, review)
    }

    fn decide(conn: &Connection, review: &str, stage: &str, target: &str, decision: &str) {
        events::append(
            conn,
            &NewEvent {
                review_id: review.into(),
                target_type: if stage == "ta_screen" { "record" } else { "report" }.into(),
                target_id: target.into(),
                stage: stage.into(),
                actor_type: Some(ActorType::Agent),
                actor_id: "resolver".into(),
                decision: decision.into(),
                reason_code: (decision == "exclude").then(|| "EX-POP".into()),
                ..Default::default()
            },
        )
        .unwrap();
    }

    fn counts(conn: &Connection) -> (i64, i64, i64, i64, i64, i64) {
        conn.query_row(
            "SELECT records_screened, records_not_screened, records_excluded, reports_sought, reports_assessed, reports_awaiting FROM v_prisma_counts",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap()
    }

    #[test]
    fn screening_skip_counts_only_decided_records_and_resume_restores() {
        let (_d, conn, review) = setup();
        decide(&conn, &review, "ta_screen", "a", "include");
        decide(&conn, &review, "ta_screen", "b", "exclude");
        jobs::enqueue(&conn, &review, screening::TA_JOB, &json!({"record_id": "c"})).unwrap();
        jobs::enqueue(&conn, &review, screening::TA_JOB, &json!({"record_id": "d"})).unwrap();
        assert_eq!(counts(&conn), (4, 0, 1, 1, 0, 0));

        let k = skip(&conn, &review, "screening").unwrap();
        assert_eq!(k.stage, "screening");
        assert!(k.rationale.contains("2 of 4"));
        assert_eq!(counts(&conn), (2, 2, 1, 1, 0, 0));
        assert_eq!(jobs::outstanding(&conn, &[screening::TA_JOB]).unwrap(), 0);
        assert!(current(&conn).unwrap().contains("screening"));
        // Skipping twice records one event.
        skip(&conn, &review, "screening").unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM event WHERE decision = 'skipped'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);

        resume(&conn, &review, "screening").unwrap();
        assert!(current(&conn).unwrap().is_empty());
        assert_eq!(counts(&conn), (4, 0, 1, 1, 0, 0));
        assert_eq!(jobs::outstanding(&conn, &[screening::TA_JOB]).unwrap(), 2);
    }

    #[test]
    fn fulltext_skip_keeps_decided_reports_and_counts_the_rest() {
        let (_d, conn, review) = setup();
        for rec in ["a", "b", "c"] {
            decide(&conn, &review, "ta_screen", rec, "include");
        }
        decide(&conn, &review, "ta_screen", "d", "exclude");
        // a and b retrieved; c still waiting for retrieval.
        for (rep, rec) in [("ra", "a"), ("rb", "b")] {
            conn.execute(
                "INSERT INTO report (id, record_id, retrieval_status) VALUES (?1, ?2, 'open_access')",
                params![rep, rec],
            )
            .unwrap();
            events::append(
                &conn,
                &NewEvent {
                    review_id: review.clone(),
                    target_type: "report".into(),
                    target_id: rep.into(),
                    stage: "retrieval".into(),
                    actor_type: Some(ActorType::Agent),
                    actor_id: "retrieval".into(),
                    decision: "open_access".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        decide(&conn, &review, "ft_screen", "ra", "include");
        let before = counts(&conn);
        assert_eq!((before.4, before.5), (2, 0));

        skip(&conn, &review, "fulltext").unwrap();
        let (assessed, awaiting, not_retrieved): (i64, i64, i64) = conn
            .query_row(
                "SELECT reports_assessed, reports_awaiting, reports_not_retrieved FROM v_prisma_counts",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        // ra assessed; rb retrieved but never classified; c never retrieved.
        assert_eq!((assessed, awaiting, not_retrieved), (1, 1, 1));
        // Sought = assessed + awaiting + not retrieved.
        assert_eq!(counts(&conn).3, 3);
    }

    #[test]
    fn search_skip_needs_records_and_unknown_stages_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let review = crate::reviews::insert_review(&conn, "Q?").unwrap();
        lock(&conn, &review);
        assert!(skip(&conn, &review, "search").is_err());
        assert!(skip(&conn, &review, "gate2").is_err());
    }
}
