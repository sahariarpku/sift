//! End-to-end fixture review: every stage and gate, with mocked sources and a
//! mocked model that answers each prompt type. Checks that the pipeline
//! reaches sign-off and that PRISMA counts match the event log.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::agents::protocol::{self, tests::sample_protocol, ReviewOptions};
use crate::jobs::{Registry, Worker};
use crate::llm::store::{self, ProviderRecord, Task};
use crate::llm::{Protocol, ProviderOptions};
use crate::pipeline::env::Env;
use crate::pipeline::{self, gate2, gate3, retrieval, Halt};
use crate::records;
use crate::secrets::{MemoryStore, SecretStore};
use crate::test_http::{MockServer, Recorded, Reply};

const PAGE1: &str = "We interviewed 12 Chinese international students at a university in England. The study used semi-structured interviews and thematic analysis.";
const PAGE2: &str = "Students described feeling lonely in seminars. Many students said friendships with home students were hard to form.";
const PAGE3: &str = "Discussion. Universities could create more structured opportunities for intercultural contact across the first year of study.";

fn chat(content: Value) -> Reply {
    Reply::json(
        200,
        json!({
            "model": "mock-model",
            "choices": [{"finish_reason": "stop", "message": {"role": "assistant", "content": content.to_string()}}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 20}
        })
        .to_string(),
    )
}

fn ids_after(text: &str, prefix: char) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some((head, _)) = line.split_once(':') {
            if head.starts_with(prefix)
                && head[1..].chars().all(|c| c.is_ascii_digit())
                && head.len() > 1
            {
                out.push(head.to_owned());
            }
        }
    }
    out
}

/// A mock model that answers each prompt type with valid, checkable output.
fn model(req: &Recorded) -> Reply {
    let body = req.json();
    let msgs = body["messages"].as_array().cloned().unwrap_or_default();
    let system = msgs
        .iter()
        .find(|m| m["role"] == "system")
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
        .to_owned();
    let user = msgs
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
        .to_owned();
    let title = user
        .lines()
        .find_map(|l| l.strip_prefix("Title: "))
        .unwrap_or("")
        .to_owned();
    let relevant = user.contains("Chinese");
    let screen = |extra: Value| {
        let mut v = json!({
            "criteria": [{"code": "IN-POP", "judgement": if relevant { "met" } else { "failed" }}],
            "decision": if relevant { "include" } else { "exclude" },
            "reason_code": if relevant { Value::Null } else { json!("EX-POP") },
            "rationale": "Judged against the criteria.",
            "evidence_quote": title,
            "confidence": 0.9
        });
        if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            for (k, x) in e {
                o.insert(k.clone(), x.clone());
            }
        }
        chat(v)
    };
    if system.contains("drafting the protocol") {
        chat(serde_json::to_value(sample_protocol()).unwrap())
    } else if system.contains("You are screening records") {
        screen(json!({}))
    } else if system.contains("second, independent screener") {
        screen(json!({"argument_for_inclusion": "Possibly relevant."}))
    } else if system.contains("screening full-text reports")
        || system.contains("second, independent full-text screener")
    {
        chat(json!({
            "argument_for_inclusion": "Relevant population.",
            "criteria": [{"code": "IN-POP", "judgement": "met"}],
            "decision": "include", "reason_code": null, "rationale": "Meets all criteria.",
            "evidence_quote": "We interviewed 12 Chinese international students", "evidence_page": 1, "confidence": 0.95
        }))
    } else if system.contains("arbiter for a systematic review") {
        chat(json!({
            "decision": "include", "reason_code": null, "rationale": "Arbiter.",
            "evidence_quote": if title.is_empty() { "We interviewed 12 Chinese international students".to_owned() } else { title.clone() },
            "evidence_page": 1, "confidence": 0.8
        }))
    } else if system.contains("Extract the fields") {
        chat(json!({
            "fields": [
                {"field": "sample_size", "value": "12", "source_quote": "We interviewed 12 Chinese international students", "page": 1},
                {"field": "country", "value": "England", "source_quote": "at a university in England", "page": 1},
                {"field": "design", "value": "Semi-structured interviews, thematic analysis", "source_quote": "The study used semi-structured interviews and thematic analysis", "page": 1},
                {"field": "setting", "value": "Three universities", "source_quote": "at three universities in England", "page": 1}
            ],
            "findings": [
                {"finding": "Loneliness in seminars", "source_quote": "Students described feeling lonely in seminars", "page": 2},
                {"finding": "Hard to befriend home students", "source_quote": "friendships with home students were hard to form", "page": 2}
            ],
            "outcomes": []
        }))
    } else if system.contains("appraising the methodological quality") {
        let tool = crate::agents::appraisal::tool("CASP").unwrap();
        chat(json!({"domains": tool.domains.iter().map(|d| json!({
            "domain": d, "judgement": "Yes", "rationale": "Reported clearly.", "source_quote": null, "page": null
        })).collect::<Vec<_>>()}))
    } else if system.contains("line-by-line coding") {
        let f = ids_after(&user, 'F');
        chat(
            json!({"codes": f.iter().map(|id| json!({"code": format!("Code for {id}"), "finding_ids": [id]})).collect::<Vec<_>>()}),
        )
    } else if system.contains("developing descriptive themes") {
        chat(
            json!({"themes": [{"label": "Isolation in academic spaces", "description": "Feeling alone in class.", "code_ids": ids_after(&user, 'C')}]}),
        )
    } else if system.contains("generating analytical themes") {
        chat(
            json!({"themes": [{"label": "Belonging is built outside class", "statement": "Students found belonging through peers rather than teaching.", "descriptive_theme_ids": ids_after(&user, 'D')}]}),
        )
    } else if system.contains("draft one section of a systematic review manuscript") {
        chat(
            json!({"text": "Two studies of Chinese international students in England were included. Both described loneliness in seminars."}),
        )
    } else if system.contains("GRADE-CERQual") {
        chat(json!({
            "methodological_limitations": "minor concerns", "methodological_limitations_note": "n",
            "coherence": "no or very minor concerns", "coherence_note": "n",
            "adequacy": "moderate concerns", "adequacy_note": "Two studies.",
            "relevance": "no or very minor concerns", "relevance_note": "n",
            "overall": "moderate", "explanation": "Two small studies."
        }))
    } else {
        Reply::json(400, json!({"error": {"message": format!("mock: unknown prompt: {}", &system.chars().take(90).collect::<String>())}}).to_string())
    }
}

