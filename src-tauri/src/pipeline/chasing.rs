//! Skipping citation searching (§8.6).
//!
//! PRISMA 2020 does not require citation searching; it requires reporting
//! what was done. The author may skip it before its records are screened.
//! The skip is one human `skipped` event (stage `search`, reason code
//! `citation_chasing`); `v_current_run` then leaves out every citation run,
//! so its records drop out of screening and the flow diagram, and the
//! manuscript discloses the skip under information sources (item 6).

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use crate::agents::{screening, search};
use crate::error::{Error, Result};
use crate::events::{self, ActorType, NewEvent};
use crate::jobs;
use crate::local::laya;

pub const REASON: &str = "citation_chasing";
const SET_ASIDE: &str = "set aside: citation searching skipped by the user";

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Skipped {
    pub at: String,
    /// Unique records the citation search had returned (0 if it never ran).
    pub records: i64,
}

/// Unique records from citation runs, whether or not they are skipped.
fn chased_records(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT rec.id FROM record rec JOIN search_run run ON run.id = rec.search_run_id
         WHERE run.source = 'citation_chasing'
           AND rec.id NOT IN (SELECT record_id FROM v_duplicate)",
    )?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn skipped(conn: &Connection) -> Result<Option<Skipped>> {
    let at: Option<String> = conn
        .query_row(
            "SELECT created_at FROM v_current_event
             WHERE stage = 'search' AND decision = 'skipped' AND reason_code = ?1
             ORDER BY id LIMIT 1",
            [REASON],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match at {
        Some(at) => Some(Skipped {
            at,
            records: chased_records(conn)?.len() as i64,
        }),
        None => None,
    })
}

fn at_full_text(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM report rep JOIN record rec ON rec.id = rep.record_id
         JOIN search_run run ON run.id = rec.search_run_id
         WHERE run.source = 'citation_chasing'",
        [],
        |r| r.get(0),
    )?)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChasingState {
    /// off | pending | running | screening | done | skipped
    pub status: &'static str,
    /// Unique records the citation search returned.
    pub records: i64,
    /// Of those, how many have a final title/abstract decision.
    pub screened: i64,
    pub can_skip: bool,
}

/// Where citation searching stands, for the Overview.
pub fn state(conn: &Connection, enabled: bool) -> Result<ChasingState> {
    let records = chased_records(conn)?;
    let screened: i64 = conn.query_row(
        "SELECT COUNT(*) FROM v_final_decision f JOIN record rec ON rec.id = f.target_id
         JOIN search_run run ON run.id = rec.search_run_id
         WHERE f.stage = 'ta_screen' AND run.source = 'citation_chasing'",
        [],
        |r| r.get(0),
    )?;
    let ran: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM event WHERE stage = 'search' AND decision = 'citation_chasing')",
        [],
        |r| r.get(0),
    )?;
    let status = if skipped(conn)?.is_some() {
        "skipped"
    } else if !enabled {
        "off"
    } else if jobs::outstanding(conn, &[search::CITATION_JOB])? > 0 {
        "running"
    } else if !ran {
        "pending"
    } else if (screened as usize) < records.len() {
        "screening"
    } else {
        "done"
    };
    Ok(ChasingState {
        status,
        records: records.len() as i64,
        screened,
        can_skip: matches!(status, "pending" | "running" | "screening") && at_full_text(conn)? == 0,
    })
}

