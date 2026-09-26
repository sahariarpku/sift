//! Gate 2: calibration (§9). The user screens a blind stratified sample of
//! 100 records (40 agent-included, 40 agent-excluded, 20 uncertain, random
//! within strata). Pass: estimated recall >= 95% and Cohen's kappa >= 0.6.

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use crate::agents::protocol::review_question;
use crate::agents::screening::human_queue;
use crate::error::{Error, Result};
use crate::events::{self, ActorType, NewEvent};
use crate::records;
use crate::stats::{self, Rng, Stratum};

pub const TARGET: [(&str, usize); 3] = [("include", 40), ("exclude", 40), ("uncertain", 20)];
pub const RECALL_THRESHOLD: f64 = 0.95;
pub const KAPPA_THRESHOLD: f64 = 0.6;
const BOOTSTRAP_REPS: usize = 2000;

/// Agent outcome per screenable record: "include", "exclude" (final agent decision) or "uncertain" (human queue).
pub fn agent_strata(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT s.id,
           (SELECT f.decision FROM v_final_decision f WHERE f.stage = 'ta_screen' AND f.target_id = s.id AND f.actor_id IN ('resolver','arbiter'))
         FROM v_screenable s ORDER BY s.rec_order",
    )?;
    let rows: Vec<(String, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let queue: std::collections::HashSet<String> =
        human_queue(conn, "ta_screen")?.into_iter().collect();
    Ok(rows
        .into_iter()
        .filter_map(|(id, d)| match d {
            Some(d) => Some((id, d)),
            None if queue.contains(&id) => Some((id, "uncertain".into())),
            None => None,
        })
        .collect())
}

