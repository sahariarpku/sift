//! Bypass mode: the pipeline settles every point that would wait for a
//! person, with a fixed, disclosed rule for each, so a review runs from the
//! question to a finished report without stopping.
//!
//! Every gate bypass mode passes is recorded as a skip by an agent, so a
//! bypassed review stays unverified for good (§9). Every decision it makes
//! carries a rationale starting with [`PREFIX`], which the automation
//! disclosure lists. Real errors (a provider that keeps failing, a synthesis
//! that cannot run, no included studies) still stop the review.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection, OptionalExtension};

use super::{gate3, retrieval};
use crate::agents::{protocol, screening};
use crate::error::Result;
use crate::events::{self, ActorType, NewEvent};
use crate::jobs::{self, JobStatus};
use crate::time::now_iso;

/// Actor ID on the gate events bypass mode writes.
pub const ACTOR: &str = "bypass";
/// Every bypass rationale starts with this, so the report can list them.
pub const PREFIX: &str = "Bypass mode:";

const KEY: &str = "bypass";
const RETRIED: &str = "bypass_retried";
const RETRIED_QUEUES: &str = "bypass_retried_queues";

fn get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM review_setting WHERE key = ?1",
            [key],
            |r| r.get(0),
        )
        .optional()?)
}

fn put(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO review_setting (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn set_of(conn: &Connection, key: &str) -> Result<HashSet<String>> {
    Ok(get(conn, key)?
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or_default())
}

fn put_set(conn: &Connection, key: &str, set: &HashSet<String>) -> Result<()> {
    let mut v: Vec<&String> = set.iter().collect();
    v.sort();
    put(conn, key, &serde_json::to_string(&v)?)
}

pub fn on(conn: &Connection) -> Result<bool> {
    Ok(get(conn, KEY)?.is_some_and(|v| v == "true"))
}

/// Turns bypass mode on or off. The switch itself is a human event, so the
/// log shows who chose to run without the gates and when.
pub fn set(conn: &Connection, review_id: &str, enabled: bool) -> Result<()> {
    if on(conn)? == enabled {
        return Ok(());
    }
    put(conn, KEY, if enabled { "true" } else { "false" })?;
    events::append(
        conn,
        &NewEvent {
            review_id: review_id.into(),
            target_type: "protocol".into(),
            target_id: review_id.into(),
            stage: "bypass".into(),
            actor_type: Some(ActorType::Human),
            actor_id: "human".into(),
            decision: if enabled { "bypass_on" } else { "bypass_off" }.into(),
            rationale: Some(
                if enabled {
                    "Bypass mode turned on: the pipeline settles every point that would wait for a person, and the review stays unverified."
                } else {
                    "Bypass mode turned off."
                }
                .into(),
            ),
            ..Default::default()
        },
    )?;
    Ok(())
}

fn gate_skip(conn: &Connection, review_id: &str, stage: &str, why: &str) -> Result<()> {
    events::append(
        conn,
        &NewEvent {
            review_id: review_id.into(),
            target_type: "protocol".into(),
            target_id: review_id.into(),
            stage: stage.into(),
            actor_type: Some(ActorType::Agent),
            actor_id: ACTOR.into(),
            decision: "skip".into(),
            rationale: Some(format!("{PREFIX} {why}")),
            ..Default::default()
        },
    )?;
    Ok(())
}

fn resolver(
    conn: &Connection,
    review_id: &str,
    stage: &str,
    target_id: &str,
    decision: &str,
    reason_code: Option<String>,
    why: &str,
) -> Result<()> {
    let target_type = if stage == "ft_screen" { "report" } else { "record" };
    events::append(
        conn,
        &NewEvent {
            review_id: review_id.into(),
            target_type: target_type.into(),
            target_id: target_id.into(),
            stage: stage.into(),
            actor_type: Some(ActorType::Agent),
            // v_final_decision counts resolver decisions as final.
            actor_id: "resolver".into(),
            decision: decision.into(),
            reason_code,
            rationale: Some(format!("{PREFIX} {why}")),
            ..Default::default()
        },
    )?;
    Ok(())
}

/// Sends a screening queue back to the agents once. Returns true if it did.
fn retry_queue_once(conn: &Connection, review_id: &str, stage: &str) -> Result<bool> {
    let mut done = set_of(conn, RETRIED_QUEUES)?;
    if done.contains(stage) {
        return Ok(false);
    }
    done.insert(stage.to_owned());
    put_set(conn, RETRIED_QUEUES, &done)?;
    Ok(screening::retry_with_agents(conn, review_id, stage)? > 0)
}

/// Failed jobs: each gets one fresh round of attempts; after that, failures
/// the user could dismiss (a source, a PDF) are dismissed. Anything else is a
/// real error and stays in front of the user.
fn failed_jobs(conn: &Connection) -> Result<bool> {
    let mut retried = set_of(conn, RETRIED)?;
    let mut acted = false;
    for job in jobs::list(conn)?
        .into_iter()
        .filter(|j| j.status == JobStatus::Failed)
    {
        let id = job.id.to_string();
        if !retried.contains(&id) {
            conn.execute(
                "UPDATE job SET status = 'queued', attempts = 0, last_error = NULL, updated_at = ?1 WHERE id = ?2",
                params![now_iso(), job.id],
            )?;
            retried.insert(id);
            acted = true;
        } else if jobs::DISMISSIBLE.contains(&job.kind.as_str()) {
            conn.execute(
                "UPDATE job SET status = 'done', last_error = ?1 || COALESCE(last_error, ''), updated_at = ?2 WHERE id = ?3",
                params![format!("{PREFIX} dismissed after a retry: "), now_iso(), job.id],
            )?;
            acted = true;
        }
    }
    put_set(conn, RETRIED, &retried)?;
    Ok(acted)
}

/// Full text the arbiter left open: the agents' current votes decide. More
/// coded excludes than includes excludes on the most common code; anything
/// else includes, so a study is never dropped without a stated criterion.
fn settle_full_text(conn: &Connection, review_id: &str, report_id: &str) -> Result<()> {
    let codes: Vec<String> = protocol::require_locked(conn)?
        .body
        .codes()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let (mut include, mut exclude) = (0, 0);
    let mut by_code: HashMap<String, usize> = HashMap::new();
    for actor in ["screener_a", "screener_b", "arbiter"] {
        let Some(e) = screening::latest_by(conn, "ft_screen", report_id, actor)? else {
            continue;
        };
        match e.decision.as_str() {
            "include" => include += 1,
            "exclude" => {
                if let Some(c) = e.reason_code.filter(|c| codes.contains(c)) {
                    exclude += 1;
                    *by_code.entry(c).or_default() += 1;
                }
            }
            _ => {}
        }
    }
    if exclude > include {
        let mut ranked: Vec<(String, usize)> = by_code.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let code = ranked.into_iter().next().map(|(c, _)| c);
        resolver(
            conn,
            review_id,
            "ft_screen",
            report_id,
            "exclude",
            code,
            &format!("unresolved at full text; excluded because {exclude} agent votes excluded and {include} included."),
        )
    } else {
        resolver(
            conn,
            review_id,
            "ft_screen",
            report_id,
            "include",
            None,
            &format!("unresolved at full text; included because {include} agent votes included and {exclude} gave a coded exclusion."),
        )
    }
}

/// Settles one waiting point, matched on the message `step` returned.
/// Returns false when there is nothing bypass mode may decide.
pub fn resolve(conn: &Connection, review_id: &str, detail: &str) -> Result<bool> {
    if detail.contains("jobs failed") {
        return failed_jobs(conn);
    }
    if detail.starts_with("Gate 1:") {
        protocol::lock_by(
            conn,
            false,
            ActorType::Agent,
            ACTOR,
            &format!("{PREFIX} Gate 1 skipped; the drafted protocol was locked without review."),
        )?;
        return Ok(true);
    }
    if detail.starts_with("Gate 2") {
        gate_skip(
            conn,
            review_id,
            "gate2",
            "Gate 2 skipped; screening was not calibrated against a person.",
        )?;
        return Ok(true);
    }
    if detail.starts_with("Screen the remaining") {
        // The user had taken screening over; hand it back to the agents.
        put(conn, "manual_ta", "false")?;
        return Ok(true);
    }
    if detail.ends_with("records need your screening decision") {
        if retry_queue_once(conn, review_id, "ta_screen")? {
            return Ok(true);
        }
        let queue = screening::human_queue(conn, "ta_screen")?;
        for id in &queue {
            resolver(
                conn,
                review_id,
                "ta_screen",
                id,
                "include",
                None,
                "unresolved at title/abstract; carried forward to full-text screening.",
            )?;
        }
        return Ok(!queue.is_empty());
    }
    if detail.ends_with("full texts to upload or mark not retrieved") {
        let queue = retrieval::upload_queue(conn)?;
        retrieval::mark_not_retrieved_by(
            conn,
            &queue,
            ActorType::Agent,
            &format!("{PREFIX} no full text found by any automatic route; marked not retrieved instead of waiting for an upload."),
        )?;
        return Ok(!queue.is_empty());
    }
    if detail.ends_with("full texts need your screening decision") {
        if retry_queue_once(conn, review_id, "ft_screen")? {
            return Ok(true);
        }
        let queue = screening::human_queue(conn, "ft_screen")?;
        for id in &queue {
            settle_full_text(conn, review_id, id)?;
        }
        return Ok(!queue.is_empty());
    }
    if detail.starts_with("Gate 3:") {
        let r = gate3::requirements(conn)?;
        let mut why = "Gate 3 skipped; the report was not signed off by a person.".to_owned();
        if r.flagged_values > 0 {
            why.push_str(&format!(
                " {} flagged extraction values stay unresolved and are excluded from synthesis.",
                r.flagged_values
            ));
        }
        gate_skip(conn, review_id, "gate3", &why)?;
        return Ok(true);
    }
    Ok(false)
}

/// Bypass decisions, newest first, for the disclosure.
pub fn decisions(conn: &Connection) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT stage, decision, rationale FROM event WHERE rationale LIKE 'Bypass mode:%' ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
