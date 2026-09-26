//! Data extraction (§8.8). Fields are generated from the protocol. Every value
//! needs a verbatim quote and page; the validator voids any value whose quote
//! is not in the text (principle 3), and the field is flagged for Gate 3.

use async_trait::async_trait;
use rusqlite::{params, Connection};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::protocol::{require_locked, review_question, ProtocolBody, QuestionType};
use super::{call, AgentError, CallMeta};
use crate::error::{Error, Result};
use crate::events::{self, ActorType, NewEvent};
use crate::jobs::{Job, JobContext, JobError, JobHandler};
use crate::llm::store::Task;
use crate::records;
use crate::reviews::new_id;
use crate::validate;

pub const EXTRACT_JOB: &str = "extract";
/// v2 spells out the JSON shape, before and after the full text, for models
/// that ignore structured output. A study that went to the human queue under
/// an earlier version is retried once with the current one.
pub const PROMPT: &str = "extraction.v2";
const MAX_CHARS: usize = 110_000;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FieldSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub instruction: &'static str,
}

/// Extraction form generated from the protocol's question type (§8.8).
pub fn form(p: &ProtocolBody) -> Vec<FieldSpec> {
    let mut f = vec![
        FieldSpec { key: "aim", label: "Study aim", instruction: "The stated aim or research question of the study." },
        FieldSpec { key: "country", label: "Country", instruction: "Country or countries where the study took place." },
        FieldSpec { key: "setting", label: "Setting", instruction: "Institutions or settings, e.g. number and type of universities." },
        FieldSpec { key: "design", label: "Study design", instruction: "Design as described by the authors (e.g. qualitative interview study, RCT, survey)." },
        FieldSpec { key: "sample_size", label: "Sample size", instruction: "Number of participants analysed, as reported." },
        FieldSpec { key: "participants", label: "Participants", instruction: "Who the participants were (level of study, nationality, gender, age)." },
        FieldSpec { key: "data_collection", label: "Data collection", instruction: "Methods of data collection." },
        FieldSpec { key: "analysis", label: "Analysis", instruction: "Analysis approach named by the authors." },
        FieldSpec { key: "funding", label: "Funding", instruction: "Funding source, if stated." },
    ];
    if matches!(
        p.question_type,
        QuestionType::Intervention | QuestionType::Policy | QuestionType::Mixed
    ) {
        f.push(FieldSpec {
            key: "intervention",
            label: "Intervention",
            instruction: "The intervention, programme or policy studied.",
        });
        f.push(FieldSpec {
            key: "comparator",
            label: "Comparator",
            instruction: "The comparison condition, if any.",
        });
    }
    if matches!(p.question_type, QuestionType::Exposure) {
        f.push(FieldSpec {
            key: "exposure",
            label: "Exposure",
            instruction: "The exposure or characteristic studied.",
        });
    }
    f
}

pub fn wants_findings(p: &ProtocolBody) -> bool {
    matches!(
        p.question_type,
        QuestionType::Experience | QuestionType::Mixed | QuestionType::Policy
    )
}

