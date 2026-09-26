//! Append-only event log (principle 1). This module only inserts and reads;
//! the review schema's triggers reject any UPDATE or DELETE on `event`.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::time::now_iso;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActorType {
    Agent,
    Human,
}

impl ActorType {
    fn as_str(self) -> &'static str {
        match self {
            ActorType::Agent => "agent",
            ActorType::Human => "human",
        }
    }
}

/// A new event. Optional fields default to `None`; agent events must fill
/// provider, model and prompt version (principle 4), which later phases enforce
/// through their typed constructors.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewEvent {
    pub review_id: String,
    pub target_type: String,
    pub target_id: String,
    pub stage: String,
    pub actor_type: Option<ActorType>,
    pub actor_id: String,
    pub provider: Option<String>,
    pub model_id: Option<String>,
    pub prompt_version: Option<String>,
    pub decision: String,
    pub reason_code: Option<String>,
    pub rationale: Option<String>,
    pub evidence_quote: Option<String>,
    pub confidence: Option<f64>,
    pub blinded: bool,
    pub supersedes: Option<i64>,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub id: i64,
    pub review_id: String,
    pub target_type: String,
    pub target_id: String,
    pub stage: String,
    pub actor_type: String,
    pub actor_id: String,
    pub provider: Option<String>,
    pub model_id: Option<String>,
    pub prompt_version: Option<String>,
    pub decision: String,
    pub reason_code: Option<String>,
    pub rationale: Option<String>,
    pub evidence_quote: Option<String>,
    pub confidence: Option<f64>,
    pub blinded: bool,
    pub supersedes: Option<i64>,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub cost_usd: Option<f64>,
    pub created_at: String,
}

impl Event {
    pub fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
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
    }
}

/// Appends one event and returns its id.
pub fn append(conn: &Connection, e: &NewEvent) -> Result<i64> {
    let actor_type = e.actor_type.unwrap_or(ActorType::Agent).as_str();
    conn.execute(
        "INSERT INTO event (review_id, target_type, target_id, stage, actor_type, actor_id,
            provider, model_id, prompt_version, decision, reason_code, rationale, evidence_quote,
            confidence, blinded, supersedes, tokens_in, tokens_out, cost_usd, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
        params![
            e.review_id,
            e.target_type,
            e.target_id,
            e.stage,
            actor_type,
            e.actor_id,
            e.provider,
            e.model_id,
            e.prompt_version,
            e.decision,
            e.reason_code,
            e.rationale,
            e.evidence_quote,
            e.confidence,
            e.blinded,
            e.supersedes,
            e.tokens_in,
            e.tokens_out,
            e.cost_usd,
            now_iso(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Current state of a target at a stage: its latest event that no other event supersedes.
pub fn current(
    conn: &Connection,
    target_type: &str,
    target_id: &str,
    stage: &str,
) -> Result<Option<Event>> {
    Ok(conn
        .query_row(
            "SELECT * FROM event e
             WHERE e.target_type = ?1 AND e.target_id = ?2 AND e.stage = ?3
               AND NOT EXISTS (SELECT 1 FROM event s WHERE s.supersedes = e.id)
             ORDER BY e.id DESC LIMIT 1",
            params![target_type, target_id, stage],
            Event::from_row,
        )
        .optional()?)
}

/// Estimated spend for the review: every event's cost, superseded or not,
/// because the money was spent either way.
pub fn total_cost(conn: &Connection) -> Result<f64> {
    Ok(
        conn.query_row("SELECT COALESCE(SUM(cost_usd), 0.0) FROM event", [], |r| {
            r.get(0)
        })?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, reviews};

    fn setup() -> (tempfile::TempDir, Connection, String) {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open_review(&dir.path().join("r.sift")).unwrap();
        let id = reviews::insert_review(&conn, "Does X improve Y?").unwrap();
        (dir, conn, id)
    }

    fn screen(review_id: &str, decision: &str, supersedes: Option<i64>) -> NewEvent {
        NewEvent {
            review_id: review_id.into(),
            target_type: "record".into(),
            target_id: "rec-1".into(),
            stage: "ta_screen".into(),
            actor_type: Some(ActorType::Human),
            actor_id: "human".into(),
            decision: decision.into(),
            supersedes,
            ..Default::default()
        }
    }

    #[test]
    fn events_cannot_be_updated_or_deleted() {
        let (_d, conn, rid) = setup();
        let id = append(&conn, &screen(&rid, "include", None)).unwrap();
        let upd = conn.execute("UPDATE event SET decision = 'exclude' WHERE id = ?1", [id]);
        assert!(upd.unwrap_err().to_string().contains("append-only"));
        let del = conn.execute("DELETE FROM event WHERE id = ?1", [id]);
        assert!(del.unwrap_err().to_string().contains("append-only"));
    }

    #[test]
    fn correction_supersedes_earlier_event() {
        let (_d, conn, rid) = setup();
        let first = append(&conn, &screen(&rid, "include", None)).unwrap();
        assert_eq!(
            current(&conn, "record", "rec-1", "ta_screen")
                .unwrap()
                .unwrap()
                .decision,
            "include"
        );
        append(&conn, &screen(&rid, "exclude", Some(first))).unwrap();
        let now = current(&conn, "record", "rec-1", "ta_screen")
            .unwrap()
            .unwrap();
        assert_eq!(now.decision, "exclude");
        assert_eq!(now.supersedes, Some(first));
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 2, "the original event is kept");
    }

    #[test]
    fn total_cost_sums_every_event() {
        let (_d, conn, rid) = setup();
        assert_eq!(total_cost(&conn).unwrap(), 0.0);
        let first = append(
            &conn,
            &NewEvent {
                cost_usd: Some(0.25),
                ..screen(&rid, "include", None)
            },
        )
        .unwrap();
        append(
            &conn,
            &NewEvent {
                cost_usd: Some(0.5),
                ..screen(&rid, "exclude", Some(first))
            },
        )
        .unwrap();
        assert!((total_cost(&conn).unwrap() - 0.75).abs() < 1e-12);
    }

    #[test]
    fn actor_type_is_constrained() {
        let (_d, conn, rid) = setup();
        let bad = conn.execute(
            "INSERT INTO event (review_id, target_type, target_id, stage, actor_type, actor_id, decision, created_at)
             VALUES (?1, 'record', 'r', 'ta_screen', 'robot', 'x', 'include', 'now')",
            [rid],
        );
        assert!(bad.is_err());
    }
}
