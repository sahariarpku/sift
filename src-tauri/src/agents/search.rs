//! Search agent (§8.2), deduplication (§8.3) and citation chasing (§8.6).

use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::protocol::{options, require_locked, review_question};
use super::{call, AgentError};
use crate::dedup::{self, Candidate};
use crate::error::{Error, Result};
use crate::events::{self, ActorType, NewEvent};
use crate::jobs::{Job, JobContext, JobError, JobHandler};
use crate::llm::store::Task;
use crate::query::{QueryAst, Rendered};
use crate::records;
use crate::sources::adapter::{NormalisedRecord, SourceAdapter, SourceError};
use crate::sources::{
    core::Core, eric::Eric, openalex::OpenAlex, semantic_scholar::SemanticScholar,
};

/// Sources with a search adapter today.
pub const SEARCHABLE: &[&str] = &["eric", "openalex", "core", "semantic_scholar"];

fn db_err(e: Error) -> JobError {
    JobError::Fatal(e.to_string())
}

fn src_err(e: SourceError) -> JobError {
    match e {
        SourceError::Retryable(m) => JobError::Retryable(m),
        SourceError::Auth(m) => {
            JobError::Pause(format!("{m} Check the key in Settings, then resume."))
        }
        SourceError::Fatal(m) => JobError::Fatal(m),
    }
}

/// Builds the adapter for a source, reading its key from the keychain.
pub fn adapter(
    ctx: &JobContext,
    id: &str,
) -> std::result::Result<Box<dyn SourceAdapter>, JobError> {
    let env = &ctx.env;
    let key = env
        .secrets
        .get(&crate::sources::key_ref(id))
        .map_err(db_err)?;
    let base = env.source_base(id);
    Ok(match id {
        "eric" => Box::new(Eric::new(env.http.clone(), base)),
        "openalex" => Box::new(OpenAlex::new(env.http.clone(), base, key)),
        "semantic_scholar" => Box::new(SemanticScholar::new(env.http.clone(), base, key)),
        "core" => match key {
            Some(k) => Box::new(Core::new(env.http.clone(), base, k)),
            None => return Err(JobError::Pause(
                "CORE needs a key. Add it in Settings, or remove CORE from this review's sources."
                    .into(),
            )),
        },
        other => return Err(JobError::Fatal(format!("no search adapter for {other}"))),
    })
}

// ---------------------------------------------------------------------------
// Search plan: the AST for the current iteration

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchPlan {
    pub iteration: i64,
    pub ast: QueryAst,
}

pub fn plan(conn: &Connection) -> Result<SearchPlan> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM review_setting WHERE key = 'search_plan'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(s) = raw {
        return Ok(serde_json::from_str(&s)?);
    }
    let p = require_locked(conn)?;
    let o = options(conn)?;
    Ok(SearchPlan {
        iteration: 1,
        ast: QueryAst::from_protocol(&p.body, &o),
    })
}

fn save_plan(conn: &Connection, p: &SearchPlan) -> Result<()> {
    conn.execute(
        "INSERT INTO review_setting (key, value) VALUES ('search_plan', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [serde_json::to_string(p)?],
    )?;
    Ok(())
}

