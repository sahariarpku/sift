//! Quality checks for the Overview (§9) and the automation disclosure (§8.11).

use rusqlite::Connection;
use serde::Serialize;

use crate::error::Result;
use crate::pipeline::{gate2, verification, Verification};
use crate::stats;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Quality {
    pub gate2: Option<gate2::Metrics>,
    pub gate2_outcome: Option<String>,
    /// Cohen's kappa between Screener A and B at title/abstract stage.
    pub kappa_ab: Option<f64>,
    pub kappa_pairs: usize,
    /// Share of extracted values whose quote was found (before any human fix).
    pub quote_rate: Option<f64>,
    pub quotes_checked: i64,
    pub seed_found: Option<(i64, i64)>,
    /// Share of re-screened records whose decision changed.
    pub drift: Option<f64>,
    pub drift_n: i64,
}

fn pairs(conn: &Connection, stage: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT a.decision, b.decision FROM v_current_event a JOIN v_current_event b
           ON a.target_id = b.target_id AND a.stage = b.stage
         WHERE a.stage = ?1 AND a.actor_id = 'screener_a' AND b.actor_id = 'screener_b'
           AND a.decision IN ('include','exclude','uncertain') AND b.decision IN ('include','exclude','uncertain')",
    )?;
    let rows = stmt.query_map([stage], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn quality(conn: &Connection) -> Result<Quality> {
    let outcome = gate2::outcome(conn)?;
    let g2: Option<gate2::Metrics> = if outcome.is_some() || !gate2::sample(conn)?.is_empty() {
        Some(gate2::metrics(conn)?)
    } else {
        None
    };
    // After Gate 2 the stored metrics are the record of the decision.
    let stored: Option<gate2::Metrics> = conn
        .query_row(
            "SELECT rationale FROM event WHERE stage = 'gate2' AND decision IN ('pass','fail') ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            Some(gate2::Metrics {
                sampled: v["sampled"].as_u64()? as usize,
                judged: v["judged"].as_u64()? as usize,
                recall: v["recall"].as_f64(),
                recall_ci: v["recallCi"].as_array().and_then(|a| Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))),
                kappa: v["kappa"].as_f64(),
                strata: Vec::new(),
                passed: v["passed"].as_bool()?,
            })
        });
    let p = pairs(conn, "ta_screen")?;
    let a: Vec<&str> = p.iter().map(|x| x.0.as_str()).collect();
    let b: Vec<&str> = p.iter().map(|x| x.1.as_str()).collect();
    let (verified, flagged): (i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(decision = 'verified'), 0), COALESCE(SUM(decision = 'quote_not_found'), 0)
         FROM event WHERE stage = 'extract' AND actor_type = 'agent' AND target_type = 'extraction_value'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let checked = verified + flagged;
    let seed: Option<(i64, i64)> = conn
        .query_row(
            "SELECT rationale FROM event WHERE stage = 'search' AND decision LIKE 'seed_check_%' ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
        .and_then(|s| {
            // "Iteration N: x of y seed studies found."
            let after = s.split_once(": ")?.1;
            let mut it = after.split_whitespace();
            let x: i64 = it.next()?.parse().ok()?;
            it.next()?;
            let y: i64 = it.next()?.parse().ok()?;
            Some((x, y))
        });
    let drift: Option<(f64, i64)> = conn
        .query_row(
            "SELECT confidence, CAST(reason_code AS INTEGER) FROM event WHERE stage = 'ta_screen' AND decision = 'drift_result' ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    Ok(Quality {
        gate2: stored.or(g2),
        gate2_outcome: outcome,
        kappa_ab: stats::cohen_kappa(&a, &b),
        kappa_pairs: p.len(),
        quote_rate: (checked > 0).then(|| verified as f64 / checked as f64),
        quotes_checked: checked,
        seed_found: seed,
        drift: drift.map(|d| d.0),
        drift_n: drift.map(|d| d.1).unwrap_or(0),
    })
}

