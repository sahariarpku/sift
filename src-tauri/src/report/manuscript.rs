//! Manuscript content and the PRISMA 2020 checklist (§8.11).
//!
//! Sections are one of three kinds:
//! - auto: written by code from the protocol, the event log and the tables;
//!   every number comes from the database.
//! - drafted: drafted for the author to edit (abstract, results of syntheses,
//!   certainty, discussion, registration, availability). The model only
//!   writes prose around facts it is given; counts are inserted by code.
//! - author: written by the author (rationale, support, competing interests).

use async_trait::async_trait;
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::apa::{self, Reference};
use crate::agents::protocol::{self, ProtocolBody};
use crate::agents::{appraisal, call, extraction, par, synthesis, AgentError};
use crate::error::Result;
use crate::jobs::{Job, JobContext, JobError, JobHandler};
use crate::llm::store::Task;
use crate::pipeline::gate3;
use crate::records::{self, Prisma};

pub const DRAFT_JOB: &str = "draft_manuscript";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Auto,
    Drafted,
    Author,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    pub item: &'static str,
    pub heading: &'static str,
    pub kind: Kind,
    pub text: String,
    /// The key the author edits (`author_text`), for drafted and author sections.
    pub key: Option<String>,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StudyRow {
    pub id: String,
    pub label: String,
    /// APA narrative citation of the study's first report: "Smith et al. (2021)".
    pub citation: String,
    pub title: String,
    pub values: Vec<(String, String)>,
    pub tool: Option<String>,
    pub appraisal: Vec<(String, String)>,
    pub reports: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThemeRow {
    pub id: String,
    pub label: String,
    pub statement: String,
    pub certainty: Option<String>,
    pub certainty_confirmed: bool,
    pub descriptive: Vec<String>,
    pub quotes: Vec<(String, String, Option<i64>)>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Manuscript {
    pub title: String,
    pub question: String,
    pub verification: String,
    pub sections: Vec<Section>,
    pub studies: Vec<StudyRow>,
    pub themes: Vec<ThemeRow>,
    pub swim: Vec<(String, serde_json::Value)>,
    pub prisma: Prisma,
    /// The APA reference list as plain text.
    pub references: Vec<String>,
    /// The APA reference list with its parts, for italics and the asterisk
    /// that marks included studies.
    pub apa_references: Vec<Reference>,
    pub keywords: Vec<String>,
    /// From Settings, for the APA title page (`apply_settings`).
    pub author_name: Option<String>,
    pub affiliation: Option<String>,
    /// Points bypass mode settled; the author note says so.
    pub bypass_decisions: usize,
    /// Every current search run, for the PRISMA-S appendix.
    pub searches: Vec<records::SearchRunRow>,
}

/// Fills the title page from Settings, and the funding and competing
/// interests statements (items 25 and 26) when the review has none of its
/// own. Both statements are the author's own words, written once in Settings.
pub fn apply_settings(m: &mut Manuscript, s: &crate::settings::AppSettings) {
    m.author_name = s.author_name.clone();
    m.affiliation = s.author_affiliation.clone();
    for (item, default) in [("25", &s.funding_statement), ("26", &s.competing_interests)] {
        let Some(text) = default.as_deref().filter(|t| !t.trim().is_empty()) else {
            continue;
        };
        if let Some(sec) = m.sections.iter_mut().find(|x| x.item == item) {
            if sec.text.trim().is_empty() {
                sec.text = text.to_owned();
                sec.done = true;
            }
        }
    }
}

fn list(items: &[String]) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        _ => format!(
            "{} and {}",
            items[..items.len() - 1].join(", "),
            items[items.len() - 1]
        ),
    }
}

fn source_name(id: &str) -> String {
    match id {
        "eric" => "ERIC".into(),
        "openalex" => "OpenAlex".into(),
        "core" => "CORE".into(),
        "semantic_scholar" => "Semantic Scholar".into(),
        "citation_chasing" => "citation searching in OpenAlex".into(),
        "manual_import" => "records imported by the author".into(),
        other => other.into(),
    }
}

fn pct(x: Option<f64>) -> String {
    x.map(|v| format!("{:.0}%", v * 100.0))
        .unwrap_or_else(|| "not available".into())
}

/// APA references for every included record.
fn included_references(conn: &Connection) -> Result<Vec<Reference>> {
    Ok(apa::from_records(&records::records_where(
        conn,
        "r.id IN (SELECT record_id FROM v_included_report)",
    )?))
}