/// Skips citation searching and sets aside the work queued for its records.
/// Refused once any of its records has reached full-text retrieval, since
/// taking them out then would change decisions already made.
pub fn skip(conn: &Connection, review_id: &str) -> Result<Skipped> {
    if let Some(s) = skipped(conn)? {
        return Ok(s);
    }
    let records = chased_records(conn)?;
    let reached = at_full_text(conn)?;
    if reached > 0 {
        return Err(Error::Invalid(format!(
            "Citation searching can no longer be skipped: {reached} of its records have reached full text."
        )));
    }
    let rationale = if records.is_empty() {
        "The review author chose not to search the citations of included studies.".to_owned()
    } else {
        format!(
            "The review author stopped citation searching before its {} records were screened. \
             They are left out of the flow diagram, and the skip is reported under information sources.",
            records.len()
        )
    };
    events::append(
        conn,
        &NewEvent {
            review_id: review_id.to_owned(),
            target_type: "review".into(),
            target_id: review_id.to_owned(),
            stage: "search".into(),
            actor_type: Some(ActorType::Human),
            actor_id: "human".into(),
            decision: "skipped".into(),
            reason_code: Some(REASON.into()),
            rationale: Some(rationale),
            ..Default::default()
        },
    )?;
    let ids: HashSet<String> = records.into_iter().collect();
    jobs::set_aside(conn, search::CITATION_JOB, None, SET_ASIDE)?;
    jobs::set_aside(
        conn,
        screening::TA_JOB,
        Some(("record_id", &ids)),
        SET_ASIDE,
    )?;
    // The pre-screen job scores whatever is still unscored; once only
    // skipped records were left, it has nothing to do.
    if laya::unscored(conn)?.is_empty() {
        jobs::set_aside(conn, crate::agents::prescreen::JOB, None, SET_ASIDE)?;
    }
    skipped(conn)?.ok_or_else(|| Error::Invalid("The skip was not recorded.".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use serde_json::json;

    /// One database run (a, b) and one citation run (c, d, and a duplicate of a).
    /// a and b are screened; c and d wait for screening with jobs queued.
    fn setup() -> (tempfile::TempDir, Connection, String) {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let review = crate::reviews::insert_review(&conn, "Q?").unwrap();
        for (run, source) in [("s1", "openalex"), ("s2", "citation_chasing")] {
            conn.execute(
                "INSERT INTO search_run (id, review_id, source, query_string, query_ast, run_at, hits) VALUES (?1, ?2, ?3, 'q', '{}', '2026-09-25T10:00:00Z', 3)",
                params![run, review, source],
            )
            .unwrap();
        }
        for (rec, run) in [
            ("a", "s1"),
            ("b", "s1"),
            ("c", "s2"),
            ("d", "s2"),
            ("a2", "s2"),
        ] {
            conn.execute(
                "INSERT INTO record (id, search_run_id, title) VALUES (?1, ?2, ?1)",
                params![rec, run],
            )
            .unwrap();
        }
        let ev = |target: &str, stage: &str, actor: &str, decision: &str| {
            events::append(
                &conn,
                &NewEvent {
                    review_id: review.clone(),
                    target_type: "record".into(),
                    target_id: target.into(),
                    stage: stage.into(),
                    actor_type: Some(ActorType::Agent),
                    actor_id: actor.into(),
                    decision: decision.into(),
                    ..Default::default()
                },
            )
            .unwrap();
        };
        ev("a", "ta_screen", "resolver", "include");
        ev("b", "ta_screen", "resolver", "exclude");
        ev("a2", "dedup", "dedup", "duplicate");
        ev("d", "ta_screen", "arbiter", "uncertain");
        conn.execute(
            "INSERT INTO event (review_id, target_type, target_id, stage, actor_type, actor_id, decision, created_at)
             VALUES (?1, 'search_run', 's2', 'search', 'agent', 'search_agent', 'citation_chasing', 'now')",
            [&review],
        )
        .unwrap();
        for rec in ["c", "d"] {
            jobs::enqueue(
                &conn,
                &review,
                screening::TA_JOB,
                &json!({"record_id": rec}),
            )
            .unwrap();
        }
        jobs::enqueue(
            &conn,
            &review,
            screening::TA_JOB,
            &json!({"record_id": "b"}),
        )
        .unwrap();
        (dir, conn, review)
    }

    fn counts(conn: &Connection) -> (i64, i64, i64) {
        conn.query_row(
            "SELECT identified_databases, identified_other, records_screened FROM v_prisma_counts",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }

    #[test]
    fn skipping_takes_unscreened_citation_records_out_of_the_counts() {
        let (_dir, conn, review) = setup();
        assert_eq!(counts(&conn), (2, 3, 4));
        let before = state(&conn, true).unwrap();
        assert_eq!(
            (before.status, before.records, before.screened),
            ("screening", 2, 0)
        );
        assert!(before.can_skip);
        assert_eq!(
            screening::human_queue(&conn, "ta_screen").unwrap(),
            vec!["d"]
        );

        let s = skip(&conn, &review).unwrap();
        // A citation record the arbiter left uncertain no longer holds up the review.
        assert!(screening::human_queue(&conn, "ta_screen")
            .unwrap()
            .is_empty());
        assert_eq!(s.records, 2);
        assert_eq!(counts(&conn), (2, 0, 2));
        let screenable: Vec<String> = conn
            .prepare("SELECT id FROM v_screenable ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(screenable, vec!["a", "b"]);
        assert!(!crate::records::screenable(&conn, "c").unwrap());

        // Only the citation records' jobs are set aside.
        let left = jobs::outstanding(&conn, &[screening::TA_JOB]).unwrap();
        assert_eq!(left, 1);
        let after = state(&conn, true).unwrap();
        assert_eq!(after.status, "skipped");
        assert!(!after.can_skip);

        // A second skip changes nothing; the event is human and explained.
        skip(&conn, &review).unwrap();
        let (n, actor, why): (i64, String, String) = conn
            .query_row(
                "SELECT COUNT(*), MAX(actor_type), MAX(rationale) FROM event WHERE decision = 'skipped'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((n, actor.as_str()), (1, "human"));
        assert!(why.contains("2 records"));
    }

    #[test]
    fn skipping_is_refused_once_a_citation_record_reached_full_text() {
        let (_dir, conn, review) = setup();
        conn.execute(
            "INSERT INTO report (id, record_id, retrieval_status) VALUES ('pc', 'c', 'pending')",
            [],
        )
        .unwrap();
        assert!(!state(&conn, true).unwrap().can_skip);
        assert!(skip(&conn, &review).is_err());
        assert_eq!(counts(&conn), (2, 3, 4));
    }

    #[test]
    fn skipping_before_the_search_ran_stops_it() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let review = crate::reviews::insert_review(&conn, "Q?").unwrap();
        assert_eq!(state(&conn, true).unwrap().status, "pending");
        assert_eq!(state(&conn, false).unwrap().status, "off");
        jobs::enqueue(&conn, &review, search::CITATION_JOB, &json!({})).unwrap();
        let s = skip(&conn, &review).unwrap();
        assert_eq!(s.records, 0);
        assert_eq!(
            jobs::outstanding(&conn, &[search::CITATION_JOB]).unwrap(),
            0
        );
    }
}