pub fn wants_outcomes(p: &ProtocolBody) -> bool {
    matches!(
        p.question_type,
        QuestionType::Intervention
            | QuestionType::Exposure
            | QuestionType::Mixed
            | QuestionType::Policy
    )
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ExtractedField {
    pub field: String,
    pub value: Option<String>,
    pub source_quote: Option<String>,
    pub page: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ExtractedFinding {
    pub finding: String,
    pub source_quote: String,
    pub page: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ExtractedOutcome {
    pub outcome: String,
    pub group_or_comparison: Option<String>,
    /// mean, SD, n, t, F, OR, r, ...
    pub statistic_type: String,
    pub value: String,
    pub direction_stated: Option<String>,
    pub source_quote: String,
    pub page: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ExtractionOut {
    pub fields: Vec<ExtractedField>,
    pub findings: Vec<ExtractedFinding>,
    pub outcomes: Vec<ExtractedOutcome>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub field: String,
    pub value: Option<serde_json::Value>,
    pub quote: Option<String>,
    pub page: Option<i64>,
    pub validation: &'static str,
}

/// Applies the quote validator to one value.
pub fn check_value(
    field: &str,
    value: Option<serde_json::Value>,
    quote: Option<&str>,
    page: Option<i64>,
    pages: &[String],
) -> Checked {
    let quote = quote.map(str::trim).filter(|q| !q.is_empty());
    match (value, quote) {
        (None, _) => Checked {
            field: field.into(),
            value: None,
            quote: None,
            page: None,
            validation: "not_reported",
        },
        (Some(v), Some(q)) => match validate::locate(q, pages, page) {
            Some(p) => Checked {
                field: field.into(),
                value: Some(v),
                quote: Some(q.into()),
                page: Some(p),
                validation: "verified",
            },
            None => Checked {
                field: field.into(),
                value: None,
                quote: Some(q.into()),
                page,
                validation: "quote_not_found",
            },
        },
        (Some(_), None) => Checked {
            field: field.into(),
            value: None,
            quote: None,
            page,
            validation: "quote_not_found",
        },
    }
}

/// Validates every extracted item against the pages.
pub fn check_all(out: &ExtractionOut, form: &[FieldSpec], pages: &[String]) -> Vec<Checked> {
    let mut checked = Vec::new();
    for spec in form {
        let got = out.fields.iter().find(|f| f.field == spec.key);
        let value = got
            .and_then(|f| f.value.clone())
            .filter(|v| !v.trim().is_empty())
            .map(serde_json::Value::String);
        checked.push(check_value(
            spec.key,
            value,
            got.and_then(|f| f.source_quote.as_deref()),
            got.and_then(|f| f.page),
            pages,
        ));
    }
    for (i, f) in out.findings.iter().enumerate() {
        let v = json!({"finding": f.finding});
        checked.push(check_value(
            &format!("finding_{:02}", i + 1),
            Some(v),
            Some(&f.source_quote),
            f.page,
            pages,
        ));
    }
    for (i, o) in out.outcomes.iter().enumerate() {
        let v = json!({
            "outcome": o.outcome, "group": o.group_or_comparison, "statistic_type": o.statistic_type,
            "value": o.value, "direction_stated": o.direction_stated
        });
        let mut c = check_value(
            &format!("outcome_{:02}", i + 1),
            Some(v),
            Some(&o.source_quote),
            o.page,
            pages,
        );
        // Effect data must be copied exactly: the reported number has to be inside the quote.
        if c.validation == "verified" && !validate::quote_in(&o.value, &o.source_quote) {
            c.value = None;
            c.validation = "quote_not_found";
        }
        checked.push(c);
    }
    checked
}

// ---------------------------------------------------------------------------
// Second request for quotes the validator could not find
//
// Models often paraphrase or trim a quote even when the value is right. One
// narrow follow-up asks for the exact passage behind each flagged value; the
// value itself cannot change, and the answer goes through the same validator.
// Only values still unverified afterwards go to the author at Gate 3.

pub const REQUOTE_PROMPT: &str = "extraction_requote.v1";

/// One flagged value to ask about again.
#[derive(Debug, Clone, PartialEq)]
pub struct Retry {
    pub index: usize,
    pub id: String,
    pub value: serde_json::Value,
    /// The value in words, shown to the model.
    pub shown: String,
    /// For effect data: the reported number that must appear in the quote.
    pub number: Option<String>,
    pub page: Option<i64>,
    pub quote: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RequotedItem {
    pub id: String,
    pub source_quote: Option<String>,
    pub page: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RequoteOut {
    pub items: Vec<RequotedItem>,
}

/// The flagged values of a first pass that had a value to look for.
pub fn retries(out: &ExtractionOut, checked: &[Checked]) -> Vec<Retry> {
    let nth = |id: &str, prefix: &str| -> Option<usize> {
        id.strip_prefix(prefix)?.parse::<usize>().ok()?.checked_sub(1)
    };
    checked
        .iter()
        .enumerate()
        .filter(|(_, c)| c.validation == "quote_not_found")
        .filter_map(|(index, c)| {
            let id = c.field.clone();
            if let Some(i) = nth(&id, "finding_") {
                let f = out.findings.get(i)?;
                return Some(Retry {
                    index,
                    value: json!({"finding": f.finding}),
                    shown: f.finding.clone(),
                    number: None,
                    page: f.page,
                    quote: Some(f.source_quote.clone()),
                    id,
                });
            }
            if let Some(i) = nth(&id, "outcome_") {
                let o = out.outcomes.get(i)?;
                return Some(Retry {
                    index,
                    value: json!({
                        "outcome": o.outcome, "group": o.group_or_comparison, "statistic_type": o.statistic_type,
                        "value": o.value, "direction_stated": o.direction_stated
                    }),
                    shown: format!(
                        "{} {}{} = {}",
                        o.outcome,
                        o.group_or_comparison.as_deref().map(|g| format!("({g}) ")).unwrap_or_default(),
                        o.statistic_type,
                        o.value
                    ),
                    number: Some(o.value.clone()),
                    page: o.page,
                    quote: Some(o.source_quote.clone()),
                    id,
                });
            }
            let f = out.fields.iter().find(|f| f.field == id)?;
            let v = f.value.clone().filter(|v| !v.trim().is_empty())?;
            Some(Retry {
                index,
                value: serde_json::Value::String(v.clone()),
                shown: v,
                number: None,
                page: f.page,
                quote: f.source_quote.clone(),
                id,
            })
        })
        .collect()
}

fn retry_text(rs: &[Retry]) -> String {
    rs.iter()
        .map(|r| {
            format!(
                "- id: {}\n  value: {}\n  earlier quote (not found): {}\n  earlier page: {}\n",
                r.id,
                r.shown,
                r.quote.as_deref().unwrap_or("none"),
                r.page.map(|p| p.to_string()).unwrap_or_else(|| "none".into())
            )
        })
        .collect()
}

/// The pages the model needs: each claimed page and its neighbours, or the
/// whole text when any value came without a page.
fn retry_pages(rs: &[Retry], pages: &[String]) -> String {
    if rs.iter().any(|r| r.page.is_none()) {
        return records::paged_text(pages, MAX_CHARS);
    }
    let mut want: Vec<usize> = rs
        .iter()
        .filter_map(|r| r.page)
        .flat_map(|p| [p - 1, p, p + 1])
        .filter(|p| *p >= 1 && (*p as usize) <= pages.len())
        .map(|p| p as usize)
        .collect();
    want.sort_unstable();
    want.dedup();
    if want.is_empty() {
        return records::paged_text(pages, MAX_CHARS);
    }
    want.iter()
        .map(|p| format!("[[page {p}]]\n{}\n\n", pages[p - 1].trim()))
        .collect()
}

/// Applies the second answers; returns the indexes of values now verified.
pub fn apply_requote(
    checked: &mut [Checked],
    rs: &[Retry],
    answer: &RequoteOut,
    pages: &[String],
) -> Vec<usize> {
    let mut fixed = Vec::new();
    for r in rs {
        let Some(a) = answer.items.iter().find(|a| a.id == r.id) else {
            continue;
        };
        let Some(q) = a.source_quote.as_deref().filter(|q| !q.trim().is_empty()) else {
            continue;
        };
        let c = check_value(&r.id, Some(r.value.clone()), Some(q), a.page, pages);
        if c.validation != "verified" {
            continue;
        }
        if let Some(n) = &r.number {
            if !validate::quote_in(n, q) {
                continue;
            }
        }
        if let Some(slot) = checked.get_mut(r.index) {
            *slot = c;
            fixed.push(r.index);
        }
    }
    fixed
}

fn fields_text(form: &[FieldSpec], findings: bool, outcomes: bool) -> String {
    let mut s: String = form
        .iter()
        .map(|f| format!("- {} ({}): {}\n", f.key, f.label, f.instruction))
        .collect();
    if findings {
        s.push_str("- findings: each distinct finding relevant to the review question, with a supporting quote.\n");
    } else {
        s.push_str("- findings: return an empty list.\n");
    }
    if outcomes {
        s.push_str("- outcomes: each reported result for an outcome relevant to the question, with statistic type and value copied exactly.\n");
    } else {
        s.push_str("- outcomes: return an empty list.\n");
    }
    s
}

/// The report whose text is used for a study: the longest text among its included reports.
pub fn primary_report(conn: &Connection, study_id: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM v_included_report WHERE study_id = ?1 ORDER BY COALESCE(length(page_texts), 0) DESC LIMIT 1",
    )?;
    Ok(stmt.query_row([study_id], |r| r.get(0)).ok())
}

/// The study's current "needs your review" extraction event, which a new
/// attempt supersedes.
fn open_failure(conn: &Connection, study_id: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT MAX(id) FROM v_current_event
             WHERE stage = 'extract' AND target_type = 'study' AND target_id = ?1 AND decision = 'needs_human'",
            [study_id],
            |r| r.get(0),
        )?)
}

fn store(
    conn: &Connection,
    review_id: &str,
    study_id: &str,
    report_id: &str,
    meta: &CallMeta,
    checked: &[Checked],
    requoted: Option<(&CallMeta, &[usize])>,
) -> Result<(usize, usize)> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM extraction_value WHERE study_id = ?1 AND id NOT IN (SELECT extraction_value_id FROM synthesis_link)", [study_id])?;
    let mut flagged = 0;
    for (i, c) in checked.iter().enumerate() {
        // A value verified on the second request carries that call's provenance.
        let second = requoted.and_then(|(m, ix)| ix.contains(&i).then_some(m));
        let id = new_id();
        tx.execute(
            "INSERT INTO extraction_value (id, study_id, report_id, field, value, source_quote, page, validation) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, study_id, report_id, c.field, c.value.as_ref().map(|v| v.to_string()), c.quote, c.page, c.validation],
        )?;
        if c.validation == "quote_not_found" {
            flagged += 1;
        }
        let mut e = second.unwrap_or(meta).event(
            review_id,
            "extraction_value",
            &id,
            "extract",
            "extraction_agent",
            c.validation,
        );
        e.evidence_quote = c.quote.clone();
        e.rationale = Some(format!(
            "{}{}{}",
            c.field,
            c.page.map(|p| format!(", page {p}")).unwrap_or_default(),
            if second.is_some() {
                "; verbatim quote supplied on a second request"
            } else {
                ""
            }
        ));
        // Tokens and cost belong to the study-level event, not every value.
        e.tokens_in = None;
        e.tokens_out = None;
        e.cost_usd = None;
        events::append(&tx, &e)?;
    }
    let mut e = meta.event(
        review_id,
        "study",
        study_id,
        "extract",
        "extraction_agent",
        "extracted",
    );
    let recovered = requoted.map(|(_, ix)| ix.len()).unwrap_or(0);
    e.rationale = Some(format!(
        "{} values, {flagged} flagged (quote not found){}.",
        checked.len(),
        if recovered > 0 {
            format!(", {recovered} verified on a second request for the exact quote")
        } else {
            String::new()
        }
    ));
    e.supersedes = open_failure(&tx, study_id)?;
    events::append(&tx, &e)?;
    tx.commit()?;
    Ok((checked.len(), flagged))
}

/// Whether a study needs no further extraction: it was extracted, or the
/// current prompt already failed on it and it waits for the user. A failure
/// under an older prompt version is retried once with the current one.
pub fn settled(conn: &Connection, study_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM v_current_event WHERE stage = 'extract' AND target_type = 'study' AND target_id = ?1
           AND (decision = 'extracted' OR (decision = 'needs_human' AND prompt_version = ?2)))",
        rusqlite::params![study_id, PROMPT],
        |r| r.get(0),
    )?)
}