/// Study ID to its APA citation (parenthetical, narrative), from the study's
/// first included report; the study label when no record matches.
fn citations(
    conn: &Connection,
    refs: &[Reference],
) -> Result<std::collections::HashMap<String, (String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.label, (SELECT ir.record_id FROM v_included_report ir WHERE ir.study_id = s.id ORDER BY ir.record_id LIMIT 1) FROM study s",
    )?;
    let rows: Vec<(String, String, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .into_iter()
        .map(|(id, label, rec)| {
            let found = rec.and_then(|rid| {
                refs.iter()
                    .find(|r| r.record_id.as_deref() == Some(rid.as_str()))
            });
            let pair = match found {
                Some(r) => (r.cite.clone(), r.narrative.clone()),
                None => (label.clone(), label),
            };
            (id, pair)
        })
        .collect())
}

fn studies(
    conn: &Connection,
    cites: &std::collections::HashMap<String, (String, String)>,
) -> Result<Vec<StudyRow>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.label, (SELECT r.title FROM record r JOIN v_included_report ir ON ir.record_id = r.id WHERE ir.study_id = s.id LIMIT 1),
                (SELECT COUNT(*) FROM v_included_report ir WHERE ir.study_id = s.id)
         FROM study s WHERE s.id IN (SELECT study_id FROM v_included_report) ORDER BY s.label",
    )?;
    let base: Vec<(String, String, Option<String>, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let apps = appraisal::rows(conn)?;
    let mut out = Vec::new();
    for (id, label, title, reports) in base {
        let values = extraction::values(conn, Some(&id))?
            .into_iter()
            .filter(|v| !v.field.starts_with("finding_") && !v.field.starts_with("outcome_"))
            .map(|v| {
                let text = match &v.value {
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None if v.validation == "quote_not_found" => {
                        "[flagged: quote not found]".into()
                    }
                    None => "Not reported".into(),
                };
                (v.field, text)
            })
            .collect();
        let mine: Vec<&appraisal::AppraisalRow> =
            apps.iter().filter(|a| a.study_id == id).collect();
        out.push(StudyRow {
            citation: cites
                .get(&id)
                .map(|c| c.1.clone())
                .unwrap_or_else(|| label.clone()),
            id,
            label,
            title: title.unwrap_or_default(),
            values,
            tool: mine.first().map(|a| a.tool.clone()),
            appraisal: mine
                .iter()
                .map(|a| (a.domain.clone(), a.judgement.clone()))
                .collect(),
            reports,
        });
    }
    Ok(out)
}