/// Sampled record ids with their stratum, in presentation order.
pub fn sample(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT target_id, rationale FROM event WHERE stage = 'gate2' AND decision = 'sampled' ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get::<_, Option<String>>(1)?.unwrap_or_default(),
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn seed_for(review_id: &str) -> u64 {
    review_id.bytes().fold(1469598103934665603u64, |h, b| {
        (h ^ b as u64).wrapping_mul(1099511628211)
    })
}

/// Draws the sample once. Returns the number of records sampled.
pub fn draw_sample(conn: &Connection) -> Result<usize> {
    if !sample(conn)?.is_empty() {
        return Ok(sample(conn)?.len());
    }
    let (review_id, _) = review_question(conn)?;
    let seed = seed_for(&review_id);
    let mut rng = Rng::new(seed);
    let strata = agent_strata(conn)?;
    let mut picked: Vec<(String, String)> = Vec::new();
    for (name, n) in TARGET {
        let mut ids: Vec<String> = strata
            .iter()
            .filter(|(_, s)| s == name)
            .map(|(id, _)| id.clone())
            .collect();
        rng.shuffle(&mut ids);
        picked.extend(ids.into_iter().take(n).map(|id| (id, name.to_owned())));
    }
    // Present in random order so strata are not guessable from position.
    rng.shuffle(&mut picked);
    let tx = conn.unchecked_transaction()?;
    for (id, stratum) in &picked {
        events::append(
            &tx,
            &NewEvent {
                review_id: review_id.clone(),
                target_type: "record".into(),
                target_id: id.clone(),
                stage: "gate2".into(),
                actor_type: Some(ActorType::Agent),
                actor_id: "calibration".into(),
                decision: "sampled".into(),
                rationale: Some(stratum.clone()),
                blinded: true,
                ..Default::default()
            },
        )?;
    }
    events::append(
        &tx,
        &NewEvent {
            review_id: review_id.clone(),
            target_type: "protocol".into(),
            target_id: review_id,
            stage: "gate2".into(),
            actor_type: Some(ActorType::Agent),
            actor_id: "calibration".into(),
            decision: "sample_drawn".into(),
            rationale: Some(format!(
                "{} records sampled, 40/40/20 target, random within strata, seed {seed}.",
                picked.len()
            )),
            ..Default::default()
        },
    )?;
    tx.commit()?;
    Ok(picked.len())
}

/// The user's blind judgement on a sampled record (include, exclude or uncertain).
pub fn judge(
    conn: &Connection,
    record_id: &str,
    decision: &str,
    reason_code: Option<&str>,
) -> Result<()> {
    if !matches!(decision, "include" | "exclude" | "uncertain") {
        return Err(Error::Invalid("Choose include, exclude or unsure.".into()));
    }
    if !sample(conn)?.iter().any(|(id, _)| id == record_id) {
        return Err(Error::Invalid(
            "That record is not in the calibration sample.".into(),
        ));
    }
    let (review_id, _) = review_question(conn)?;
    let prior: Option<i64> = conn
        .query_row(
            "SELECT id FROM v_current_event WHERE stage = 'gate2' AND actor_id = 'human' AND target_id = ?1 AND decision IN ('include','exclude','uncertain') ORDER BY id DESC LIMIT 1",
            [record_id],
            |r| r.get(0),
        )
        .optional()?;
    events::append(
        conn,
        &NewEvent {
            review_id,
            target_type: "record".into(),
            target_id: record_id.into(),
            stage: "gate2".into(),
            actor_type: Some(ActorType::Human),
            actor_id: "human".into(),
            decision: decision.into(),
            reason_code: reason_code.map(str::to_owned).filter(|c| !c.is_empty()),
            blinded: true,
            supersedes: prior,
            ..Default::default()
        },
    )?;
    Ok(())
}

fn human_judgements(conn: &Connection) -> Result<std::collections::HashMap<String, String>> {
    let mut stmt = conn.prepare(
        "SELECT target_id, decision FROM v_current_event WHERE stage = 'gate2' AND actor_id = 'human' AND decision IN ('include','exclude','uncertain') ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    pub sampled: usize,
    pub judged: usize,
    pub recall: Option<f64>,
    pub recall_ci: Option<(f64, f64)>,
    pub kappa: Option<f64>,
    pub strata: Vec<Stratum>,
    pub passed: bool,
}

pub fn metrics(conn: &Connection) -> Result<Metrics> {
    let s = sample(conn)?;
    let human = human_judgements(conn)?;
    let population = agent_strata(conn)?;
    let judged: Vec<(&String, &String, &String)> = s
        .iter()
        .filter_map(|(id, st)| human.get(id).map(|h| (id, st, h)))
        .collect();
    let strata: Vec<Stratum> = TARGET
        .iter()
        .map(|(name, _)| {
            let in_stratum: Vec<&(&String, &String, &String)> =
                judged.iter().filter(|(_, st, _)| st == name).collect();
            Stratum {
                name: (*name).into(),
                population: population.iter().filter(|(_, st)| st == name).count() as u64,
                sampled: in_stratum.len() as u64,
                // "Unsure" counts as relevant: the person would not have excluded it.
                relevant: in_stratum
                    .iter()
                    .filter(|(_, _, h)| h.as_str() != "exclude")
                    .count() as u64,
                passed_by_agents: *name != "exclude",
            }
        })
        .collect();
    let a: Vec<&str> = judged.iter().map(|(_, st, _)| st.as_str()).collect();
    let b: Vec<&str> = judged.iter().map(|(_, _, h)| h.as_str()).collect();
    let kappa = stats::cohen_kappa(&a, &b);
    let recall = stats::stratified_recall(&strata);
    let (review_id, _) = review_question(conn)?;
    let recall_ci = stats::recall_bootstrap(&strata, BOOTSTRAP_REPS, seed_for(&review_id));
    let passed = judged.len() == s.len()
        && !s.is_empty()
        && recall.unwrap_or(1.0) >= RECALL_THRESHOLD
        && kappa.unwrap_or(0.0) >= KAPPA_THRESHOLD;
    Ok(Metrics {
        sampled: s.len(),
        judged: judged.len(),
        recall,
        recall_ci,
        kappa,
        strata,
        passed,
    })
}

/// Latest gate 2 outcome: "pass", "fail" or "skip".
pub fn outcome(conn: &Connection) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT decision FROM v_current_event WHERE stage = 'gate2' AND decision IN ('pass','fail','skip') ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?)
}