fn eric(_: &Recorded) -> Reply {
    let doc = |id: &str, title: &str, author: &str, year: i32, abs: &str| json!({"id": id, "title": title, "author": [author], "publicationdateyear": year, "description": abs, "e_fulltextauth": 0});
    let docs = vec![
        doc(
            "EJ1",
            "Chinese international students and belonging in English universities",
            "Li, Mei",
            2019,
            "We interviewed Chinese international students in England about belonging.",
        ),
        doc(
            "EJ2",
            "Loneliness among Chinese postgraduates in the UK",
            "Wang, Jun",
            2021,
            "A qualitative study of Chinese postgraduate students in the United Kingdom.",
        ),
        doc(
            "EJ3",
            "Mathematics anxiety in primary schools",
            "Smith, Anne",
            2018,
            "Survey of pupils.",
        ),
        doc(
            "EJ4",
            "Chinese International Students and Belonging in English Universities.",
            "Mei Li",
            2019,
            "Interviews in England.",
        ),
    ];
    Reply::json(
        200,
        json!({"response": {"numFound": docs.len(), "start": 0, "docs": docs}}).to_string(),
    )
}

fn fake_pdf() -> Vec<u8> {
    b"%PDF-1.4 fixture".to_vec()
}

/// Mock servers (kept alive), a worker and the review file for a fixture
/// review with ERIC only, seed check on and citation chasing off.
struct Fixture {
    _llm: MockServer,
    _eric: MockServer,
    worker: Worker,
    path: std::path::PathBuf,
    id: String,
}

