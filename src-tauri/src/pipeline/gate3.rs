//! Gate 3: sign-off (§9). The user resolves flagged extractions, confirms
//! report-to-study links and certainty judgements, and writes PRISMA items 3
//! (rationale), 25 (support) and 26 (competing interests). Sign-off is
//! possible only when all of that is done.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::agents::protocol::review_question;
use crate::error::{Error, Result};
use crate::events::{self, ActorType, NewEvent};
use crate::time::now_iso;

/// Author-written sections, keyed as stored in `author_text`.
pub const AUTHOR_ITEMS: [(&str, &str); 3] = [
    ("item3_rationale", "3. Rationale"),
    ("item25_support", "25. Support"),
    ("item26_competing", "26. Competing interests"),
];

pub fn outcome(conn: &Connection) -> Result<Option<String>> {
    super::gate_outcome(conn, "gate3")
}

pub fn author_text(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT text FROM author_text WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()?)
}

pub fn set_author_text(conn: &Connection, key: &str, text: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO author_text (key, text, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET text = excluded.text, updated_at = excluded.updated_at",
        params![key, text, now_iso()],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Requirements {
    pub flagged_values: i64,
    pub links_to_confirm: i64,
    pub links_confirmed: bool,
    pub certainty_to_confirm: i64,
    pub missing_author_items: Vec<String>,
    pub studies_needing_you: i64,
    pub ready: bool,
}

pub fn requirements(conn: &Connection) -> Result<Requirements> {
    let flagged_values = crate::agents::extraction::open_flags(conn)?;
    let links_to_confirm: i64 = conn.query_row(
        "SELECT COUNT(*) FROM event WHERE stage = 'link' AND decision = 'link'",
        [],
        |r| r.get(0),
    )?;
    let last_link: i64 = conn.query_row(
        "SELECT COALESCE(MAX(id), 0) FROM event WHERE stage = 'link'",
        [],
        |r| r.get(0),
    )?;
    let last_confirm: i64 =
        conn.query_row("SELECT COALESCE(MAX(id), 0) FROM event WHERE stage = 'gate3' AND decision = 'confirm_links'", [], |r| r.get(0))?;
    let links_confirmed = links_to_confirm == 0 || last_confirm > last_link;
    let certainty_to_confirm: i64 = conn.query_row(
        "SELECT COUNT(*) FROM synthesis_item WHERE kind IN ('cerqual','grade') AND json_extract(body, '$.confirmed') IS NOT 1",
        [],
        |r| r.get(0),
    )?;
    let mut missing = Vec::new();
    for (key, label) in AUTHOR_ITEMS {
        if author_text(conn, key)?
            .map(|t| t.trim().is_empty())
            .unwrap_or(true)
        {
            missing.push(label.to_owned());
        }
    }
    let studies_needing_you: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT study) FROM (
           SELECT target_id AS study FROM v_current_event
            WHERE stage IN ('extract','appraise') AND decision = 'needs_human' AND target_type = 'study'
           UNION
           -- Appraisal domains left without verified support or agreement.
           SELECT a.study_id FROM v_current_event e JOIN appraisal a ON a.id = e.target_id
            WHERE e.stage = 'appraise' AND e.target_type = 'appraisal' AND e.actor_id = 'needs_human')",
        [],
        |r| r.get(0),
    )?;
    let ready =
        flagged_values == 0 && links_confirmed && certainty_to_confirm == 0 && missing.is_empty();
    Ok(Requirements {
        flagged_values,
        links_to_confirm,
        links_confirmed,
        certainty_to_confirm,
        missing_author_items: missing,
        studies_needing_you,
        ready,
    })
}

fn human_event(conn: &Connection, decision: &str, rationale: &str) -> Result<()> {
    let (review_id, _) = review_question(conn)?;
    events::append(
        conn,
        &NewEvent {
            review_id: review_id.clone(),
            target_type: "protocol".into(),
            target_id: review_id,
            stage: "gate3".into(),
            actor_type: Some(ActorType::Human),
            actor_id: "human".into(),
            decision: decision.into(),
            rationale: Some(rationale.into()),
            ..Default::default()
        },
    )?;
    Ok(())
}

pub fn confirm_links(conn: &Connection) -> Result<()> {
    human_event(
        conn,
        "confirm_links",
        "Report-to-study links reviewed and confirmed.",
    )
}

pub fn sign_off(conn: &Connection) -> Result<()> {
    let r = requirements(conn)?;
    if !r.ready {
        return Err(Error::Invalid(
            "Sign-off needs every requirement met first.".into(),
        ));
    }
    human_event(conn, "pass", "Report signed off by the review author.")?;
    let v = super::verification(conn)?;
    if v.verified {
        conn.execute("UPDATE review SET verification = 'verified'", [])?;
    }
    Ok(())
}

pub fn skip(conn: &Connection) -> Result<()> {
    human_event(
        conn,
        "skip",
        "Gate 3 skipped: the report was not signed off.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirements_block_until_author_items_written() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        crate::reviews::insert_review(&conn, "q").unwrap();
        let r = requirements(&conn).unwrap();
        assert!(!r.ready);
        assert_eq!(r.missing_author_items.len(), 3);
        assert!(sign_off(&conn).is_err());
        for (k, _) in AUTHOR_ITEMS {
            set_author_text(&conn, k, "Written.").unwrap();
        }
        assert!(requirements(&conn).unwrap().ready);
        sign_off(&conn).unwrap();
        assert_eq!(outcome(&conn).unwrap().as_deref(), Some("pass"));
        // Gates 1 and 2 were never passed, so the review is still unverified.
        let v = crate::pipeline::verification(&conn).unwrap();
        assert!(!v.verified);
        assert_eq!(v.gates_passed, 1);
    }
}