fn themes(
    conn: &Connection,
    cites: &std::collections::HashMap<String, (String, String)>,
) -> Result<Vec<ThemeRow>> {
    let items = synthesis::items(conn)?;
    let values = extraction::values(conn, None)?;
    // Quotes are cited in APA parenthetical form: (Smith et al., 2021, p. 4).
    let labels: std::collections::HashMap<String, String> = cites
        .iter()
        .map(|(k, (cite, _))| (k.clone(), cite.clone()))
        .collect();
    Ok(items
        .iter()
        .filter(|i| i.kind == "analytical_theme")
        .map(|t| {
            let cert = items
                .iter()
                .find(|c| c.kind == "cerqual" && c.parent_id.as_deref() == Some(&t.id));
            let desc_ids: Vec<String> = t.body["descriptive_theme_ids"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let quotes = synthesis::supporting_values(&items, &t.id)
                .iter()
                .filter_map(|vid| values.iter().find(|v| &v.id == vid))
                .filter(|v| v.validation == "verified")
                .map(|v| {
                    (
                        labels.get(&v.study_id).cloned().unwrap_or_default(),
                        v.source_quote.clone().unwrap_or_default(),
                        v.page,
                    )
                })
                .collect();
            ThemeRow {
                id: t.id.clone(),
                label: t.label.clone(),
                statement: t.body["statement"].as_str().unwrap_or("").to_owned(),
                certainty: cert.and_then(|c| c.body["overall"].as_str().map(str::to_owned)),
                certainty_confirmed: cert.is_some_and(|c| c.body["confirmed"] == true),
                descriptive: desc_ids
                    .iter()
                    .filter_map(|d| items.iter().find(|i| &i.id == d).map(|i| i.label.clone()))
                    .collect(),
                quotes,
            }
        })
        .collect())
}

/// Facts given to the drafting model, all computed here.
pub fn facts(conn: &Connection) -> Result<String> {
    let (_, question) = protocol::review_question(conn)?;
    let p = records::prisma(conn)?;
    let refs = included_references(conn)?;
    let cites = citations(conn, &refs)?;
    let th = themes(conn, &cites)?;
    let st = studies(conn, &cites)?;
    let mut f = format!(
        "Review question: {question}\nRecords identified: {}. Duplicates removed: {}. Records screened: {}. Records excluded: {}. Reports assessed: {}. Reports not retrieved: {}. Studies included: {} (in {} reports).\n",
        p.identified_databases + p.identified_other,
        p.duplicates_removed,
        p.records_screened,
        p.records_excluded,
        p.reports_assessed,
        p.reports_not_retrieved,
        p.studies_included,
        p.reports_included
    );
    if p.records_not_screened > 0 {
        f.push_str(&format!(
            "Records not screened because screening was stopped early: {}.\n",
            p.records_not_screened
        ));
    }
    if p.reports_awaiting > 0 {
        f.push_str(&format!(
            "Reports awaiting classification because full-text screening was stopped early: {}.\n",
            p.reports_awaiting
        ));
    }
    for k in crate::pipeline::skip::list(conn)? {
        f.push_str(&format!("Stage stopped early ({}): {}\n", k.name, k.rationale));
    }
    f.push_str("Included studies (cite each exactly as given, parenthetical form in brackets):\n");
    for s in &st {
        let get = |k: &str| {
            s.values
                .iter()
                .find(|(f, _)| f == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| "not reported".into())
        };
        f.push_str(&format!(
            "- {} [({})]: {}; country {}; design {}; sample {}\n",
            s.citation,
            cites.get(&s.id).map(|c| c.0.as_str()).unwrap_or(&s.label),
            s.title,
            get("country"),
            get("design"),
            get("sample_size")
        ));
    }
    f.push_str("Analytical themes (with GRADE-CERQual confidence):\n");
    for t in &th {
        f.push_str(&format!(
            "- {}: {} (confidence: {})\n",
            t.label,
            t.statement,
            t.certainty.as_deref().unwrap_or("not assessed")
        ));
    }
    for it in synthesis::items(conn)?
        .iter()
        .filter(|i| i.kind == "swim_group")
    {
        f.push_str(&format!(
            "- Outcome group {}: {}\n",
            it.label, it.body["votes"]
        ));
    }
    Ok(f)
}

pub fn build(conn: &Connection) -> Result<Manuscript> {
    let (_, question) = protocol::review_question(conn)?;
    let locked = protocol::locked(conn)?;
    let body: Option<ProtocolBody> = locked.as_ref().map(|l| l.body.clone());
    let opts = protocol::options(conn)?;
    let p = records::prisma(conn)?;
    let d = super::metrics::disclosure(conn)?;
    let runs = records::runs(conn)?;
    let current: Vec<&records::SearchRunRow> = runs.iter().filter(|r| r.current).collect();
    let included = included_references(conn)?;
    let cites = citations(conn, &included)?;
    let st = studies(conn, &cites)?;
    let th = themes(conn, &cites)?;
    let bypass_decisions = crate::pipeline::bypass::decisions(conn)?.len();
    let bypassed = bypass_decisions > 0;
    let by_author = if bypassed {
        "Judgements were drafted by an LLM; because the review ran in bypass mode, they were not confirmed by the review author."
    } else {
        "Judgements were drafted by an LLM and confirmed or changed by the review author."
    };
    let text = |key: &str| {
        gate3::author_text(conn, key)
            .ok()
            .flatten()
            .unwrap_or_default()
    };

    let model_for = |task: &str| -> String {
        let ms: Vec<String> = d
            .models
            .iter()
            .filter(|m| m.task == task)
            .map(|m| {
                format!(
                    "{} via {} ({})",
                    m.model_reported
                        .clone()
                        .unwrap_or_else(|| m.model_requested.clone()),
                    m.provider,
                    m.prompt_version
                )
            })
            .collect();
        if ms.is_empty() {
            "not used".into()
        } else {
            list(&ms)
        }
    };

    // One sentence per stage the author stopped early, empty otherwise.
    let stopped = |id: &str| -> String {
        d.skips
            .iter()
            .find(|k| k.stage == id)
            .map(|k| format!(" {} (Decision recorded on {}.)", k.rationale, &k.at[..10]))
            .unwrap_or_default()
    };
    let title = apa::title_case(&format!(
        "{}: a systematic review",
        question.trim().trim_end_matches('?')
    ));
    let mut sections: Vec<Section> = Vec::new();
    let mut auto = |item: &'static str, heading: &'static str, text: String| {
        sections.push(Section {
            item,
            heading,
            kind: Kind::Auto,
            done: !text.trim().is_empty(),
            text,
            key: None,
        });
    };

    auto("1", "Title", title.clone());
    if let Some(b) = &body {
        auto(
            "4",
            "Objectives",
            format!(
                "This review asked: {question} The question was classified as {} and framed with {}; searching used {}. {}",
                serde_json::to_value(b.question_type).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default(),
                serde_json::to_value(b.framework).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default(),
                serde_json::to_value(b.search_framework).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default(),
                b.framework_elements.iter().map(|e| format!("{}: {}.", e.element, e.description)).collect::<Vec<_>>().join(" ")
            ) + " The review is reported following the PRISMA 2020 statement (Page et al., 2021).",
        );
        let crit = |cs: &[protocol::Criterion]| {
            cs.iter()
                .map(|c| {
                    format!(
                        "{} ({}): {}",
                        c.code,
                        match c.testable_at {
                            protocol::TestableAt::TitleAbstract => "title and abstract",
                            protocol::TestableAt::FullText => "full text",
                            protocol::TestableAt::Both => "both stages",
                        },
                        c.text
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        };
        auto(
            "5",
            "Eligibility criteria",
            format!(
                "Inclusion criteria: {}. Exclusion criteria: {}. Publications from {} to {} in {} were eligible. Eligible designs: {}.",
                crit(&b.inclusion_criteria),
                crit(&b.exclusion_criteria),
                b.date_range.from,
                b.date_range.to,
                list(&b.languages),
                list(&b.study_designs)
            ),
        );
    }
    let searched: Vec<String> = current
        .iter()
        .map(|r| format!("{} (searched {})", source_name(&r.source), &r.run_at[..10]))
        .collect();
    let citations = match crate::pipeline::chasing::skipped(conn)? {
        Some(s) if s.records > 0 => format!(
            " Citation searching of included studies in OpenAlex was started but stopped by the review author on {} before screening; the {} records it returned were not screened and are not counted in the flow diagram.",
            &s.at[..10],
            s.records
        ),
        Some(_) => " Citations of included studies were not searched.".to_owned(),
        None if opts.citation_chasing => {
            if current.iter().any(|r| r.source == "citation_chasing") {
                " Backward and forward citations of included studies were identified in OpenAlex."
                    .to_owned()
            } else if d.skips.iter().any(|k| k.stage == "screening" || k.stage == "fulltext") {
                " Citation searching of included studies was planned but not run, because screening was stopped early by the review author.".to_owned()
            } else {
                " Citation searching of included studies was planned but has not run.".to_owned()
            }
        }
        None => String::new(),
    };
    auto(
        "6",
        "Information sources",
        format!("We searched {}.{citations}", list(&searched)),
    );
    let mut strategy = String::new();
    for r in &current {
        if r.source == "citation_chasing" || r.source == "manual_import" {
            continue;
        }
        strategy.push_str(&format!(
            "{} (iteration {}, {} hits): {}\n",
            source_name(&r.source),
            r.iteration,
            r.hits,
            r.query_string
        ));
        for s in &r.simplifications {
            strategy.push_str(&format!("  Note: {s}\n"));
        }
    }
    let broadened: i64 = conn.query_row(
        "SELECT COUNT(*) FROM event WHERE stage = 'search' AND decision = 'broaden'",
        [],
        |r| r.get(0),
    )?;
    if broadened > 0 {
        strategy.push_str(&format!("The search was broadened {broadened} time(s) because known relevant seed studies were missing from the results.\n"));
    }
    strategy.push_str("Full search strategies for every source are reported in Appendix B, following PRISMA-S (Rethlefsen et al., 2021).\n");
    strategy.push_str(stopped("search").trim());
    auto("7", "Search strategy", strategy.trim().to_owned());
    let g2 = d.quality.gate2.clone();
    auto(
        "8",
        "Selection process",
        format!(
            "Two independent LLM screeners assessed each title and abstract against the criteria (Screener A: {}; Screener B: {}), with Screener B instructed to argue for inclusion first. Disagreements and uncertain decisions went to an LLM arbiter ({}); {} Agreement between the two screeners was κ = {}.{} {} Full texts were screened in the same way.{}{}",
            model_for("screener_a"),
            model_for("screener_b"),
            model_for("arbiter"),
            if bypassed {
                "records the arbiter could not decide were settled by the fixed bypass rules listed in Appendix A, not by the review author.".to_owned()
            } else if d.carried_forward > 0 {
                format!("records the arbiter could not decide ({}) were carried forward to full-text screening, and records without a usable agent answer were screened by the review author.", d.carried_forward)
            } else {
                "records the arbiter could not decide were screened by the review author.".to_owned()
            },
            d.quality.kappa_ab.map(|k| format!("{k:.2}")).unwrap_or_else(|| "not available".into()),
            d.prescreen.as_ref().map(|p| format!(" {}", p.note)).unwrap_or_default(),
            match (&g2, d.quality.gate2_outcome.as_deref()) {
                (_, Some("skip")) => "The calibration against a human screener was skipped, so screening performance was not verified.".to_owned(),
                (Some(m), Some(o)) => format!(
                    "The review author screened a blind stratified sample of {} records; estimated recall of the automated screening was {} (95% CI {}) and Cohen's κ against the author was {} (calibration {}).",
                    m.sampled,
                    pct(m.recall),
                    m.recall_ci.map(|(a, b)| format!("{:.0}% to {:.0}%", a * 100.0, b * 100.0)).unwrap_or_else(|| "not available".into()),
                    m.kappa.map(|k| format!("{k:.2}")).unwrap_or_else(|| "not available".into()),
                    if o == "pass" { "passed" } else { "failed" }
                ),
                _ => "Calibration against the review author is not complete.".to_owned(),
            },
            stopped("screening"),
            stopped("fulltext")
        ) + &if bypassed {
            format!(" The review was run in bypass mode: {bypass_decisions} points that would have waited for the review author, including all three verification gates, were settled by fixed rules disclosed in Appendix A, so the automated decisions were not verified by a person.")
        } else {
            String::new()
        },
    );
    auto(
        "9",
        "Data collection process",
        format!(
            "Data were extracted by an LLM ({}) instructed never to infer or calculate values. Every value had to be supported by a verbatim quote and page number; a validator checked each quote against the full text. When a quote was not found, the model was asked once more for the exact passage (the value could not change), and the answer went through the same check; values still unsupported were cleared ({} of {} values verified automatically, {}). {}{}",
            model_for("extraction"),
            d.quality.quotes_checked - (d.quality.quotes_checked as f64 * (1.0 - d.quality.quote_rate.unwrap_or(1.0))).round() as i64,
            d.quality.quotes_checked,
            pct(d.quality.quote_rate),
            if bypassed {
                "Cleared values were not resolved by the review author and were not used in the synthesis."
            } else {
                "Cleared values were resolved by the review author."
            },
            stopped("extraction")
        ),
    );
    if let Some(b) = &body {
        let form: Vec<String> = extraction::form(b)
            .iter()
            .map(|f| f.label.to_lowercase())
            .collect();
        auto(
            "10",
            "Data items",
            format!(
                "We extracted {}{}.",
                list(&form),
                if extraction::wants_findings(b) {
                    ", and each finding relevant to the review question with a supporting quote"
                } else {
                    ""
                }
            ),
        );
    }
    let tools: Vec<String> = {
        let mut t: Vec<String> = st.iter().filter_map(|s| s.tool.clone()).collect();
        t.sort();
        t.dedup();
        t.iter()
            .filter_map(|id| appraisal::tool(id).map(|x| x.name))
            .collect()
    };
    auto(
        "11",
        "Study risk of bias assessment",
        format!(
            "Studies were appraised with {} according to design. Two independent LLM passes ({} and {}) judged each domain with a rationale and supporting quote; disagreements were reconciled by the arbiter ({}).{}",
            if tools.is_empty() { "tools chosen by design".into() } else { list(&tools) },
            model_for("appraisal"),
            model_for("screener_b"),
            model_for("arbiter"),
            stopped("appraisal")
        ),
    );
    let qualitative = body.as_ref().is_some_and(|b| b.is_qualitative());
    auto(
        "12",
        "Effect measures",
        if qualitative {
            "Not applicable: this is a synthesis of qualitative evidence.".into()
        } else {
            "Results were summarised by the direction of effect reported by study authors. No effect measures were pooled.".into()
        },
    );
    let synth_stop = stopped("synthesis");
    auto(
        "13",
        "Synthesis methods",
        if qualitative {
            format!("Findings were synthesised thematically following Thomas and Harden (2008): line-by-line coding of extracted findings, grouping into descriptive themes, and development of analytical themes ({}). Every theme is linked to the verbatim quotes that support it.{synth_stop}", model_for("synthesis"))
        } else {
            format!("Results were grouped by outcome construct and synthesised without meta-analysis following SWiM guidance (Campbell et al., 2020), using vote counting by direction of effect, never by statistical significance ({}).{}{synth_stop}", model_for("synthesis"), d.meta_analysis_note.clone().map(|n| format!(" {n}")).unwrap_or_default())
        },
    );
    auto(
        "14",
        "Reporting bias assessment",
        if qualitative {
            "Not applicable to a qualitative evidence synthesis; dissemination bias was considered within GRADE-CERQual relevance and adequacy.".into()
        } else if p.studies_included >= 10 {
            "Funnel plots and Egger's test require the statistics engine, which was not available; reporting bias was assessed narratively.".into()
        } else {
            "Not assessed statistically: fewer than 10 studies.".into()
        },
    );
    auto(
        "15",
        "Certainty assessment",
        if qualitative {
            format!("Confidence in each review finding was assessed with GRADE-CERQual (Lewin et al., 2018): methodological limitations, coherence, adequacy and relevance. {by_author}")
        } else {
            format!("Certainty of evidence for each outcome was assessed with GRADE (Guyatt et al., 2008). {by_author}")
        },
    );
    let reasons: Vec<String> = p
        .exclusion_reasons
        .iter()
        .map(|(r, n)| format!("{r} (n = {n})"))
        .collect();
    auto(
        "16",
        "Study selection",
        format!(
            "The searches identified {} records; {} duplicates were removed and {} records were screened, of which {} were excluded.{} We sought {} reports, could not retrieve {}, and assessed {} for eligibility. {} reports were excluded{}.{} {} studies ({} reports) were included (Figure 1).",
            p.identified_databases + p.identified_other,
            p.duplicates_removed,
            p.records_screened,
            p.records_excluded,
            if p.records_not_screened > 0 {
                format!(" A further {} records were not screened because screening was stopped early; they did not go forward.", p.records_not_screened)
            } else {
                String::new()
            },
            p.reports_sought,
            p.reports_not_retrieved,
            p.reports_assessed,
            p.reports_excluded,
            if reasons.is_empty() { String::new() } else { format!(": {}", list(&reasons)) },
            if p.reports_awaiting > 0 {
                format!(" {} retrieved reports are awaiting classification because full-text screening was stopped early.", p.reports_awaiting)
            } else {
                String::new()
            },
            p.studies_included,
            p.reports_included
        ),
    );
    auto(
        "17",
        "Study characteristics",
        format!(
            "Characteristics of the {} included studies are shown in Table 1.",
            st.len()
        ),
    );
    auto(
        "18",
        "Risk of bias in studies",
        format!(
            "Appraisal judgements for each study are shown in Table 2 ({} studies appraised).",
            st.iter().filter(|s| s.tool.is_some()).count()
        ),
    );
    auto(
        "19",
        "Results of individual studies",
        if qualitative {
            "The findings extracted from each study, with their supporting quotes, are listed in the supplementary extraction table.".into()
        } else {
            "The results extracted from each study, with statistic type, value and supporting quote, are listed in the supplementary extraction table.".into()
        },
    );
    auto(
        "21",
        "Reporting biases",
        if qualitative {
            "Not applicable (see methods).".into()
        } else {
            "See methods: no statistical assessment was possible.".into()
        },
    );

    // Drafted sections: the author's edit if any, otherwise the draft.
    let mut drafted = |item: &'static str, heading: &'static str, fallback: String| {
        let key = format!("item{item}");
        let edited = text(&key);
        let draft = text(&format!("draft_item{item}"));
        let body = if !edited.trim().is_empty() {
            edited
        } else if !draft.trim().is_empty() {
            draft
        } else {
            fallback
        };
        sections.push(Section {
            item,
            heading,
            kind: Kind::Drafted,
            done: !body.trim().is_empty(),
            text: body,
            key: Some(key),
        });
    };
    drafted("2", "Abstract", String::new());
    drafted("20", "Results of syntheses", String::new());
    drafted(
        "22",
        "Certainty of evidence",
        th.iter()
            .map(|t| {
                format!(
                    "{}: {} confidence{}.",
                    t.label,
                    t.certainty.as_deref().unwrap_or("not assessed"),
                    if t.certainty_confirmed {
                        ""
                    } else {
                        " [CHECK: not yet confirmed]"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    );
    drafted("23", "Discussion", String::new());
    drafted(
        "24",
        "Registration and protocol",
        format!(
            "The protocol was written before searching and locked on {} (version {}). [CHECK: add a registration number, or state that the review was not registered.] Amendments after locking: {}.",
            locked.as_ref().and_then(|l| l.locked_at.clone()).map(|d| d[..10].to_owned()).unwrap_or_else(|| "not locked".into()),
            locked.as_ref().map(|l| l.version).unwrap_or(0),
            conn.query_row("SELECT COUNT(*) FROM protocol_version WHERE version > 1", [], |r| r.get::<_, i64>(0))?
        ),
    );
    drafted(
        "27",
        "Availability of data, code and other materials",
        "The review file, the complete decision log (CSV), extraction data with source quotes, prompts and model identifiers are available from the authors. [CHECK: give a repository link.]".into(),
    );
    for (key, label) in gate3::AUTHOR_ITEMS {
        let item: &'static str = match key {
            "item3_rationale" => "3",
            "item25_support" => "25",
            _ => "26",
        };
        let heading: &'static str = match item {
            "3" => "Rationale",
            "25" => "Support",
            _ => "Competing interests",
        };
        let body = text(key);
        let _ = label;
        sections.push(Section {
            item,
            heading,
            kind: Kind::Author,
            done: !body.trim().is_empty(),
            text: body,
            key: Some(key.to_owned()),
        });
    }
    sections.sort_by_key(|s| s.item.parse::<u32>().unwrap_or(99));

    let mut methods = vec![apa::prisma(), apa::prisma_s()];
    if qualitative {
        methods.extend([apa::thomas_harden(), apa::cerqual()]);
    } else {
        methods.extend([apa::swim(), apa::grade()]);
    }
    let apa_references = apa::reference_list(methods, included);
    let references = apa_references.iter().map(Reference::plain).collect();
    let mut keywords: Vec<String> = body
        .as_ref()
        .map(|b| {
            b.concepts
                .iter()
                .map(|c| {
                    if c.name.chars().filter(|ch| ch.is_uppercase()).count() > 1 {
                        c.name.clone()
                    } else {
                        c.name.to_lowercase()
                    }
                })
                .take(5)
                .collect()
        })
        .unwrap_or_default();
    keywords.push("systematic review".into());
    let swim = synthesis::items(conn)?
        .into_iter()
        .filter(|i| i.kind == "swim_group")
        .map(|i| (i.label, i.body))
        .collect();
    Ok(Manuscript {
        title,
        question,
        verification: d.verification.label.clone(),
        sections,
        studies: st,
        themes: th,
        swim,
        prisma: p,
        references,
        apa_references,
        keywords,
        author_name: None,
        affiliation: None,
        bypass_decisions,
        searches: runs.into_iter().filter(|r| r.current).collect(),
    })
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChecklistSummary {
    pub auto: usize,
    pub drafted: usize,
    pub author: usize,
    pub items: Vec<(String, String, Kind, bool)>,
}

pub fn checklist(m: &Manuscript) -> ChecklistSummary {
    let count = |k: Kind| m.sections.iter().filter(|s| s.kind == k).count();
    ChecklistSummary {
        auto: count(Kind::Auto),
        drafted: count(Kind::Drafted),
        author: count(Kind::Author),
        items: m
            .sections
            .iter()
            .map(|s| (s.item.to_owned(), s.heading.to_owned(), s.kind, s.done))
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Drafting job

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SectionDraft {
    pub text: String,
}

pub struct DraftManuscript;

#[async_trait]
impl JobHandler for DraftManuscript {
    async fn run(&self, _job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        let err = |e: crate::error::Error| JobError::Fatal(e.to_string());
        let facts = facts(&ctx.open().map_err(err)?).map_err(err)?;
        let live = ctx
            .env
            .live
            .track_in(&ctx.review_id, "report", &ctx.review_id, "Manuscript draft");
        let sections = vec![
            ("2", "Abstract: one paragraph of at most 250 words with no headings (APA 7), stating the objective, eligibility criteria, information sources, synthesis method, the number of included studies, the main results and the conclusions"),
            ("20", "Results of syntheses: describe each analytical theme or outcome group and the evidence behind it"),
            ("23", "Discussion: summary of findings, limitations of the evidence and of the review process (including use of LLM agents), implications for practice and research"),
        ];
        // The three sections share the same facts and do not depend on each
        // other, so they are drafted at the same time.
        live.step("Drafting the abstract, results of syntheses and discussion (PRISMA items 2, 20, 23)");
        let replies = par::bounded(sections.clone(), par::LIMIT, |(_, section)| {
            let facts = &facts;
            async move {
                call::<SectionDraft, _>(ctx, Task::Synthesis, "manuscript.v3", &[("section", section.to_owned()), ("facts", facts.clone())], 3000, |d| {
                    if d.text.trim().len() < 40 {
                        Err("section text is too short".into())
                    } else {
                        Ok(())
                    }
                })
                .await
            }
        })
        .await;
        let conn = ctx.open().map_err(err)?;
        for ((item, _), reply) in sections.into_iter().zip(replies) {
            let text = match reply {
                Ok(r) => r.value.text,
                Err(AgentError::NeedsHuman(w)) => format!("[CHECK: the draft could not be generated ({w}). Write this section.]"),
                Err(e) => return Err(e.into()),
            };
            gate3::set_author_text(&conn, &format!("draft_item{item}"), &text).map_err(err)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checklist_split_is_18_auto_6_drafted_3_author() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let rid = crate::reviews::insert_review(&conn, "How do students experience X?").unwrap();
        let body = crate::agents::protocol::tests::sample_protocol();
        conn.execute(
            "INSERT INTO protocol_version (id, review_id, version, body, locked_at) VALUES ('v1', ?1, 1, ?2, '2026-09-24T10:00:00.000Z')",
            rusqlite::params![rid, serde_json::to_string(&body).unwrap()],
        )
        .unwrap();
        let m = build(&conn).unwrap();
        let c = checklist(&m);
        assert_eq!((c.auto, c.drafted, c.author), (18, 6, 3), "{:?}", c.items);
        assert_eq!(m.sections.len(), 27);
        let items: Vec<&str> = m.sections.iter().map(|s| s.item).collect();
        let expected: Vec<String> = (1..=27).map(|n| n.to_string()).collect();
        assert_eq!(
            items,
            expected.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert_eq!(m.title, "How Do Students Experience X: A Systematic Review");
    }

    #[test]
    fn stopped_stages_are_disclosed_in_methods() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let rid = crate::reviews::insert_review(&conn, "How do students experience X?").unwrap();
        let body = crate::agents::protocol::tests::sample_protocol();
        conn.execute(
            "INSERT INTO protocol_version (id, review_id, version, body, locked_at) VALUES ('v1', ?1, 1, ?2, '2026-09-24T10:00:00.000Z')",
            rusqlite::params![rid, serde_json::to_string(&body).unwrap()],
        )
        .unwrap();
        let section = |m: &Manuscript, item: &str| {
            m.sections.iter().find(|s| s.item == item).unwrap().text.clone()
        };
        let before = build(&conn).unwrap();
        assert!(!section(&before, "9").contains("stopped"));

        crate::pipeline::skip::skip(&conn, &rid, "extraction").unwrap();
        crate::pipeline::skip::skip(&conn, &rid, "appraisal").unwrap();
        let m = build(&conn).unwrap();
        assert!(section(&m, "9").contains("Data extraction was stopped by the review author after 0 studies"));
        assert!(section(&m, "11").contains("Appraisal was stopped by the review author"));
        assert!(!section(&m, "13").contains("stopped"));
        assert!(facts(&conn).unwrap().contains("Stage stopped early (data extraction)"));
        let d = super::super::metrics::disclosure(&conn).unwrap();
        assert_eq!(d.skips.len(), 2);
        // The checklist split does not change when stages are stopped.
        let c = checklist(&m);
        assert_eq!((c.auto, c.drafted, c.author), (18, 6, 3));
    }
}