async fn fixture(dir: &std::path::Path) -> Fixture {
    let llm = MockServer::handler(Arc::new(model)).await;
    let eric_mock = MockServer::handler(Arc::new(eric)).await;

    // App settings: one provider for every task.
    let app_path = dir.join("app.db");
    let app = crate::db::open_app(&app_path).unwrap();
    store::upsert(
        &app,
        &ProviderRecord {
            id: "mock".into(),
            preset: "custom".into(),
            display_name: "Mock".into(),
            protocol: Protocol::OpenaiCompat,
            base_url: format!("{}/v1", llm.base()),
            api_key_ref: Some("provider:mock".into()),
            models: vec![],
            data_policy_warning: false,
            concurrency: 4,
            options: ProviderOptions::default(),
            last_test_status: None,
            last_test_message: None,
            last_test_at: None,
        },
    )
    .unwrap();
    for t in Task::ALL {
        store::set_assignment(&app, t, Some(("mock", "mock-model"))).unwrap();
    }
    let secrets = Arc::new(MemoryStore::default());
    secrets.set("provider:mock", "mock-key-0000000000").unwrap();
    let env = Env::detached_with(
        app_path,
        secrets,
        HashMap::from([("eric".to_owned(), eric_mock.base().to_owned())]),
    );

    // A review with ERIC only, seed check on, citation chasing off.
    let summary = crate::reviews::create(
        &app,
        &dir.join("reviews"),
        "How do Chinese international students experience UK universities?",
    )
    .unwrap();
    let path = std::path::PathBuf::from(&summary.path);
    {
        let conn = crate::db::open_review(&path).unwrap();
        let opts = ReviewOptions {
            sources: vec!["eric".into()],
            citation_chasing: false,
            ..Default::default()
        };
        protocol::save_options(&conn, &opts).unwrap();
    }
    let worker = Worker::new(
        path.clone(),
        summary.id.clone(),
        Arc::new(Registry::standard()),
        env,
    );
    Fixture {
        _llm: llm,
        _eric: eric_mock,
        worker,
        path,
        id: summary.id,
    }
}

fn failed_jobs(conn: &rusqlite::Connection) -> Vec<(String, Option<String>)> {
    crate::jobs::list(conn)
        .unwrap()
        .into_iter()
        .filter(|j| j.status == crate::jobs::JobStatus::Failed)
        .map(|j| (j.kind, j.last_error))
        .collect()
}