pub struct Extract;

#[async_trait]
impl JobHandler for Extract {
    async fn run(&self, job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        let study_id = job.payload["study_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let err = |e: Error| JobError::Fatal(e.to_string());
        let (review_id, question, protocol, report_id, pages, label) = {
            let conn = ctx.open().map_err(err)?;
            // An older queued job for a study already settled under the
            // current prompt has nothing left to do.
            if settled(&conn, &study_id).map_err(err)? {
                return Ok(());
            }
            let label = study_label(&conn, &study_id).map_err(err)?;
            let (rid, q) = review_question(&conn).map_err(err)?;
            let p = require_locked(&conn).map_err(err)?.body;
            let rep = primary_report(&conn, &study_id)
                .map_err(err)?
                .ok_or_else(|| JobError::Fatal("study has no included report".into()))?;
            let pages = records::page_texts(&conn, &rep).map_err(err)?;
            (rid, q, p, rep, pages, label)
        };
        let live = ctx
            .env
            .live
            .track_in(&review_id, "extraction", &study_id, &label);
        let spec = form(&protocol);
        let vars = [
            ("question", question),
            (
                "fields",
                fields_text(&spec, wants_findings(&protocol), wants_outcomes(&protocol)),
            ),
            (
                "keys",
                spec.iter().map(|f| f.key).collect::<Vec<_>>().join(", "),
            ),
            ("report", records::paged_text(&pages, MAX_CHARS)),
        ];
        live.step(format!(
            "Extracting {} fields from {} pages",
            spec.len(),
            pages.len()
        ));
        let reply =
            match call::<ExtractionOut, _>(ctx, Task::Extraction, PROMPT, &vars, 8000, |_| Ok(()))
                .await
            {
                Ok(r) => r,
                Err(AgentError::NeedsHuman(why)) => {
                    let conn = ctx.open().map_err(err)?;
                    let supersedes = open_failure(&conn, &study_id).map_err(err)?;
                    events::append(
                        &conn,
                        &NewEvent {
                            review_id,
                            target_type: "study".into(),
                            target_id: study_id,
                            stage: "extract".into(),
                            actor_type: Some(ActorType::Agent),
                            actor_id: "extraction_agent".into(),
                            prompt_version: Some(PROMPT.into()),
                            decision: "needs_human".into(),
                            rationale: Some(why),
                            supersedes,
                            ..Default::default()
                        },
                    )
                    .map_err(err)?;
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            };
        live.step("Checking every quote against the full text");
        let mut checked = check_all(&reply.value, &spec, &pages);
        let rs = retries(&reply.value, &checked);
        let mut second: Option<(CallMeta, Vec<usize>)> = None;
        if !rs.is_empty() {
            live.step(format!(
                "Asking again for the exact quote behind {} value{}",
                rs.len(),
                if rs.len() == 1 { "" } else { "s" }
            ));
            let vars = [
                ("items", retry_text(&rs)),
                (
                    "ids",
                    rs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>().join(", "),
                ),
                ("report", retry_pages(&rs, &pages)),
            ];
            // A failed second request leaves the values flagged for the author;
            // the first pass is never lost.
            if let Ok(r) =
                call::<RequoteOut, _>(ctx, Task::Extraction, REQUOTE_PROMPT, &vars, 4000, |_| Ok(()))
                    .await
            {
                let fixed = apply_requote(&mut checked, &rs, &r.value, &pages);
                second = Some((r.meta, fixed));
            }
        }
        let conn = ctx.open().map_err(err)?;
        store(
            &conn,
            &review_id,
            &study_id,
            &report_id,
            &reply.meta,
            &checked,
            second.as_ref().map(|(m, ix)| (m, ix.as_slice())),
        )
        .map_err(err)?;
        Ok(())
    }
}

/// The study's display label, e.g. "Smith 2021".
pub fn study_label(conn: &Connection, study_id: &str) -> Result<String> {
    Ok(conn.query_row("SELECT label FROM study WHERE id = ?1", [study_id], |r| {
        r.get(0)
    })?)
}

// ---------------------------------------------------------------------------
// Gate 3: the user resolves flagged values

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ValueRow {
    pub id: String,
    pub study_id: String,
    pub report_id: String,
    pub field: String,
    pub value: Option<serde_json::Value>,
    pub source_quote: Option<String>,
    pub page: Option<i64>,
    pub validation: String,
}

pub fn values(conn: &Connection, study_id: Option<&str>) -> Result<Vec<ValueRow>> {
    let sql = "SELECT id, study_id, report_id, field, value, source_quote, page, validation FROM extraction_value
               WHERE (?1 IS NULL OR study_id = ?1) ORDER BY study_id, field";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map([study_id], |r| {
            let v: Option<String> = r.get(4)?;
            Ok(ValueRow {
                id: r.get(0)?,
                study_id: r.get(1)?,
                report_id: r.get(2)?,
                field: r.get(3)?,
                value: v.and_then(|s| serde_json::from_str(&s).ok()),
                source_quote: r.get(5)?,
                page: r.get(6)?,
                validation: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Resolves a flagged value: the user supplies a value with a quote that must
/// be found in the text, or marks it not reported.
pub fn resolve(
    conn: &Connection,
    value_id: &str,
    value: Option<&str>,
    quote: Option<&str>,
    page: Option<i64>,
) -> Result<ValueRow> {
    let row = values(conn, None)?
        .into_iter()
        .find(|v| v.id == value_id)
        .ok_or_else(|| Error::Invalid("value not found".into()))?;
    let pages = records::page_texts(conn, &row.report_id)?;
    let (review_id, _) = review_question(conn)?;
    let (new_value, new_quote, new_page, validation) = match value
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        None => (None, None, None, "not_reported"),
        Some(v) => {
            let q = quote
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .ok_or_else(|| Error::Invalid("Add the quote that supports the value.".into()))?;
            let p = validate::locate(q, &pages, page).ok_or_else(|| {
                Error::Invalid(
                    "That quote is not in the text on that page or the next. Copy it exactly."
                        .into(),
                )
            })?;
            let json_value = if row.field.starts_with("finding_") {
                json!({"finding": v})
            } else {
                serde_json::Value::String(v.to_owned())
            };
            (Some(json_value), Some(q.to_owned()), Some(p), "verified")
        }
    };
    conn.execute(
        "UPDATE extraction_value SET value = ?1, source_quote = ?2, page = ?3, validation = ?4 WHERE id = ?5",
        params![new_value.as_ref().map(|v| v.to_string()), new_quote, new_page, validation, value_id],
    )?;
    events::append(
        conn,
        &NewEvent {
            review_id,
            target_type: "extraction_value".into(),
            target_id: value_id.into(),
            stage: "gate3".into(),
            actor_type: Some(ActorType::Human),
            actor_id: "human".into(),
            decision: "resolve".into(),
            reason_code: Some(validation.into()),
            evidence_quote: new_quote,
            rationale: Some(format!("{}: resolved by the user", row.field)),
            ..Default::default()
        },
    )?;
    values(conn, None)?
        .into_iter()
        .find(|v| v.id == value_id)
        .ok_or_else(|| Error::Invalid("value lost".into()))
}

/// Flagged values the user has not resolved.
pub fn open_flags(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM extraction_value WHERE validation = 'quote_not_found'",
        [],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_under_an_older_prompt_is_retried_once() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let review = crate::reviews::insert_review(&conn, "Q?").unwrap();
        let fail = |prompt: Option<&str>, supersedes: Option<i64>| {
            events::append(
                &conn,
                &NewEvent {
                    review_id: review.clone(),
                    target_type: "study".into(),
                    target_id: "s".into(),
                    stage: "extract".into(),
                    actor_type: Some(ActorType::Agent),
                    actor_id: "extraction".into(),
                    decision: "needs_human".into(),
                    prompt_version: prompt.map(str::to_owned),
                    supersedes,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        assert!(!settled(&conn, "s").unwrap());
        let old = fail(None, None);
        assert!(!settled(&conn, "s").unwrap());
        assert_eq!(open_failure(&conn, "s").unwrap(), Some(old));
        let new = fail(Some(PROMPT), open_failure(&conn, "s").unwrap());
        assert!(settled(&conn, "s").unwrap());
        // The retry replaced the old failure rather than adding a second one.
        assert_eq!(open_failure(&conn, "s").unwrap(), Some(new));
        let open: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM v_current_event WHERE stage = 'extract' AND decision = 'needs_human'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(open, 1);
    }

    fn pages() -> Vec<String> {
        vec![
            "Abstract. We explored belonging.".to_owned(),
            "We recruited 24 first-year undergraduates at two universities in the north of England.".to_owned(),
            "Participants felt like outsiders in seminars. One said \"I never spoke in class\".".to_owned(),
        ]
    }

    #[test]
    fn values_without_verbatim_quotes_are_voided() {
        let out = ExtractionOut {
            fields: vec![
                ExtractedField {
                    field: "sample_size".into(),
                    value: Some("24".into()),
                    source_quote: Some("We recruited 24 first-year undergraduates".into()),
                    page: Some(2),
                },
                ExtractedField {
                    field: "country".into(),
                    value: Some("England".into()),
                    source_quote: Some("in the north of England".into()),
                    page: Some(3),
                },
                ExtractedField {
                    field: "setting".into(),
                    value: Some("Two universities".into()),
                    source_quote: Some("at three universities".into()),
                    page: Some(2),
                },
                ExtractedField {
                    field: "funding".into(),
                    value: None,
                    source_quote: None,
                    page: None,
                },
            ],
            findings: vec![ExtractedFinding {
                finding: "Felt like outsiders".into(),
                source_quote: "felt like outsiders in seminars".into(),
                page: Some(3),
            }],
            outcomes: vec![],
        };
        let spec = form(&crate::agents::protocol::tests::sample_protocol());
        let checked = check_all(&out, &spec, &pages());
        let get = |f: &str| checked.iter().find(|c| c.field == f).unwrap().clone();
        assert_eq!(get("sample_size").validation, "verified");
        assert_eq!(
            get("country").validation,
            "verified",
            "adjacent page is accepted"
        );
        assert_eq!(
            get("country").page,
            Some(2),
            "page corrected to where the quote is"
        );
        let bad = get("setting");
        assert_eq!((bad.validation, bad.value), ("quote_not_found", None));
        assert_eq!(get("funding").validation, "not_reported");
        assert_eq!(
            get("aim").validation,
            "not_reported",
            "missing fields are not reported"
        );
        assert_eq!(get("finding_01").validation, "verified");
    }

    #[test]
    fn effect_numbers_must_be_inside_the_quote() {
        let pages =
            vec!["The mean belonging score was 3.8 (SD 0.6) in the mentoring group.".to_owned()];
        let mk = |value: &str| ExtractionOut {
            fields: vec![],
            findings: vec![],
            outcomes: vec![ExtractedOutcome {
                outcome: "belonging".into(),
                group_or_comparison: Some("mentoring".into()),
                statistic_type: "mean".into(),
                value: value.into(),
                direction_stated: None,
                source_quote: "The mean belonging score was 3.8 (SD 0.6)".into(),
                page: Some(1),
            }],
        };
        let ok = check_all(&mk("3.8"), &[], &pages);
        assert_eq!(ok[0].validation, "verified");
        let bad = check_all(&mk("3.9"), &[], &pages);
        assert_eq!(bad[0].validation, "quote_not_found");
    }

    #[test]
    fn a_second_request_can_verify_a_flagged_value_but_not_invent_one() {
        let out = ExtractionOut {
            fields: vec![
                ExtractedField {
                    field: "setting".into(),
                    value: Some("Two universities".into()),
                    source_quote: Some("at 2 universities".into()),
                    page: Some(2),
                },
                ExtractedField {
                    field: "country".into(),
                    value: Some("Scotland".into()),
                    source_quote: Some("in Scotland".into()),
                    page: Some(2),
                },
            ],
            findings: vec![],
            outcomes: vec![],
        };
        let spec = form(&crate::agents::protocol::tests::sample_protocol());
        let mut checked = check_all(&out, &spec, &pages());
        let rs = retries(&out, &checked);
        assert_eq!(
            rs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["country", "setting"],
            "only flagged values with a value are asked about again"
        );
        assert!(retry_pages(&rs, &pages()).contains("[[page 3]]"));
        let answer = RequoteOut {
            items: vec![
                RequotedItem {
                    id: "setting".into(),
                    source_quote: Some("at two universities in the north of England".into()),
                    page: Some(2),
                },
                // A quote that is still not in the text changes nothing.
                RequotedItem {
                    id: "country".into(),
                    source_quote: Some("the study took place in Scotland".into()),
                    page: Some(2),
                },
            ],
        };
        let fixed = apply_requote(&mut checked, &rs, &answer, &pages());
        let get = |f: &str| checked.iter().find(|c| c.field == f).unwrap().clone();
        assert_eq!(fixed.len(), 1);
        assert_eq!(get("setting").validation, "verified");
        assert_eq!(
            get("setting").value,
            Some(serde_json::Value::String("Two universities".into())),
            "the value is kept as first extracted"
        );
        assert_eq!(get("country").validation, "quote_not_found");
        assert_eq!(get("country").value, None);
    }

    #[test]
    fn a_second_quote_for_effect_data_must_still_contain_the_number() {
        let pages =
            vec!["The mean belonging score was 3.8 (SD 0.6) in the mentoring group.".to_owned()];
        let out = ExtractionOut {
            fields: vec![],
            findings: vec![],
            outcomes: vec![ExtractedOutcome {
                outcome: "belonging".into(),
                group_or_comparison: None,
                statistic_type: "mean".into(),
                value: "3.9".into(),
                direction_stated: None,
                source_quote: "mean belonging was 3.9".into(),
                page: Some(1),
            }],
        };
        let mut checked = check_all(&out, &[], &pages);
        let rs = retries(&out, &checked);
        assert_eq!(rs.len(), 1);
        let answer = RequoteOut {
            items: vec![RequotedItem {
                id: "outcome_01".into(),
                source_quote: Some("The mean belonging score was 3.8 (SD 0.6)".into()),
                page: Some(1),
            }],
        };
        assert!(apply_requote(&mut checked, &rs, &answer, &pages).is_empty());
        assert_eq!(checked[0].validation, "quote_not_found");
    }
}