// ---------------------------------------------------------------------------
// Automation disclosure

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelUse {
    pub task: String,
    pub provider: String,
    pub model_requested: String,
    pub model_reported: Option<String>,
    pub prompt_version: String,
    pub calls: i64,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cost_usd: Option<f64>,
    pub prompt_json_fallback: i64,
    pub temperature_not_applied: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Disclosure {
    pub models: Vec<ModelUse>,
    pub search_simplifications: Vec<(String, Vec<String>)>,
    pub verification: Verification,
    pub quality: Quality,
    pub human_decisions: i64,
    pub agent_decisions: i64,
    pub human_overrides: i64,
    pub needs_human_items: i64,
    pub meta_analysis_note: Option<String>,
    /// The optional local pre-screen, when a review used it.
    pub prescreen: Option<PrescreenUse>,
    /// Title/abstract records carried to full text after an uncertain arbiter.
    pub carried_forward: i64,
    /// Stages the review author stopped early, with what was kept.
    pub skips: Vec<crate::pipeline::skip::Skip>,
    /// Points bypass mode settled, grouped by stage and decision.
    pub bypass: Vec<BypassRow>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BypassRow {
    pub stage: String,
    pub decision: String,
    pub count: i64,
    /// The rule applied, from the first such decision.
    pub rule: String,
}

fn bypass_rows(conn: &Connection) -> Result<Vec<BypassRow>> {
    let mut rows: Vec<BypassRow> = Vec::new();
    for (stage, decision, rationale) in crate::pipeline::bypass::decisions(conn)? {
        match rows
            .iter_mut()
            .find(|r| r.stage == stage && r.decision == decision)
        {
            Some(r) => r.count += 1,
            None => rows.push(BypassRow {
                stage,
                decision,
                count: 1,
                rule: rationale
                    .trim_start_matches(crate::pipeline::bypass::PREFIX)
                    .trim()
                    .to_owned(),
            }),
        }
    }
    Ok(rows)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PrescreenUse {
    pub model_id: String,
    pub prompt_version: String,
    pub mode: String,
    pub scored: i64,
    pub single_screened: i64,
    pub escalated: i64,
    /// One sentence for the methods and the disclosure.
    pub note: String,
}

fn prescreen_use(conn: &Connection) -> Result<Option<PrescreenUse>> {
    use crate::local::laya::{Mode, STAGE};
    let n = |decision: &str| -> Result<i64> {
        Ok(conn.query_row(
            "SELECT COUNT(DISTINCT target_id) FROM event WHERE stage = ?1 AND decision = ?2",
            [STAGE, decision],
            |r| r.get(0),
        )?)
    };
    let scored = n("score")?;
    if scored == 0 {
        return Ok(None);
    }
    let model_id: String = conn.query_row(
        "SELECT COALESCE(model_id, '') FROM event WHERE stage = ?1 AND decision = 'score' ORDER BY id DESC LIMIT 1",
        [STAGE],
        |r| r.get(0),
    )?;
    let opts = crate::agents::protocol::options(conn)?.prescreen;
    let (single, escalated) = (n("single_screen")?, n("escalate")?);
    let mode = match opts.mode {
        Mode::Off => "off (was on earlier in this review)",
        Mode::SafetyNet => "safety net",
        Mode::Speed => "speed",
    };
    let pct = |f: f64| (f * 100.0).round();
    let mut note = format!(
        "A local typed-decision model (Laya-MLX, {model_id}, prompt laya_prescreen.v1) estimated for {scored} records the probability that each inclusion criterion was met; the geometric mean ranked records for screening order. It made no screening decisions. "
    );
    if opts.mode == Mode::Speed || single > 0 {
        note.push_str(&format!(
            "In speed mode, records ranked in the lowest {}% were screened by Screener A alone when A excluded them ({single} records). ",
            pct(opts.speed_fraction)
        ));
    }
    note.push_str(&format!(
        "Records ranked in the top {}% that both screeners excluded were sent to the arbiter ({escalated} records).",
        pct(opts.net_fraction)
    ));
    Ok(Some(PrescreenUse {
        model_id,
        prompt_version: crate::local::laya::PROMPT_VERSION.into(),
        mode: mode.into(),
        scored,
        single_screened: single,
        escalated,
        note,
    }))
}

pub fn disclosure(conn: &Connection) -> Result<Disclosure> {
    let mut stmt = conn.prepare(
        "SELECT task, provider, model_requested, COALESCE(routed_via, model_reported), prompt_version, COUNT(*),
                COALESCE(SUM(tokens_in), 0), COALESCE(SUM(tokens_out), 0), SUM(cost_usd),
                COALESCE(SUM(structured_output = 0), 0), COALESCE(SUM(temperature_applied IS NULL), 0)
         FROM llm_call GROUP BY 1, 2, 3, 4, 5 ORDER BY 1, 5, 4",
    )?;
    let models = stmt
        .query_map([], |r| {
            Ok(ModelUse {
                task: r.get(0)?,
                provider: r.get(1)?,
                model_requested: r.get(2)?,
                model_reported: r.get(3)?,
                prompt_version: r.get(4)?,
                calls: r.get(5)?,
                tokens_in: r.get(6)?,
                tokens_out: r.get(7)?,
                cost_usd: r.get(8)?,
                prompt_json_fallback: r.get(9)?,
                temperature_not_applied: r.get(10)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let simplifications = crate::records::runs(conn)?
        .into_iter()
        .filter(|r| r.current && !r.simplifications.is_empty())
        .map(|r| (r.source, r.simplifications))
        .collect();
    let count = |sql: &str| -> Result<i64> { Ok(conn.query_row(sql, [], |r| r.get(0))?) };
    let meta: Option<String> = conn
        .query_row(
            "SELECT label FROM synthesis_item WHERE kind = 'swim_group' AND json_extract(body, '$.meta_analysis_eligible') = 1 LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .map(|l| format!("At least one outcome group ({l}) had 3 or more studies; a meta-analysis was not run because the statistics engine is not available in this version. Results are synthesised narratively (SWiM)."));
    Ok(Disclosure {
        models,
        search_simplifications: simplifications,
        verification: verification(conn)?,
        quality: quality(conn)?,
        human_decisions: count("SELECT COUNT(*) FROM event WHERE actor_type = 'human'")?,
        agent_decisions: count("SELECT COUNT(*) FROM event WHERE actor_type = 'agent'")?,
        human_overrides: count("SELECT COUNT(*) FROM event WHERE actor_type = 'human' AND supersedes IS NOT NULL AND stage IN ('ta_screen','ft_screen')")?,
        needs_human_items: count("SELECT COUNT(DISTINCT target_id) FROM event WHERE decision = 'needs_human'")?,
        meta_analysis_note: meta,
        prescreen: prescreen_use(conn)?,
        carried_forward: conn.query_row(
            "SELECT COUNT(*) FROM event WHERE stage = 'ta_screen' AND actor_id = 'resolver' AND rationale = ?1",
            [crate::agents::screening::CARRIED_FORWARD],
            |r| r.get(0),
        )?,
        skips: crate::pipeline::skip::list(conn)?,
        bypass: bypass_rows(conn)?,
    })
}