/// Scores the finished sample, records pass or fail, and applies the user's
/// judgements as final decisions where they differ from the agents'.
pub fn finish(conn: &Connection) -> Result<Metrics> {
    let m = metrics(conn)?;
    if m.judged < m.sampled {
        return Err(Error::Invalid(format!(
            "{} of {} records still to screen.",
            m.sampled - m.judged,
            m.sampled
        )));
    }
    let (review_id, _) = review_question(conn)?;
    let human = human_judgements(conn)?;
    let tx = conn.unchecked_transaction()?;
    for (id, _) in sample(&tx)? {
        let Some(h) = human.get(&id) else { continue };
        let target = if h == "exclude" { "exclude" } else { "include" };
        let current: Option<(i64, String)> = tx
            .query_row(
                "SELECT event_id, decision FROM v_final_decision WHERE stage = 'ta_screen' AND target_id = ?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if current.as_ref().is_some_and(|(_, d)| d == target) {
            continue;
        }
        let reason: Option<String> = tx
            .query_row(
                "SELECT reason_code FROM v_current_event WHERE stage = 'gate2' AND actor_id = 'human' AND target_id = ?1 ORDER BY id DESC LIMIT 1",
                [&id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        events::append(
            &tx,
            &NewEvent {
                review_id: review_id.clone(),
                target_type: "record".into(),
                target_id: id.clone(),
                stage: "ta_screen".into(),
                actor_type: Some(ActorType::Human),
                actor_id: "human".into(),
                decision: target.into(),
                reason_code: if target == "exclude" { reason } else { None },
                rationale: Some(format!(
                    "Calibration judgement ({h}) applied as the final decision."
                )),
                supersedes: current.map(|c| c.0),
                ..Default::default()
            },
        )?;
    }
    events::append(
        &tx,
        &NewEvent {
            review_id: review_id.clone(),
            target_type: "protocol".into(),
            target_id: review_id,
            stage: "gate2".into(),
            actor_type: Some(ActorType::Human),
            actor_id: "human".into(),
            decision: if m.passed { "pass" } else { "fail" }.into(),
            confidence: m.recall,
            rationale: Some(serde_json::to_string(&m)?),
            ..Default::default()
        },
    )?;
    tx.commit()?;
    Ok(m)
}

/// Skips Gate 2. The review stays unverified for good (§9).
pub fn skip(conn: &Connection) -> Result<()> {
    let (review_id, _) = review_question(conn)?;
    events::append(
        conn,
        &NewEvent {
            review_id: review_id.clone(),
            target_type: "protocol".into(),
            target_id: review_id,
            stage: "gate2".into(),
            actor_type: Some(ActorType::Human),
            actor_id: "human".into(),
            decision: "skip".into(),
            rationale: Some(
                "Gate 2 skipped: screening was not calibrated against a person.".into(),
            ),
            ..Default::default()
        },
    )?;
    Ok(())
}

/// After a failed Gate 2: the user screens every record without a human decision themselves.
pub fn hand_screening_to_user(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT INTO review_setting (key, value) VALUES ('manual_ta', 'true') ON CONFLICT(key) DO UPDATE SET value = 'true'",
        [],
    )?;
    Ok(())
}

pub fn manual_ta(conn: &Connection) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT value FROM review_setting WHERE key = 'manual_ta'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .is_some_and(|v| v == "true"))
}

/// Records the user still has to screen when they took over screening.
pub fn manual_queue(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT s.id FROM v_screenable s
         WHERE NOT EXISTS (SELECT 1 FROM v_final_decision f WHERE f.stage = 'ta_screen' AND f.target_id = s.id AND f.actor_id = 'human')
         ORDER BY s.rec_order",
    )?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// For the calibration screen: the record without any agent decision.
pub fn blind_record(conn: &Connection, record_id: &str) -> Result<records::RecordRow> {
    records::record(conn, record_id)?.ok_or_else(|| Error::Invalid("record not found".into()))
}