#[tokio::test]
async fn fixture_review_runs_every_stage_to_sign_off() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path()).await;
    let (worker, path) = (&f.worker, &f.path);
    let conn = || crate::db::open_review(path).unwrap();
    let halt = || pipeline::advance(&conn(), &f.id).unwrap();

    // Protocol draft, then Gate 1.
    worker.drain().await.unwrap();
    let failed: Vec<(String, Option<String>)> = crate::jobs::list(&conn())
        .unwrap()
        .into_iter()
        .filter(|j| j.status == crate::jobs::JobStatus::Failed)
        .map(|j| (j.kind, j.last_error))
        .collect();
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(
        halt(),
        Halt::Waiting("Gate 1: review and lock the protocol".into())
    );
    protocol::lock(&conn(), true).unwrap();

    // Seed set, search, seed check, dedup, screening, up to Gate 2.
    worker.drain().await.unwrap();
    assert_eq!(
        halt(),
        Halt::Waiting("Gate 2: screen the calibration sample".into())
    );
    assert!(!crate::agents::search::seed_set(&conn()).unwrap().is_empty());
    let seed_pass: bool = conn()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM event WHERE decision = 'seed_check_pass')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(seed_pass, "both seed studies are in the search results");

    // Gate 2: the person agrees with the agents.
    for (id, stratum) in gate2::sample(&conn()).unwrap() {
        let reason = (stratum == "exclude").then_some("EX-POP");
        gate2::judge(&conn(), &id, &stratum, reason).unwrap();
    }
    let m = gate2::finish(&conn()).unwrap();
    assert!(m.passed, "{m:?}");
    assert_eq!(m.kappa, Some(1.0));
    assert_eq!(m.recall, Some(1.0));

    // Retrieval finds nothing open access: both reports wait for upload.
    worker.drain().await.unwrap();
    let waiting = retrieval::upload_queue(&conn()).unwrap();
    assert_eq!(waiting.len(), 2);
    let data_dir = path.with_file_name(format!(
        "{}.files",
        path.file_stem().unwrap().to_string_lossy()
    ));
    for rep in &waiting {
        retrieval::upload(&conn(), &data_dir, rep, &fake_pdf()).unwrap();
        // What pdf.js in the app window does.
        let pages = vec![PAGE1.to_owned(), PAGE2.to_owned(), PAGE3.to_owned()];
        assert!(retrieval::store_page_texts(&conn(), rep, &pages).unwrap());
    }

    // Full-text screening, linking, extraction, appraisal, synthesis.
    worker.drain().await.unwrap();
    let failed: Vec<(String, Option<String>)> = crate::jobs::list(&conn())
        .unwrap()
        .into_iter()
        .filter(|j| j.status == crate::jobs::JobStatus::Failed)
        .map(|j| (j.kind, j.last_error))
        .collect();
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(
        halt(),
        Halt::Waiting("Gate 3: resolve flags, write items 3, 25 and 26, then sign off".into())
    );

    // PRISMA counts come from the event log.
    let p = records::prisma(&conn()).unwrap();
    assert_eq!(p.identified_databases, 4);
    assert_eq!(p.duplicates_removed, 1);
    assert_eq!(p.records_screened, 3);
    assert_eq!(p.records_excluded, 1);
    assert_eq!(p.reports_sought, 2);
    assert_eq!(p.reports_not_retrieved, 0);
    assert_eq!(p.reports_assessed, 2);
    assert_eq!(p.reports_excluded, 0);
    assert_eq!(p.studies_included, 2);
    assert_eq!(p.reports_included, 2);
    assert_eq!(p.by_source, vec![("eric".to_owned(), 4)]);

    // The invented "three universities" quote was voided and flagged.
    let r = gate3::requirements(&conn()).unwrap();
    assert_eq!(r.flagged_values, 2);
    assert!(!r.ready);
    let flagged: Vec<String> = crate::agents::extraction::values(&conn(), None)
        .unwrap()
        .into_iter()
        .filter(|v| v.validation == "quote_not_found")
        .map(|v| v.id)
        .collect();
    for id in flagged {
        assert!(crate::agents::extraction::resolve(
            &conn(),
            &id,
            Some("One university"),
            Some("made-up quote"),
            Some(1)
        )
        .is_err());
        crate::agents::extraction::resolve(
            &conn(),
            &id,
            Some("One university"),
            Some("at a university in England"),
            Some(1),
        )
        .unwrap();
    }

    // Synthesis traced to quotes; certainty drafted.
    let items = crate::agents::synthesis::items(&conn()).unwrap();
    let analytical = items.iter().find(|i| i.kind == "analytical_theme").unwrap();
    assert_eq!(
        crate::agents::synthesis::supporting_values(&items, &analytical.id).len(),
        4
    );
    let cerqual = items.iter().find(|i| i.kind == "cerqual").unwrap();
    assert_eq!(cerqual.body["studies"], 2);

    // Appraisal used CASP (qualitative design), reconciled without the arbiter.
    let appraisals = crate::agents::appraisal::rows(&conn()).unwrap();
    assert_eq!(appraisals.len(), 20);
    assert!(appraisals.iter().all(|a| a.tool == "CASP"));

    // Reporting: manuscript, checklist, drafts, exports, disclosure.
    let m = crate::report::manuscript::build(&conn()).unwrap();
    let c = crate::report::manuscript::checklist(&m);
    assert_eq!((c.auto, c.drafted, c.author), (18, 6, 3));
    assert!(
        m.sections
            .iter()
            .find(|s| s.item == "20")
            .unwrap()
            .text
            .contains("Two studies"),
        "drafted by the manuscript job"
    );
    assert!(m
        .sections
        .iter()
        .find(|s| s.item == "16")
        .unwrap()
        .text
        .contains("The searches identified 4 records; 1 duplicates were removed"));
    assert_eq!(m.studies.len(), 2);
    assert_eq!(m.themes[0].quotes.len(), 4);
    let svg = crate::report::flow::svg(&m.prisma);
    assert!(svg.contains("Studies included in review (n = 2)"));
    let ris = crate::report::exports::ris(&conn()).unwrap();
    assert_eq!(ris.matches("TY  - JOUR").count(), 2);
    let csv = crate::report::exports::events_csv(&conn()).unwrap();
    let events: i64 = conn()
        .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))
        .unwrap();
    assert_eq!(csv.matches("\r\n").count() as i64, events + 1);
    let d = crate::report::metrics::disclosure(&conn()).unwrap();
    assert!(d
        .models
        .iter()
        .any(|m| m.task == "screener_a" && m.model_reported.as_deref() == Some("mock-model")));
    assert_eq!(d.quality.kappa_ab, Some(1.0));
    assert!(
        d.quality.drift.is_none(),
        "3 screened records: below the 5 needed for a drift check"
    );
    assert_eq!(
        d.quality.seed_found.map(|s| s.0),
        d.quality.seed_found.map(|s| s.1)
    );

    // Gate 3.
    gate3::confirm_links(&conn()).unwrap();
    crate::agents::synthesis::confirm_certainty(&conn(), &cerqual.id, "moderate", None).unwrap();
    for (k, _) in gate3::AUTHOR_ITEMS {
        gate3::set_author_text(&conn(), k, "Written by the author.").unwrap();
    }
    gate3::sign_off(&conn()).unwrap();
    assert_eq!(halt(), Halt::Done);
    let v = pipeline::verification(&conn()).unwrap();
    assert!(v.verified, "{v:?}");

    // Every agent event carries provider, model and prompt version (principle 4).
    let missing: i64 = conn()
        .query_row(
            "SELECT COUNT(*) FROM event WHERE actor_type = 'agent' AND prompt_version IS NOT NULL AND (provider IS NULL OR model_id IS NULL)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(missing, 0);
    // Raw responses are stored for every call (principle 6).
    let calls: i64 = conn()
        .query_row("SELECT COUNT(*) FROM llm_call", [], |r| r.get(0))
        .unwrap();
    assert!(calls >= 20, "{calls}");
    let raw_empty: i64 = conn()
        .query_row(
            "SELECT COUNT(*) FROM llm_call WHERE raw_response = ''",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(raw_empty, 0);
}

/// Bypass mode: no gate or queue waits for a person. The run stops only on a
/// real problem (here: no full text could be found), and every decision it
/// made is an agent event with a "Bypass mode:" rationale.
#[tokio::test]
async fn bypass_mode_runs_without_a_person() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path()).await;
    let conn = || crate::db::open_review(&f.path).unwrap();
    let halt = || pipeline::advance(&conn(), &f.id).unwrap();
    pipeline::bypass::set(&conn(), &f.id, true).unwrap();

    // Protocol, Gate 1, search, screening, Gate 2 and retrieval in one go;
    // no PDF is open access, so both reports are marked not retrieved and
    // nothing can be included.
    f.worker.drain().await.unwrap();
    assert!(failed_jobs(&conn()).is_empty(), "{:?}", failed_jobs(&conn()));
    assert_eq!(
        halt(),
        Halt::Waiting(
            "No studies were included. Check the screening decisions or the protocol.".into()
        )
    );
    let by_bypass = |stage: &str, decision: &str| -> i64 {
        conn()
            .query_row(
                "SELECT COUNT(*) FROM event WHERE stage = ?1 AND decision = ?2 AND actor_type = 'agent' AND rationale LIKE 'Bypass mode:%'",
                [stage, decision],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(by_bypass("gate1", "skip"), 1);
    assert_eq!(by_bypass("gate2", "skip"), 1);
    assert_eq!(by_bypass("retrieval", "not_retrieved"), 2);
    assert!(retrieval::upload_queue(&conn()).unwrap().is_empty());

    // The author adds the PDFs later; bypass mode carries on to the end.
    let data_dir = f.path.with_file_name(format!(
        "{}.files",
        f.path.file_stem().unwrap().to_string_lossy()
    ));
    for rep in retrieval::without_pdf(&conn()).unwrap() {
        retrieval::upload(&conn(), &data_dir, &rep, &fake_pdf()).unwrap();
        let pages = vec![PAGE1.to_owned(), PAGE2.to_owned(), PAGE3.to_owned()];
        assert!(retrieval::store_page_texts(&conn(), &rep, &pages).unwrap());
    }
    f.worker.drain().await.unwrap();
    assert!(failed_jobs(&conn()).is_empty(), "{:?}", failed_jobs(&conn()));
    assert_eq!(halt(), Halt::Done);
    assert_eq!(by_bypass("gate3", "skip"), 1);
    let v = pipeline::verification(&conn()).unwrap();
    assert!(!v.verified);
    assert_eq!(v.skipped, vec![1, 2, 3]);
    assert_eq!(pipeline::bypass::decisions(&conn()).unwrap().len(), 5);
}
