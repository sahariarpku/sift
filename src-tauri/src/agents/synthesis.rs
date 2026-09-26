//! Synthesis (§8.10).
//!
//! Qualitative findings: thematic synthesis (Thomas and Harden, 2008) in three
//! steps: codes per study, descriptive themes, analytical themes. Every item
//! links back to verified extraction values, so every theme traces to quotes.
//! GRADE-CERQual is drafted per analytical theme for the user to confirm.
//!
//! Quantitative results: SWiM grouping by outcome construct and vote counting
//! by direction of effect (never by significance); counts are done in code.
//! Random-effects meta-analysis (WebR + metafor) is not built yet: groups that
//! would qualify are flagged, and the report says a meta-analysis was not run.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use rusqlite::{params, Connection};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::protocol::{require_locked, review_question};
use super::{call, par, AgentError, CallMeta};
use crate::error::{Error, Result};
use crate::events::{self, ActorType, NewEvent};
use crate::jobs::{Job, JobContext, JobError, JobHandler};
use crate::llm::store::Task;
use crate::pipeline::live::Tracked;
use crate::reviews::new_id;
use crate::time::now_iso;

pub const SYNTH_JOB: &str = "synthesis";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodesOut {
    pub codes: Vec<CodeOut>,
}
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeOut {
    pub code: String,
    pub finding_ids: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ThemesOut {
    pub themes: Vec<ThemeOut>,
}
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ThemeOut {
    pub label: String,
    pub description: String,
    pub code_ids: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AnalyticalOut {
    pub themes: Vec<AnalyticalTheme>,
}
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AnalyticalTheme {
    pub label: String,
    pub statement: String,
    pub descriptive_theme_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Concern {
    #[serde(rename = "no or very minor concerns")]
    None,
    #[serde(rename = "minor concerns")]
    Minor,
    #[serde(rename = "moderate concerns")]
    Moderate,
    #[serde(rename = "serious concerns")]
    Serious,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Moderate,
    Low,
    #[serde(rename = "very low")]
    VeryLow,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CerqualOut {
    pub methodological_limitations: Concern,
    pub methodological_limitations_note: String,
    pub coherence: Concern,
    pub coherence_note: String,
    pub adequacy: Concern,
    pub adequacy_note: String,
    pub relevance: Concern,
    pub relevance_note: String,
    pub overall: Confidence,
    pub explanation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Direction {
    #[serde(rename = "favours intervention")]
    FavoursIntervention,
    #[serde(rename = "favours comparator")]
    FavoursComparator,
    #[serde(rename = "no clear difference")]
    NoClearDifference,
    #[serde(rename = "unclear")]
    Unclear,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SwimOut {
    pub groups: Vec<SwimGroup>,
}
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SwimGroup {
    pub construct: String,
    pub results: Vec<SwimResult>,
}
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SwimResult {
    pub outcome_id: String,
    pub direction: Direction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Seriousness {
    #[serde(rename = "not serious")]
    NotSerious,
    Serious,
    #[serde(rename = "very serious")]
    VerySerious,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GradeOut {
    pub risk_of_bias: Seriousness,
    pub inconsistency: Seriousness,
    pub indirectness: Seriousness,
    pub imprecision: Seriousness,
    pub publication_bias: Seriousness,
    pub overall: Confidence,
    pub explanation: String,
}

// ---------------------------------------------------------------------------
// Storage

fn add_item(
    conn: &Connection,
    review_id: &str,
    kind: &str,
    parent: Option<&str>,
    label: &str,
    body: &serde_json::Value,
) -> Result<String> {
    let id = new_id();
    conn.execute(
        "INSERT INTO synthesis_item (id, review_id, kind, parent_id, label, body, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, review_id, kind, parent, label, body.to_string(), now_iso()],
    )?;
    Ok(id)
}

fn link(conn: &Connection, item: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO synthesis_link (item_id, extraction_value_id) VALUES (?1, ?2)",
        params![item, value],
    )?;
    Ok(())
}

fn synth_event(
    conn: &Connection,
    meta: Option<&CallMeta>,
    review_id: &str,
    item_id: &str,
    decision: &str,
    rationale: String,
) -> Result<()> {
    let mut e = match meta {
        Some(m) => m.event(
            review_id,
            "synthesis",
            item_id,
            "synth",
            "synthesis_agent",
            decision,
        ),
        None => NewEvent {
            review_id: review_id.into(),
            target_type: "synthesis".into(),
            target_id: item_id.into(),
            stage: "synth".into(),
            actor_type: Some(ActorType::Agent),
            actor_id: "synthesis_agent".into(),
            decision: decision.into(),
            ..Default::default()
        },
    };
    e.rationale = Some(rationale);
    events::append(conn, &e)?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    pub kind: String,
    pub parent_id: Option<String>,
    pub label: String,
    pub body: serde_json::Value,
    pub value_ids: Vec<String>,
}

pub fn items(conn: &Connection) -> Result<Vec<Item>> {
    let mut stmt =
        conn.prepare("SELECT id, kind, parent_id, label, body FROM synthesis_item ORDER BY rowid")?;
    let mut rows = stmt
        .query_map([], |r| {
            let body: String = r.get(4)?;
            Ok(Item {
                id: r.get(0)?,
                kind: r.get(1)?,
                parent_id: r.get(2)?,
                label: r.get(3)?,
                body: serde_json::from_str(&body).unwrap_or(serde_json::Value::Null),
                value_ids: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut links: HashMap<String, Vec<String>> = HashMap::new();
    let mut s = conn.prepare("SELECT item_id, extraction_value_id FROM synthesis_link")?;
    for row in s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (i, v) = row?;
        links.entry(i).or_default().push(v);
    }
    for it in &mut rows {
        it.value_ids = links.remove(&it.id).unwrap_or_default();
    }
    Ok(rows)
}

/// Extraction values supporting an item, following theme -> code links.
pub fn supporting_values(all: &[Item], item_id: &str) -> Vec<String> {
    let Some(item) = all.iter().find(|i| i.id == item_id) else {
        return Vec::new();
    };
    let mut out = item.value_ids.clone();
    let children: Vec<String> = match item.kind.as_str() {
        "analytical_theme" => item.body["descriptive_theme_ids"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        "descriptive_theme" => item.body["code_ids"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    for c in children {
        out.extend(supporting_values(all, &c));
    }
    out.sort();
    out.dedup();
    out
}

// ---------------------------------------------------------------------------
// Qualitative path

struct Finding {
    value_id: String,
    study_id: String,
    study_label: String,
    finding: String,
    quote: String,
    page: Option<i64>,
}

fn findings(conn: &Connection) -> Result<Vec<Finding>> {
    let mut stmt = conn.prepare(
        "SELECT ev.id, ev.study_id, s.label, ev.value, ev.source_quote, ev.page FROM extraction_value ev
         JOIN study s ON s.id = ev.study_id
         WHERE ev.field LIKE 'finding_%' AND ev.validation = 'verified' ORDER BY s.label, ev.field",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let v: String = r.get(3)?;
            let finding = serde_json::from_str::<serde_json::Value>(&v)
                .ok()
                .and_then(|j| j["finding"].as_str().map(str::to_owned))
                .unwrap_or_default();
            Ok(Finding {
                value_id: r.get(0)?,
                study_id: r.get(1)?,
                study_label: r.get(2)?,
                finding,
                quote: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                page: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

async fn qualitative(
    ctx: &JobContext,
    live: &Tracked,
    review_id: &str,
    question: &str,
) -> std::result::Result<usize, JobError> {
    let err = |e: Error| JobError::Fatal(e.to_string());
    let all = { findings(&ctx.open().map_err(err)?).map_err(err)? };
    if all.is_empty() {
        return Ok(0);
    }
    // 1. Codes per study.
    let mut by_study: Vec<(String, String, Vec<&Finding>)> = Vec::new();
    for f in &all {
        match by_study.iter_mut().find(|s| s.0 == f.study_id) {
            Some(s) => s.2.push(f),
            None => by_study.push((f.study_id.clone(), f.study_label.clone(), vec![f])),
        }
    }
    // Studies are coded independently, so the calls run side by side and are
    // stored in study order afterwards. A study whose codes cannot be produced
    // is recorded and left out rather than stopping the whole synthesis.
    let total = by_study.len();
    let done = AtomicUsize::new(0);
    live.step(format!("Coding findings from {total} studies"));
    let replies = par::bounded(by_study.iter().collect(), par::LIMIT, |(_, label, fs)| {
        let done = &done;
        async move {
            let ids: Vec<String> = (1..=fs.len()).map(|i| format!("F{i}")).collect();
            let listing = fs
                .iter()
                .zip(&ids)
                .map(|(f, id)| {
                    format!(
                        "{id}: {} | Quote (p. {}): \"{}\"",
                        f.finding,
                        f.page.map(|p| p.to_string()).unwrap_or_else(|| "?".into()),
                        f.quote
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let vars = [
                ("question", question.to_owned()),
                ("study", label.clone()),
                ("findings", listing),
            ];
            let reply = call::<CodesOut, _>(
                ctx,
                Task::Synthesis,
                "synthesis_codes.v2",
                &vars,
                3000,
                |o| {
                    for c in &o.codes {
                        if c.finding_ids.is_empty()
                            || c.finding_ids.iter().any(|i| !ids.contains(i))
                        {
                            return Err(format!(
                                "code \"{}\" must cite finding ids from: {}",
                                c.code,
                                ids.join(", ")
                            ));
                        }
                    }
                    Ok(())
                },
            )
            .await;
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            live.step(format!("Coding findings: {n} of {total} studies coded ({label} done)"));
            reply
        }
    })
    .await;
    let mut code_rows: Vec<(String, String, String)> = Vec::new(); // (item id, code text, study label)
    for ((study_id, label, fs), reply) in by_study.iter().zip(replies) {
        let ids: Vec<String> = (1..=fs.len()).map(|i| format!("F{i}")).collect();
        let reply = match reply {
            Ok(r) => r,
            Err(AgentError::NeedsHuman(m)) => {
                let conn = ctx.open().map_err(err)?;
                synth_event(
                    &conn,
                    None,
                    review_id,
                    study_id,
                    "not_coded",
                    format!("{label}: findings could not be coded ({m}). Left out of the thematic synthesis; check at sign-off."),
                )
                .map_err(err)?;
                continue;
            }
            Err(other) => return Err(other.into()),
        };
        let conn = ctx.open().map_err(err)?;
        for c in &reply.value.codes {
            let id = add_item(
                &conn,
                review_id,
                "code",
                None,
                &c.code,
                &json!({"study_id": study_id, "study": label}),
            )
            .map_err(err)?;
            for fid in &c.finding_ids {
                if let Some(pos) = ids.iter().position(|x| x == fid) {
                    link(&conn, &id, &fs[pos].value_id).map_err(err)?;
                }
            }
            synth_event(
                &conn,
                Some(&reply.meta),
                review_id,
                &id,
                "code",
                format!("{label}: {}", c.code),
            )
            .map_err(err)?;
            code_rows.push((id, c.code.clone(), label.clone()));
        }
    }

    if code_rows.is_empty() {
        return Ok(0);
    }

    // 2. Descriptive themes.
    live.step(format!(
        "Grouping {} codes from {} studies into descriptive themes",
        code_rows.len(),
        by_study.len()
    ));
    let cids: Vec<String> = (1..=code_rows.len()).map(|i| format!("C{i}")).collect();
    let listing = code_rows
        .iter()
        .zip(&cids)
        .map(|((_, code, study), id)| format!("{id}: {code} ({study})"))
        .collect::<Vec<_>>()
        .join("\n");
    let reply = call::<ThemesOut, _>(
        ctx,
        Task::Synthesis,
        "synthesis_themes.v2",
        &[("question", question.to_owned()), ("codes", listing)],
        6000,
        |o| {
            for t in &o.themes {
                if t.code_ids.iter().any(|i| !cids.contains(i)) {
                    return Err(format!("theme \"{}\" cites unknown code ids", t.label));
                }
            }
            Ok(())
        },
    )
    .await
    .map_err(|e| match e {
        AgentError::NeedsHuman(m) => JobError::Pause(format!("Descriptive themes failed: {m}")),
        other => other.into(),
    })?;
    let desc = {
        let conn = ctx.open().map_err(err)?;
        store_descriptive(
            &conn,
            review_id,
            &reply.value,
            &reply.meta,
            &cids,
            &code_rows,
        )
        .map_err(err)?
    };

    // 3. Analytical themes.
    live.step(format!(
        "Building analytical themes from {} descriptive themes",
        desc.len()
    ));
    let dids: Vec<String> = (1..=desc.len()).map(|i| format!("D{i}")).collect();
    let listing = desc
        .iter()
        .zip(&dids)
        .map(|((_, l, d), id)| format!("{id}: {l}: {d}"))
        .collect::<Vec<_>>()
        .join("\n");
    let reply = call::<AnalyticalOut, _>(
        ctx,
        Task::Synthesis,
        "synthesis_analytical.v2",
        &[("question", question.to_owned()), ("themes", listing)],
        4000,
        |o| {
            for t in &o.themes {
                if t.descriptive_theme_ids.is_empty()
                    || t.descriptive_theme_ids.iter().any(|i| !dids.contains(i))
                {
                    return Err(format!(
                        "analytical theme \"{}\" must cite descriptive theme ids",
                        t.label
                    ));
                }
            }
            Ok(())
        },
    )
    .await
    .map_err(|e| match e {
        AgentError::NeedsHuman(m) => JobError::Pause(format!("Analytical themes failed: {m}")),
        other => other.into(),
    })?;
    let mut analytical: Vec<(String, String)> = Vec::new();
    {
        let conn = ctx.open().map_err(err)?;
        for t in &reply.value.themes {
            let d_ids: Vec<String> = t
                .descriptive_theme_ids
                .iter()
                .filter_map(|s| dids.iter().position(|x| x == s).map(|p| desc[p].0.clone()))
                .collect();
            let id = add_item(
                &conn,
                review_id,
                "analytical_theme",
                None,
                &t.label,
                &json!({"statement": t.statement, "descriptive_theme_ids": d_ids}),
            )
            .map_err(err)?;
            synth_event(
                &conn,
                Some(&reply.meta),
                review_id,
                &id,
                "analytical_theme",
                t.label.clone(),
            )
            .map_err(err)?;
            analytical.push((id, format!("{}: {}", t.label, t.statement)));
        }
    }

    // 4. GRADE-CERQual drafts, one independent call per theme.
    let mut inputs: Vec<(String, usize)> = Vec::new(); // (studies text, study count)
    {
        let conn = ctx.open().map_err(err)?;
        let all_items = items(&conn).map_err(err)?;
        for (theme_id, _) in &analytical {
            let values = supporting_values(&all_items, theme_id);
            let mut studies: Vec<String> = Vec::new();
            for v in &values {
                if let Some(f) = all.iter().find(|f| &f.value_id == v) {
                    if !studies.contains(&f.study_id) {
                        studies.push(f.study_id.clone());
                    }
                }
            }
            let mut text = String::new();
            for s in &studies {
                let label = all
                    .iter()
                    .find(|f| &f.study_id == s)
                    .map(|f| f.study_label.clone())
                    .unwrap_or_default();
                let quotes: Vec<String> = all
                    .iter()
                    .filter(|f| &f.study_id == s && values.contains(&f.value_id))
                    .map(|f| format!("\"{}\"", f.quote))
                    .take(3)
                    .collect();
                text.push_str(&format!(
                    "- {label}. Appraisal: {}. Supporting quotes: {}\n",
                    super::appraisal::summary(&conn, s).map_err(err)?,
                    quotes.join(" ")
                ));
            }
            inputs.push((text, studies.len()));
        }
    }
    let total = analytical.len();
    let done = AtomicUsize::new(0);
    live.step(format!("Drafting GRADE-CERQual confidence for {total} themes"));
    let work: Vec<_> = analytical.iter().zip(inputs.iter()).collect();
    let replies = par::bounded(work, par::LIMIT, |((_, finding), (studies_text, _))| {
        let done = &done;
        async move {
            let vars = [
                ("question", question.to_owned()),
                ("finding", finding.clone()),
                ("studies", studies_text.clone()),
            ];
            let reply =
                call::<CerqualOut, _>(ctx, Task::Synthesis, "cerqual.v2", &vars, 2000, |_| Ok(()))
                    .await;
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            live.step(format!("Drafting GRADE-CERQual confidence: {n} of {total} themes"));
            reply
        }
    })
    .await;
    let conn = ctx.open().map_err(err)?;
    for (((theme_id, _), (_, n_studies)), reply) in analytical.iter().zip(&inputs).zip(replies) {
        let (body, meta, rationale) = match reply {
            Ok(r) => {
                let mut body = serde_json::to_value(&r.value).unwrap_or_default();
                body["studies"] = json!(n_studies);
                body["confirmed"] = json!(false);
                (body, Some(r.meta), format!("{:?}", r.value.overall))
            }
            Err(AgentError::NeedsHuman(m)) => (not_drafted(*n_studies), None, format!("not drafted: {m}")),
            Err(other) => return Err(other.into()),
        };
        let id = add_item(
            &conn,
            review_id,
            "cerqual",
            Some(theme_id),
            "GRADE-CERQual",
            &body,
        )
        .map_err(err)?;
        synth_event(
            &conn,
            meta.as_ref(),
            review_id,
            &id,
            "draft_certainty",
            rationale,
        )
        .map_err(err)?;
    }
    Ok(analytical.len())
}

/// A certainty item the model could not draft: the author judges it at sign-off.
fn not_drafted(studies: usize) -> serde_json::Value {
    json!({
        "overall": null,
        "not_drafted": true,
        "studies": studies,
        "explanation": "The draft could not be generated. Judge the confidence in this finding yourself.",
        "confirmed": false,
    })
}

/// Stores descriptive themes; codes the model left out go into one extra theme.
fn store_descriptive(
    conn: &Connection,
    review_id: &str,
    out: &ThemesOut,
    meta: &CallMeta,
    cids: &[String],
    code_rows: &[(String, String, String)],
) -> Result<Vec<(String, String, String)>> {
    let mut desc: Vec<(String, String, String)> = Vec::new();
    let mut make_theme = |label: &str,
                          description: &str,
                          code_short: &[String],
                          meta: Option<&CallMeta>|
     -> Result<()> {
        let code_ids: Vec<String> = code_short
            .iter()
            .filter_map(|s| {
                cids.iter()
                    .position(|x| x == s)
                    .map(|p| code_rows[p].0.clone())
            })
            .collect();
        let id = add_item(
            conn,
            review_id,
            "descriptive_theme",
            None,
            label,
            &json!({"description": description, "code_ids": code_ids}),
        )?;
        for c in &code_ids {
            conn.execute(
                "UPDATE synthesis_item SET parent_id = ?1 WHERE id = ?2",
                params![id, c],
            )?;
        }
        synth_event(
            conn,
            meta,
            review_id,
            &id,
            "descriptive_theme",
            format!("{label} ({} codes)", code_ids.len()),
        )?;
        desc.push((id, label.to_owned(), description.to_owned()));
        Ok(())
    };
    let mut assigned = std::collections::HashSet::new();
    for t in &out.themes {
        for c in &t.code_ids {
            assigned.insert(c.clone());
        }
        make_theme(&t.label, &t.description, &t.code_ids, Some(meta))?;
    }
    let left: Vec<String> = cids
        .iter()
        .filter(|c| !assigned.contains(*c))
        .cloned()
        .collect();
    if !left.is_empty() {
        make_theme(
            "Codes not grouped by the model",
            "Codes the synthesis agent did not assign to a theme. Review them at sign-off.",
            &left,
            None,
        )?;
    }
    Ok(desc)
}

// ---------------------------------------------------------------------------
// Quantitative path

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VoteCount {
    pub favours_intervention: usize,
    pub favours_comparator: usize,
    pub no_clear_difference: usize,
    pub unclear: usize,
    pub studies: usize,
}

/// Vote counting by direction of effect (SWiM), one vote per study per group:
/// a study with mixed directions within a group counts as unclear.
pub fn vote_count(results: &[(String, Direction)]) -> VoteCount {
    let mut per_study: HashMap<&str, Vec<Direction>> = HashMap::new();
    for (study, d) in results {
        per_study.entry(study.as_str()).or_default().push(*d);
    }
    let mut v = VoteCount {
        studies: per_study.len(),
        ..Default::default()
    };
    for dirs in per_study.values() {
        let first = dirs[0];
        let d = if dirs.iter().all(|x| *x == first) {
            first
        } else {
            Direction::Unclear
        };
        match d {
            Direction::FavoursIntervention => v.favours_intervention += 1,
            Direction::FavoursComparator => v.favours_comparator += 1,
            Direction::NoClearDifference => v.no_clear_difference += 1,
            Direction::Unclear => v.unclear += 1,
        }
    }
    v
}

async fn quantitative(
    ctx: &JobContext,
    live: &Tracked,
    review_id: &str,
    question: &str,
) -> std::result::Result<usize, JobError> {
    let err = |e: Error| JobError::Fatal(e.to_string());
    let outcomes: Vec<(String, String, String, serde_json::Value)> = {
        let conn = ctx.open().map_err(err)?;
        let mut stmt = conn
            .prepare(
                "SELECT ev.id, ev.study_id, s.label, ev.value FROM extraction_value ev JOIN study s ON s.id = ev.study_id
                 WHERE ev.field LIKE 'outcome_%' AND ev.validation = 'verified' ORDER BY s.label, ev.field",
            )
            .map_err(|e| JobError::Fatal(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                let v: String = r.get(3)?;
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    serde_json::from_str(&v).unwrap_or(serde_json::Value::Null),
                ))
            })
            .map_err(|e| JobError::Fatal(e.to_string()))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| JobError::Fatal(e.to_string()))?
    };
    if outcomes.is_empty() {
        return Ok(0);
    }
    live.step(format!(
        "Grouping {} outcome results by construct (SWiM)",
        outcomes.len()
    ));
    let oids: Vec<String> = (1..=outcomes.len()).map(|i| format!("O{i}")).collect();
    let listing = outcomes
        .iter()
        .zip(&oids)
        .map(|((_, _, label, v), id)| format!("{id} ({label}): {v}"))
        .collect::<Vec<_>>()
        .join("\n");
    let reply = call::<SwimOut, _>(
        ctx,
        Task::Synthesis,
        "swim.v2",
        &[("question", question.to_owned()), ("results", listing)],
        4000,
        |o| {
            for g in &o.groups {
                if g.results.iter().any(|r| !oids.contains(&r.outcome_id)) {
                    return Err(format!(
                        "group \"{}\" cites unknown outcome ids",
                        g.construct
                    ));
                }
            }
            Ok(())
        },
    )
    .await
    .map_err(|e| match e {
        AgentError::NeedsHuman(m) => JobError::Pause(format!("SWiM grouping failed: {m}")),
        other => other.into(),
    })?;
    // Store every group (votes are counted in code), then draft GRADE for
    // all groups side by side.
    let mut groups: Vec<(String, String, String)> = Vec::new(); // (item id, construct, evidence)
    let conn = ctx.open().map_err(err)?;
    for g in &reply.value.groups {
        let mut study_dirs: Vec<(String, Direction)> = Vec::new();
        let mut value_ids = Vec::new();
        for r in &g.results {
            if let Some(p) = oids.iter().position(|x| x == &r.outcome_id) {
                study_dirs.push((outcomes[p].1.clone(), r.direction));
                value_ids.push(outcomes[p].0.clone());
            }
        }
        let votes = vote_count(&study_dirs);
        let meta_analysis_eligible = votes.studies >= 3;
        let id = add_item(
            &conn,
            review_id,
            "swim_group",
            None,
            &g.construct,
            &json!({"votes": votes, "meta_analysis_eligible": meta_analysis_eligible,
                    "meta_analysis": if meta_analysis_eligible { "not run: the statistics engine (WebR with metafor) is not available in this version" } else { "fewer than 3 studies" }}),
        )
        .map_err(err)?;
        for v in &value_ids {
            link(&conn, &id, v).map_err(err)?;
        }
        synth_event(
            &conn,
            Some(&reply.meta),
            review_id,
            &id,
            "swim_group",
            format!("{}: {} studies", g.construct, votes.studies),
        )
        .map_err(err)?;
        let evidence = format!(
            "{} studies. Direction of effect: {} favour intervention, {} favour comparator, {} no clear difference, {} unclear.",
            votes.studies, votes.favours_intervention, votes.favours_comparator, votes.no_clear_difference, votes.unclear
        );
        groups.push((id, g.construct.clone(), evidence));
    }
    drop(conn);
    let total = groups.len();
    let done = AtomicUsize::new(0);
    live.step(format!("Drafting GRADE certainty for {total} outcome groups"));
    let replies = par::bounded(groups.iter().collect(), par::LIMIT, |(_, construct, evidence)| {
        let done = &done;
        async move {
            let reply = call::<GradeOut, _>(
                ctx,
                Task::Synthesis,
                "grade.v2",
                &[("outcome", construct.clone()), ("evidence", evidence.clone())],
                1500,
                |_| Ok(()),
            )
            .await;
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            live.step(format!("Drafting GRADE certainty: {n} of {total} outcome groups"));
            reply
        }
    })
    .await;
    let conn = ctx.open().map_err(err)?;
    for ((id, _, _), reply) in groups.iter().zip(replies) {
        let (body, meta, rationale) = match reply {
            Ok(r) => {
                let mut body = serde_json::to_value(&r.value).unwrap_or_default();
                body["confirmed"] = json!(false);
                (body, Some(r.meta), format!("{:?}", r.value.overall))
            }
            Err(AgentError::NeedsHuman(m)) => {
                let mut body = not_drafted(0);
                if let Some(o) = body.as_object_mut() {
                    o.remove("studies");
                }
                (body, None, format!("not drafted: {m}"))
            }
            Err(other) => return Err(other.into()),
        };
        let gid = add_item(&conn, review_id, "grade", Some(id), "GRADE", &body).map_err(err)?;
        synth_event(&conn, meta.as_ref(), review_id, &gid, "draft_certainty", rationale)
            .map_err(err)?;
    }
    Ok(total)
}

pub struct Synthesise;

#[async_trait]
impl JobHandler for Synthesise {
    async fn run(&self, _job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        let err = |e: Error| JobError::Fatal(e.to_string());
        let (review_id, question) = {
            let conn = ctx.open().map_err(err)?;
            require_locked(&conn).map_err(err)?;
            // A rerun replaces earlier synthesis items.
            conn.execute_batch("DELETE FROM synthesis_link; DELETE FROM synthesis_item;")
                .map_err(|e| JobError::Fatal(e.to_string()))?;
            review_question(&conn).map_err(err)?
        };
        let live = ctx
            .env
            .live
            .track_in(&review_id, "synthesis", &review_id, "Synthesis");
        let themes = qualitative(ctx, &live, &review_id, &question).await?;
        let groups = quantitative(ctx, &live, &review_id, &question).await?;
        let conn = ctx.open().map_err(err)?;
        synth_event(
            &conn,
            None,
            &review_id,
            &review_id,
            "complete",
            format!("{themes} analytical themes; {groups} SWiM outcome groups."),
        )
        .map_err(err)?;
        Ok(())
    }
}

/// The user confirms or changes a drafted certainty judgement (Gate 3).
pub fn confirm_certainty(
    conn: &Connection,
    item_id: &str,
    overall: &str,
    note: Option<&str>,
) -> Result<()> {
    let valid = ["high", "moderate", "low", "very low"];
    if !valid.contains(&overall) {
        return Err(Error::Invalid(
            "Choose high, moderate, low or very low.".into(),
        ));
    }
    let body: String = conn.query_row(
        "SELECT body FROM synthesis_item WHERE id = ?1 AND kind IN ('cerqual','grade')",
        [item_id],
        |r| r.get(0),
    )?;
    let mut v: serde_json::Value = serde_json::from_str(&body)?;
    let drafted = v["overall"].as_str().unwrap_or("").to_owned();
    v["overall"] = json!(overall);
    v["confirmed"] = json!(true);
    conn.execute(
        "UPDATE synthesis_item SET body = ?1 WHERE id = ?2",
        params![v.to_string(), item_id],
    )?;
    let (review_id, _) = review_question(conn)?;
    events::append(
        conn,
        &NewEvent {
            review_id,
            target_type: "synthesis".into(),
            target_id: item_id.into(),
            stage: "gate3".into(),
            actor_type: Some(ActorType::Human),
            actor_id: "human".into(),
            decision: if drafted == overall {
                "confirm"
            } else {
                "change"
            }
            .into(),
            reason_code: Some(overall.into()),
            rationale: note
                .map(str::to_owned)
                .or(Some(format!("Drafted as {drafted}."))),
            ..Default::default()
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn votes_one_per_study_mixed_is_unclear() {
        let v = vote_count(&[
            ("s1".into(), Direction::FavoursIntervention),
            ("s1".into(), Direction::FavoursIntervention),
            ("s2".into(), Direction::FavoursComparator),
            ("s3".into(), Direction::FavoursIntervention),
            ("s3".into(), Direction::NoClearDifference),
        ]);
        assert_eq!(
            v,
            VoteCount {
                favours_intervention: 1,
                favours_comparator: 1,
                no_clear_difference: 0,
                unclear: 1,
                studies: 3
            }
        );
    }

    #[test]
    fn supporting_values_follow_theme_links() {
        let mk = |id: &str, kind: &str, body: serde_json::Value, values: &[&str]| Item {
            id: id.into(),
            kind: kind.into(),
            parent_id: None,
            label: id.into(),
            body,
            value_ids: values.iter().map(|v| v.to_string()).collect(),
        };
        let all = vec![
            mk("c1", "code", json!({}), &["v1", "v2"]),
            mk("c2", "code", json!({}), &["v3"]),
            mk(
                "d1",
                "descriptive_theme",
                json!({"code_ids": ["c1", "c2"]}),
                &[],
            ),
            mk(
                "a1",
                "analytical_theme",
                json!({"descriptive_theme_ids": ["d1"]}),
                &[],
            ),
        ];
        assert_eq!(supporting_values(&all, "a1"), vec!["v1", "v2", "v3"]);
    }

    #[test]
    fn certainty_enums_serialise_to_tool_wording() {
        assert_eq!(
            serde_json::to_value(Concern::None).unwrap(),
            "no or very minor concerns"
        );
        assert_eq!(
            serde_json::to_value(Confidence::VeryLow).unwrap(),
            "very low"
        );
        assert_eq!(
            serde_json::to_value(Direction::NoClearDifference).unwrap(),
            "no clear difference"
        );
    }
}