/// Renders the current plan for every chosen source (Gate 1 preview and search runs).
pub fn preview(conn: &Connection, http: &reqwest::Client) -> Result<Vec<(String, Rendered)>> {
    let o = options(conn)?;
    let ast = match plan(conn) {
        Ok(p) => p.ast,
        Err(_) => {
            let d = super::protocol::draft(conn)?
                .ok_or_else(|| Error::Invalid("No protocol yet.".into()))?;
            QueryAst::from_protocol(&d.body, &o)
        }
    };
    let mut out = Vec::new();
    for s in &o.sources {
        let r = match s.as_str() {
            "eric" => Eric::new(http.clone(), None).render(&ast),
            "openalex" => OpenAlex::new(http.clone(), None, None).render(&ast),
            "core" => Core::new(http.clone(), None, String::new()).render(&ast),
            "semantic_scholar" => SemanticScholar::new(http.clone(), None, None).render(&ast),
            _ => continue,
        };
        out.push((s.clone(), r));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Running one source

pub const SEARCH_JOB: &str = "search_source";

pub struct SearchSource;

async fn run_query(
    ctx: &JobContext,
    ad: &dyn SourceAdapter,
    rendered: &Rendered,
    cap: u32,
    raw_dir: &std::path::Path,
) -> std::result::Result<(u64, Vec<NormalisedRecord>, bool), JobError> {
    std::fs::create_dir_all(raw_dir).map_err(|e| JobError::Fatal(e.to_string()))?;
    let mut cursor: Option<String> = None;
    let mut all = Vec::new();
    let mut total = 0;
    let mut page_no = 0;
    loop {
        let page = ad
            .fetch_page(rendered, cursor.clone())
            .await
            .map_err(src_err)?;
        page_no += 1;
        let _ = std::fs::write(
            raw_dir.join(format!("page-{page_no:04}.json")),
            page.raw.to_string(),
        );
        total = page.total.unwrap_or(total);
        let recs = ad.normalise(&page.raw);
        let got = recs.len();
        all.extend(recs);
        if all.len() >= cap as usize {
            all.truncate(cap as usize);
            return Ok((total, all, (total as usize) > cap as usize));
        }
        match page.next_cursor {
            Some(c) if got > 0 => cursor = Some(c),
            _ => break,
        }
        let _ = ctx;
    }
    Ok((total, all, false))
}

#[async_trait]
impl JobHandler for SearchSource {
    async fn run(&self, job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        let source = job.payload["source"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let (review_id, plan, cap) = {
            let conn = ctx.open().map_err(db_err)?;
            let (rid, _) = review_question(&conn).map_err(db_err)?;
            (
                rid,
                plan(&conn).map_err(db_err)?,
                options(&conn).map_err(db_err)?.max_records_per_source,
            )
        };
        let iteration = job.payload["iteration"].as_i64().unwrap_or(plan.iteration);
        let ad = adapter(ctx, &source)?;
        let mut rendered = ad.render(&plan.ast);
        let raw_dir = ctx
            .data_dir()
            .join("raw")
            .join(format!("{source}-it{iteration}-{}", job.id));
        let (hits, recs, capped) = run_query(ctx, ad.as_ref(), &rendered, cap, &raw_dir).await?;
        if capped {
            rendered.simplifications.push(format!(
                "Stopped after the first {cap} of {hits} records (the per-source limit set for this review)."
            ));
        }
        let conn = ctx.open().map_err(db_err)?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| JobError::Fatal(e.to_string()))?;
        let run_id = records::insert_run(
            &tx,
            &review_id,
            &source,
            &rendered,
            &plan.ast,
            iteration,
            hits,
            raw_dir.to_str(),
        )
        .map_err(db_err)?;
        let ids = records::insert_records(&tx, &run_id, &recs).map_err(db_err)?;
        events::append(
            &tx,
            &NewEvent {
                review_id,
                target_type: "search_run".into(),
                target_id: run_id,
                stage: "search".into(),
                actor_type: Some(ActorType::Agent),
                actor_id: "search_agent".into(),
                decision: "run".into(),
                rationale: Some(format!(
                    "{source}, iteration {iteration}: {hits} hits, {} records stored.",
                    ids.len()
                )),
                ..Default::default()
            },
        )
        .map_err(db_err)?;
        tx.commit().map_err(|e| JobError::Fatal(e.to_string()))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Seed set (§8.2 step 2): a narrow search whose top results both screeners confirm.

pub const SEED_SEARCH_JOB: &str = "seed_search";
pub const SEED_CHECK_JOB: &str = "seed_check";
pub const BROADEN_JOB: &str = "search_broaden";
const SEED_CANDIDATES: usize = 10;

pub struct SeedSearch;

#[async_trait]
impl JobHandler for SeedSearch {
    async fn run(&self, _job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        let (review_id, plan, sources) = {
            let conn = ctx.open().map_err(db_err)?;
            (
                review_question(&conn).map_err(db_err)?.0,
                plan(&conn).map_err(db_err)?,
                options(&conn).map_err(db_err)?.sources,
            )
        };
        // §8.2 names OpenAlex; fall back to the first other chosen source.
        let source = ["openalex", "eric", "core", "semantic_scholar"]
            .into_iter()
            .find(|s| sources.iter().any(|x| x == s))
            .unwrap_or("eric");
        let ad = adapter(ctx, source)?;
        let narrow = plan.ast.narrow();
        let rendered = ad.render(&narrow);
        let page = ad.fetch_page(&rendered, None).await.map_err(src_err)?;
        let recs: Vec<NormalisedRecord> = ad
            .normalise(&page.raw)
            .into_iter()
            .take(SEED_CANDIDATES)
            .collect();
        let conn = ctx.open().map_err(db_err)?;
        let run_id = records::insert_run(
            &conn,
            &review_id,
            &format!("seed:{source}"),
            &rendered,
            &narrow,
            0,
            page.total.unwrap_or(0),
            None,
        )
        .map_err(db_err)?;
        let ids = records::insert_records(&conn, &run_id, &recs).map_err(db_err)?;
        for id in &ids {
            crate::jobs::enqueue_once(
                &conn,
                &review_id,
                super::screening::SEED_JOB,
                &json!({"record_id": id}),
            )
            .map_err(db_err)?;
        }
        events::append(
            &conn,
            &NewEvent {
                review_id,
                target_type: "search_run".into(),
                target_id: run_id,
                stage: "search".into(),
                actor_type: Some(ActorType::Agent),
                actor_id: "search_agent".into(),
                decision: "seed_candidates".into(),
                rationale: Some(format!(
                    "{} seed candidates from a narrow {source} search.",
                    ids.len()
                )),
                ..Default::default()
            },
        )
        .map_err(db_err)?;
        Ok(())
    }
}

/// Seed candidates both screeners included.
pub fn seed_set(conn: &Connection) -> Result<Vec<records::RecordRow>> {
    let all = records::records_where(conn, "run.source LIKE 'seed:%'")?;
    let mut out = Vec::new();
    for r in all {
        let a = super::screening::latest_by(conn, "search", &r.id, "screener_a")?;
        let b = super::screening::latest_by(conn, "search", &r.id, "screener_b")?;
        if a.is_some_and(|e| e.decision == "include") && b.is_some_and(|e| e.decision == "include")
        {
            out.push(r);
        }
    }
    Ok(out)
}

fn same_work(a: &records::RecordRow, b: &records::RecordRow) -> bool {
    if let (Some(x), Some(y)) = (&a.doi, &b.doi) {
        return x == y;
    }
    match (&a.title, &b.title) {
        (Some(x), Some(y)) => {
            strsim::jaro_winkler(&dedup::normalise_title(x), &dedup::normalise_title(y)) >= 0.95
        }
        _ => false,
    }
}

/// Seed papers missing from the current search results.
pub fn missing_seeds(conn: &Connection) -> Result<(usize, Vec<records::RecordRow>)> {
    let seeds = seed_set(conn)?;
    let found = records::records_where(conn, "r.search_run_id IN (SELECT id FROM v_current_run)")?;
    let missing: Vec<records::RecordRow> = seeds
        .iter()
        .filter(|s| !found.iter().any(|f| same_work(s, f)))
        .cloned()
        .collect();
    Ok((seeds.len(), missing))
}

pub struct SeedCheck;

#[async_trait]
impl JobHandler for SeedCheck {
    async fn run(&self, _job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        let conn = ctx.open().map_err(db_err)?;
        let (review_id, _) = review_question(&conn).map_err(db_err)?;
        let (total, missing) = missing_seeds(&conn).map_err(db_err)?;
        let plan = plan(&conn).map_err(db_err)?;
        let found = total - missing.len();
        let pass = missing.is_empty();
        let decision = if pass {
            "seed_check_pass"
        } else if plan.iteration >= 3 {
            "seed_check_fail"
        } else {
            "seed_check_broaden"
        };
        events::append(
            &conn,
            &NewEvent {
                review_id: review_id.clone(),
                target_type: "protocol".into(),
                target_id: review_id.clone(),
                stage: "search".into(),
                actor_type: Some(ActorType::Agent),
                actor_id: "search_agent".into(),
                decision: decision.into(),
                reason_code: Some(plan.iteration.to_string()),
                confidence: (total > 0).then(|| found as f64 / total as f64),
                rationale: Some(format!(
                    "Iteration {}: {found} of {total} seed studies found.{}",
                    plan.iteration,
                    if missing.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " Missing: {}",
                            missing
                                .iter()
                                .filter_map(|m| m.title.clone())
                                .collect::<Vec<_>>()
                                .join(" | ")
                        )
                    }
                )),
                ..Default::default()
            },
        )
        .map_err(db_err)?;
        if decision == "seed_check_broaden" {
            crate::jobs::enqueue_once(
                &conn,
                &review_id,
                BROADEN_JOB,
                &json!({"iteration": plan.iteration + 1}),
            )
            .map_err(db_err)?;
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BroadenOut {
    pub additions: Vec<ConceptAddition>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConceptAddition {
    pub concept: String,
    pub terms: Vec<String>,
}

pub struct Broaden;

#[async_trait]
impl JobHandler for Broaden {
    async fn run(&self, job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        let (review_id, mut plan, missing) = {
            let conn = ctx.open().map_err(db_err)?;
            (
                review_question(&conn).map_err(db_err)?.0,
                plan(&conn).map_err(db_err)?,
                missing_seeds(&conn).map_err(db_err)?.1,
            )
        };
        let concepts = plan
            .ast
            .concepts
            .iter()
            .map(|c| {
                format!(
                    "- {}: {}",
                    c.name,
                    c.terms
                        .iter()
                        .map(|t| t.text.clone())
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let missed = missing
            .iter()
            .map(|m| m.screening_text())
            .collect::<Vec<_>>()
            .join("\n\n");
        let names: Vec<String> = plan
            .ast
            .concepts
            .iter()
            .map(|c| c.name.to_lowercase())
            .collect();
        let reply = call::<BroadenOut, _>(
            ctx,
            Task::ProtocolSearch,
            "search_broaden.v1",
            &[("concepts", concepts), ("missed", missed)],
            1500,
            |o| {
                if o.additions
                    .iter()
                    .any(|a| !names.contains(&a.concept.to_lowercase()))
                {
                    Err(format!(
                        "concept names must be one of: {}",
                        names.join(", ")
                    ))
                } else {
                    Ok(())
                }
            },
        )
        .await
        .map_err(|e| match e {
            AgentError::NeedsHuman(m) => JobError::Pause(format!("Search broadening failed: {m}")),
            other => other.into(),
        })?;
        let additions: Vec<(String, Vec<String>)> = reply
            .value
            .additions
            .iter()
            .map(|a| (a.concept.clone(), a.terms.clone()))
            .collect();
        let added = plan.ast.broaden(&additions);
        let next = job.payload["iteration"]
            .as_i64()
            .unwrap_or(plan.iteration + 1);
        let conn = ctx.open().map_err(db_err)?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| JobError::Fatal(e.to_string()))?;
        // Earlier runs are replaced by the rerun.
        let old: Vec<String> = {
            let mut s = tx
                .prepare("SELECT id FROM v_current_run")
                .map_err(|e| JobError::Fatal(e.to_string()))?;
            let rows = s
                .query_map([], |r| r.get(0))
                .map_err(|e| JobError::Fatal(e.to_string()))?;
            rows.collect::<rusqlite::Result<Vec<String>>>()
                .map_err(|e| JobError::Fatal(e.to_string()))?
        };
        for id in old {
            events::append(
                &tx,
                &NewEvent {
                    review_id: review_id.clone(),
                    target_type: "search_run".into(),
                    target_id: id,
                    stage: "search".into(),
                    actor_type: Some(ActorType::Agent),
                    actor_id: "search_agent".into(),
                    decision: "superseded".into(),
                    rationale: Some(format!("Replaced by broadened search, iteration {next}.")),
                    ..Default::default()
                },
            )
            .map_err(db_err)?;
        }
        plan.iteration = next;
        save_plan(&tx, &plan).map_err(db_err)?;
        let mut ev = reply.meta.event(
            &review_id,
            "protocol",
            &review_id,
            "search",
            "search_agent",
            "broaden",
        );
        ev.rationale = Some(format!(
            "Added {added} terms for iteration {next}: {}",
            serde_json::to_string(&additions).unwrap_or_default()
        ));
        events::append(&tx, &ev).map_err(db_err)?;
        tx.commit().map_err(|e| JobError::Fatal(e.to_string()))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Deduplication

pub const DEDUP_JOB: &str = "dedup";

pub struct Dedup;

/// Marks duplicates among current records not yet deduplicated. Records already
/// judged keep their status; new records (citation chasing) are compared to all.
pub fn run_dedup(conn: &Connection) -> Result<(usize, usize)> {
    let (review_id, _) = review_question(conn)?;
    let recs = records::records_where(conn, "r.search_run_id IN (SELECT id FROM v_current_run)")?;
    let order: std::collections::HashMap<String, i64> = {
        let mut s = conn.prepare("SELECT id, rowid FROM record")?;
        let rows = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let already: std::collections::HashSet<String> = {
        let mut s = conn.prepare("SELECT record_id FROM v_duplicate")?;
        let rows = s.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let candidates: Vec<Candidate> = recs
        .iter()
        .filter(|r| !already.contains(&r.id))
        .map(|r| Candidate {
            id: r.id.clone(),
            doi: r.doi.clone(),
            title: r.title.clone(),
            year: r.year,
            first_author: r.authors.first().cloned(),
            order: order.get(&r.id).copied().unwrap_or(i64::MAX),
        })
        .collect();
    let out = dedup::find(candidates);
    let tx = conn.unchecked_transaction()?;
    let judged: std::collections::HashSet<String> = {
        let mut s = tx.prepare("SELECT target_id FROM v_current_event WHERE stage = 'dedup' AND decision IN ('duplicate', 'possible_duplicate')")?;
        let rows = s.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut merged = 0;
    for m in &out.merges {
        tx.execute(
            "UPDATE record SET duplicate_of = ?1 WHERE id = ?2",
            params![m.of, m.duplicate],
        )?;
        events::append(
            &tx,
            &NewEvent {
                review_id: review_id.clone(),
                target_type: "record".into(),
                target_id: m.duplicate.clone(),
                stage: "dedup".into(),
                actor_type: Some(ActorType::Agent),
                actor_id: "dedup".into(),
                decision: "duplicate".into(),
                reason_code: Some(match m.rule {
                    dedup::MergeRule::Doi => "DOI".into(),
                    dedup::MergeRule::TitleYearAuthor => "TITLE_YEAR_AUTHOR".into(),
                }),
                rationale: Some(m.of.clone()),
                confidence: Some(m.similarity),
                ..Default::default()
            },
        )?;
        merged += 1;
    }
    let mut borderline = 0;
    for (a, b, sim) in &out.borderline {
        if judged.contains(b) {
            continue;
        }
        events::append(
            &tx,
            &NewEvent {
                review_id: review_id.clone(),
                target_type: "record".into(),
                target_id: b.clone(),
                stage: "dedup".into(),
                actor_type: Some(ActorType::Agent),
                actor_id: "dedup".into(),
                decision: "possible_duplicate".into(),
                rationale: Some(a.clone()),
                confidence: Some(*sim),
                ..Default::default()
            },
        )?;
        borderline += 1;
    }
    events::append(
        &tx,
        &NewEvent {
            review_id: review_id.clone(),
            target_type: "protocol".into(),
            target_id: review_id,
            stage: "dedup".into(),
            actor_type: Some(ActorType::Agent),
            actor_id: "dedup".into(),
            decision: "complete".into(),
            rationale: Some(format!("{merged} duplicates removed; {borderline} possible duplicates listed for checking.")),
            ..Default::default()
        },
    )?;
    tx.commit()?;
    Ok((merged, borderline))
}

#[async_trait]
impl JobHandler for Dedup {
    async fn run(&self, _job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        let conn = ctx.open().map_err(db_err)?;
        run_dedup(&conn).map_err(db_err)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Citation chasing (§8.6)

pub const CITATION_JOB: &str = "citation_chasing";
const MAX_CITING_PER_STUDY: usize = 200;

pub struct CitationChasing;

#[async_trait]
impl JobHandler for CitationChasing {
    async fn run(&self, _job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        let (review_id, included, filters) = {
            let conn = ctx.open().map_err(db_err)?;
            let rid = review_question(&conn).map_err(db_err)?.0;
            let inc = records::records_where(
                &conn,
                "r.id IN (SELECT rep.record_id FROM v_included_report rep)",
            )
            .map_err(db_err)?;
            (rid, inc, plan(&conn).map_err(db_err)?.ast.filters)
        };
        let key = ctx
            .env
            .secrets
            .get(&crate::sources::key_ref("openalex"))
            .map_err(db_err)?;
        let oa = OpenAlex::new(ctx.env.http.clone(), ctx.env.source_base("openalex"), key);
        let mut found: Vec<Value> = Vec::new();
        let mut looked_up = 0;
        for rec in &included {
            let work = match (&rec.doi, rec.source.as_str(), &rec.source_id) {
                (Some(d), _, _) => oa.by_doi(d).await.map_err(src_err)?,
                (None, "openalex", Some(id)) => oa
                    .by_ids(std::slice::from_ref(id))
                    .await
                    .map_err(src_err)?
                    .into_iter()
                    .next(),
                _ => None,
            };
            let Some(work) = work else { continue };
            looked_up += 1;
            let refs: Vec<String> = work["referenced_works"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            found.extend(oa.by_ids(&refs).await.map_err(src_err)?);
            if let Some(id) = work["id"].as_str() {
                let short = id.rsplit('/').next().unwrap_or(id);
                found.extend(
                    oa.citing(short, MAX_CITING_PER_STUDY)
                        .await
                        .map_err(src_err)?,
                );
            }
        }
        // Keep the review's date range for chased records too.
        let recs: Vec<NormalisedRecord> = found
            .iter()
            .map(OpenAlex::normalise_work)
            .filter(|r| match (r.year, filters.year_from, filters.year_to) {
                (Some(y), Some(a), Some(b)) => y >= a && y <= b,
                _ => true,
            })
            .collect();
        let mut seen = std::collections::HashSet::new();
        let recs: Vec<NormalisedRecord> = recs
            .into_iter()
            .filter(|r| seen.insert(r.source_id.clone()))
            .collect();
        let conn = ctx.open().map_err(db_err)?;
        let rendered = Rendered {
            query: format!(
                "Backward and forward citations of {looked_up} included studies (OpenAlex)"
            ),
            params: Vec::new(),
            simplifications: vec![
                format!("Forward citations capped at {MAX_CITING_PER_STUDY} per study."),
                "Studies without a DOI or OpenAlex record were not chased.".into(),
            ],
        };
        let empty = QueryAst {
            concepts: Vec::new(),
            filters,
            notes: Vec::new(),
        };
        let run_id = records::insert_run(
            &conn,
            &review_id,
            "citation_chasing",
            &rendered,
            &empty,
            1,
            recs.len() as u64,
            None,
        )
        .map_err(db_err)?;
        let ids = records::insert_records(&conn, &run_id, &recs).map_err(db_err)?;
        events::append(
            &conn,
            &NewEvent {
                review_id,
                target_type: "search_run".into(),
                target_id: run_id,
                stage: "search".into(),
                actor_type: Some(ActorType::Agent),
                actor_id: "search_agent".into(),
                decision: "citation_chasing".into(),
                rationale: Some(format!(
                    "{} records from citations of {looked_up} of {} included studies.",
                    ids.len(),
                    included.len()
                )),
                ..Default::default()
            },
        )
        .map_err(db_err)?;
        run_dedup(&conn).map_err(db_err)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Manual import (RIS, BibTeX, NBIB)

pub fn import(
    conn: &Connection,
    label: &str,
    format: crate::sources::import::Format,
    text: &str,
) -> Result<usize> {
    let (review_id, _) = review_question(conn)?;
    let recs = crate::sources::import::parse(format, text);
    if recs.is_empty() {
        return Err(Error::Invalid("No records found in the file.".into()));
    }
    let rendered = Rendered {
        query: format!("Imported from {label}"),
        params: Vec::new(),
        simplifications: Vec::new(),
    };
    let empty = QueryAst {
        concepts: Vec::new(),
        filters: Default::default(),
        notes: Vec::new(),
    };
    let run_id = records::insert_run(
        conn,
        &review_id,
        "manual_import",
        &rendered,
        &empty,
        1,
        recs.len() as u64,
        None,
    )?;
    let ids = records::insert_records(conn, &run_id, &recs)?;
    events::append(
        conn,
        &NewEvent {
            review_id,
            target_type: "search_run".into(),
            target_id: run_id,
            stage: "search".into(),
            actor_type: Some(ActorType::Human),
            actor_id: "human".into(),
            decision: "import".into(),
            rationale: Some(format!("{} records imported from {label}.", ids.len())),
            ..Default::default()
        },
    )?;
    run_dedup(conn)?;
    Ok(ids.len())
}
