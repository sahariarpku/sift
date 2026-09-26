//! Protocol agent (§8.1) and protocol versions (Gate 1).
//!
//! Version 0 is the editable draft. Locking writes version 1 (and later
//! amendments 2, 3, ...) with `locked_at` set, plus `lock` events.

use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{call, AgentError};
use crate::error::{Error, Result};
use crate::events::{self, ActorType, NewEvent};
use crate::jobs::{Job, JobContext, JobError, JobHandler};
use crate::llm::store::Task;
use crate::reviews::new_id;
use crate::time::now_iso;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum QuestionType {
    Intervention,
    Exposure,
    Experience,
    Policy,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Framework {
    #[serde(rename = "PICO")]
    Pico,
    #[serde(rename = "PEO")]
    Peo,
    #[serde(rename = "SPIDER")]
    Spider,
    #[serde(rename = "PICo")]
    PiCo,
    #[serde(rename = "SPICE")]
    Spice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TestableAt {
    TitleAbstract,
    FullText,
    Both,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FrameworkElement {
    pub element: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Concept {
    pub name: String,
    pub synonyms: Vec<String>,
    pub controlled_vocab: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Criterion {
    pub code: String,
    pub text: String,
    pub testable_at: TestableAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DateRange {
    pub from: i32,
    pub to: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProtocolBody {
    pub question_type: QuestionType,
    pub framework: Framework,
    pub search_framework: Framework,
    pub framework_elements: Vec<FrameworkElement>,
    pub concepts: Vec<Concept>,
    pub inclusion_criteria: Vec<Criterion>,
    pub exclusion_criteria: Vec<Criterion>,
    pub date_range: DateRange,
    pub languages: Vec<String>,
    pub study_designs: Vec<String>,
    pub planned_synthesis: String,
    pub open_questions: Vec<String>,
}

impl ProtocolBody {
    /// All criterion codes, inclusion first.
    pub fn codes(&self) -> Vec<&str> {
        self.inclusion_criteria
            .iter()
            .chain(&self.exclusion_criteria)
            .map(|c| c.code.as_str())
            .collect()
    }

    pub fn exclusion_codes(&self) -> Vec<&str> {
        self.exclusion_criteria
            .iter()
            .map(|c| c.code.as_str())
            .collect()
    }

    /// Criteria as the screening prompts show them.
    pub fn criteria_text(&self) -> String {
        let mut s = String::from("Inclusion:\n");
        for c in &self.inclusion_criteria {
            s.push_str(&format!("- {}: {}\n", c.code, c.text));
        }
        s.push_str("Exclusion:\n");
        for c in &self.exclusion_criteria {
            s.push_str(&format!("- {}: {}\n", c.code, c.text));
        }
        s
    }

    pub fn is_qualitative(&self) -> bool {
        matches!(self.question_type, QuestionType::Experience)
    }

    /// Structural checks a schema cannot express (§8.1).
    pub fn validate(&self) -> std::result::Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for (list, prefix) in [
            (&self.inclusion_criteria, "IN-"),
            (&self.exclusion_criteria, "EX-"),
        ] {
            if list.is_empty() {
                return Err(format!("no {prefix} criteria"));
            }
            for c in list {
                let ok_shape = c.code.starts_with(prefix)
                    && c.code.len() > 3
                    && c.code[3..]
                        .chars()
                        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '-');
                if !ok_shape {
                    return Err(format!(
                        "criterion code {:?} must look like {prefix}LABEL",
                        c.code
                    ));
                }
                if !seen.insert(c.code.clone()) {
                    return Err(format!("criterion code {} is used twice", c.code));
                }
                if c.text.trim().is_empty() {
                    return Err(format!("criterion {} has no text", c.code));
                }
            }
        }
        if self.concepts.is_empty() || self.concepts.len() > 6 {
            return Err("give 1 to 6 search concepts".into());
        }
        if self
            .concepts
            .iter()
            .any(|c| c.synonyms.iter().all(|s| s.trim().is_empty()))
        {
            return Err("every concept needs at least one search term".into());
        }
        if self.date_range.from > self.date_range.to {
            return Err("date range is reversed".into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Review options chosen on the Start screen

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ReviewOptions {
    pub year_from: i32,
    pub year_to: i32,
    pub languages: Vec<String>,
    /// "detect" or a question type chosen by the user.
    pub review_type: String,
    /// Source ids to search, in order.
    pub sources: Vec<String>,
    /// Stop fetching a source after this many records (disclosed as a simplification).
    pub max_records_per_source: u32,
    pub citation_chasing: bool,
    pub seed_check: bool,
    /// Optional local pre-screen with Laya-MLX (off unless chosen).
    pub prescreen: crate::local::laya::Prescreen,
    /// Records still uncertain after the title/abstract arbiter go on to
    /// full-text screening instead of the human queue (off by default, §8.4).
    pub uncertain_to_full_text: bool,
    /// Reports still without a PDF this many hours after they were queued
    /// for upload are marked not retrieved (§8.5).
    pub retrieval_deadline_hours: u32,
}

impl Default for ReviewOptions {
    fn default() -> Self {
        Self {
            year_from: 2010,
            year_to: 2026,
            languages: vec!["English".into()],
            review_type: "detect".into(),
            sources: vec!["eric".into(), "openalex".into(), "core".into()],
            max_records_per_source: 1500,
            citation_chasing: true,
            seed_check: true,
            prescreen: Default::default(),
            uncertain_to_full_text: false,
            retrieval_deadline_hours: 168,
        }
    }
}

pub fn save_options(conn: &Connection, o: &ReviewOptions) -> Result<()> {
    conn.execute(
        "INSERT INTO review_setting (key, value) VALUES ('options', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [serde_json::to_string(o)?],
    )?;
    Ok(())
}

pub fn options(conn: &Connection) -> Result<ReviewOptions> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM review_setting WHERE key = 'options'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or_default())
}

pub fn review_question(conn: &Connection) -> Result<(String, String)> {
    Ok(
        conn.query_row("SELECT id, question FROM review LIMIT 1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?,
    )
}

// ---------------------------------------------------------------------------
// Versions

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolVersion {
    pub id: String,
    pub version: i64,
    pub body: ProtocolBody,
    pub locked_at: Option<String>,
}

fn read_version(conn: &Connection, sql: &str) -> Result<Option<ProtocolVersion>> {
    let row: Option<(String, i64, String, Option<String>)> = conn
        .query_row(sql, [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    row.map(|(id, version, body, locked_at)| {
        Ok(ProtocolVersion {
            id,
            version,
            body: serde_json::from_str(&body)?,
            locked_at,
        })
    })
    .transpose()
}

pub fn draft(conn: &Connection) -> Result<Option<ProtocolVersion>> {
    read_version(
        conn,
        "SELECT id, version, body, locked_at FROM protocol_version WHERE version = 0",
    )
}

/// The latest locked version: what the pipeline runs on.
pub fn locked(conn: &Connection) -> Result<Option<ProtocolVersion>> {
    read_version(
        conn,
        "SELECT id, version, body, locked_at FROM protocol_version WHERE locked_at IS NOT NULL ORDER BY version DESC LIMIT 1",
    )
}

pub fn require_locked(conn: &Connection) -> Result<ProtocolVersion> {
    locked(conn)?.ok_or_else(|| Error::Invalid("The protocol is not locked yet.".into()))
}

fn store_draft(conn: &Connection, review_id: &str, body: &ProtocolBody) -> Result<String> {
    let json = serde_json::to_string(body)?;
    if let Some(d) = draft(conn)? {
        conn.execute(
            "UPDATE protocol_version SET body = ?1 WHERE id = ?2",
            params![json, d.id],
        )?;
        Ok(d.id)
    } else {
        let id = new_id();
        conn.execute(
            "INSERT INTO protocol_version (id, review_id, version, body, locked_at) VALUES (?1, ?2, 0, ?3, NULL)",
            params![id, review_id, json],
        )?;
        Ok(id)
    }
}

/// Saves the user's edits to the draft (Gate 1 editing) with a human event.
pub fn save_draft_edit(conn: &Connection, body: &ProtocolBody) -> Result<ProtocolVersion> {
    if locked(conn)?.is_some() {
        return Err(Error::Invalid(
            "The protocol is locked. Changes need an amendment.".into(),
        ));
    }
    body.validate().map_err(Error::Invalid)?;
    let (review_id, _) = review_question(conn)?;
    let id = store_draft(conn, &review_id, body)?;
    events::append(
        conn,
        &NewEvent {
            review_id,
            target_type: "protocol".into(),
            target_id: id,
            stage: "protocol".into(),
            actor_type: Some(ActorType::Human),
            actor_id: "human".into(),
            decision: "edit".into(),
            ..Default::default()
        },
    )?;
    draft(conn)?.ok_or_else(|| Error::Invalid("draft missing".into()))
}

/// Locks the draft as version 1. `reviewed` is false when the user skips
/// Gate 1, which leaves the review permanently unverified (§9).
pub fn lock(conn: &Connection, reviewed: bool) -> Result<ProtocolVersion> {
    lock_by(
        conn,
        reviewed,
        ActorType::Human,
        "human",
        "Gate 1 skipped: protocol locked without review.",
    )
}

/// `lock` for a given actor; bypass mode locks as an agent with its own rationale.
pub fn lock_by(
    conn: &Connection,
    reviewed: bool,
    actor_type: ActorType,
    actor_id: &str,
    skip_rationale: &str,
) -> Result<ProtocolVersion> {
    if let Some(l) = locked(conn)? {
        return Ok(l);
    }
    let d =
        draft(conn)?.ok_or_else(|| Error::Invalid("There is no protocol draft to lock.".into()))?;
    d.body.validate().map_err(Error::Invalid)?;
    let (review_id, _) = review_question(conn)?;
    let id = new_id();
    let now = now_iso();
    conn.execute(
        "INSERT INTO protocol_version (id, review_id, version, body, locked_at) VALUES (?1, ?2, 1, ?3, ?4)",
        params![id, review_id, serde_json::to_string(&d.body)?, now],
    )?;
    let event = |stage: &str, decision: &str, rationale: Option<&str>| NewEvent {
        review_id: review_id.clone(),
        target_type: "protocol".into(),
        target_id: id.clone(),
        stage: stage.into(),
        actor_type: Some(actor_type),
        actor_id: actor_id.into(),
        decision: decision.into(),
        rationale: rationale.map(str::to_owned),
        ..Default::default()
    };
    events::append(conn, &event("protocol", "lock", None))?;
    if reviewed {
        events::append(
            conn,
            &event(
                "gate1",
                "pass",
                Some("Protocol reviewed and locked by the user."),
            ),
        )?;
    } else {
        events::append(conn, &event("gate1", "skip", Some(skip_rationale)))?;
    }
    require_locked(conn)
}

// ---------------------------------------------------------------------------
// Agent job

pub const DRAFT_JOB: &str = "protocol_draft";

pub struct DraftProtocol;

#[async_trait]
impl JobHandler for DraftProtocol {
    async fn run(&self, _job: &Job, ctx: &JobContext) -> std::result::Result<(), JobError> {
        let (review_id, question, opts) = {
            let conn = ctx.open().map_err(|e| JobError::Fatal(e.to_string()))?;
            let (id, q) = review_question(&conn).map_err(|e| JobError::Fatal(e.to_string()))?;
            (
                id,
                q,
                options(&conn).map_err(|e| JobError::Fatal(e.to_string()))?,
            )
        };
        let vars = [
            ("question", question),
            ("years", format!("{} to {}", opts.year_from, opts.year_to)),
            ("languages", opts.languages.join(", ")),
            ("review_type", opts.review_type.clone()),
        ];
        let reply =
            call::<ProtocolBody, _>(ctx, Task::ProtocolSearch, "protocol.v2", &vars, 6000, |b| {
                b.validate()
            })
            .await
            .map_err(|e| match e {
                AgentError::NeedsHuman(m) => JobError::Pause(format!(
                    "The protocol agent could not produce a valid protocol: {m}"
                )),
                other => other.into(),
            })?;
        let mut body = reply.value;
        // The user's constraints win over the model's suggestion.
        body.date_range = DateRange {
            from: opts.year_from,
            to: opts.year_to,
        };
        if !opts.languages.is_empty() {
            body.languages = opts.languages.clone();
        }
        let conn = ctx.open().map_err(|e| JobError::Fatal(e.to_string()))?;
        let id =
            store_draft(&conn, &review_id, &body).map_err(|e| JobError::Fatal(e.to_string()))?;
        let mut ev = reply.meta.event(
            &review_id,
            "protocol",
            &id,
            "protocol",
            "protocol_agent",
            "draft",
        );
        ev.rationale = Some(format!(
            "Question type {:?}; framing {:?}; searching {:?}.",
            body.question_type, body.framework, body.search_framework
        ));
        events::append(&conn, &ev).map_err(|e| JobError::Fatal(e.to_string()))?;
        conn.execute(
            "UPDATE review SET question_type = ?1, framework = ?2",
            params![
                serde_json::to_value(body.question_type)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned)),
                serde_json::to_value(body.framework)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned)),
            ],
        )
        .map_err(|e| JobError::Fatal(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn sample_protocol() -> ProtocolBody {
        ProtocolBody {
            question_type: QuestionType::Experience,
            framework: Framework::Spider,
            search_framework: Framework::PiCo,
            framework_elements: vec![FrameworkElement {
                element: "Sample".into(),
                description: "Chinese international students".into(),
            }],
            concepts: vec![
                Concept {
                    name: "Chinese international students".into(),
                    synonyms: vec![
                        "Chinese international students".into(),
                        "Chinese students".into(),
                        "mainland Chinese".into(),
                    ],
                    controlled_vocab: vec!["Foreign Students".into()],
                },
                Concept {
                    name: "experience".into(),
                    synonyms: vec!["experience*".into(), "belong*".into()],
                    controlled_vocab: vec![],
                },
                Concept {
                    name: "UK higher education".into(),
                    synonyms: vec![
                        "UK".into(),
                        "United Kingdom".into(),
                        "British universit*".into(),
                    ],
                    controlled_vocab: vec![],
                },
            ],
            inclusion_criteria: vec![
                Criterion {
                    code: "IN-POP".into(),
                    text: "Chinese international students".into(),
                    testable_at: TestableAt::TitleAbstract,
                },
                Criterion {
                    code: "IN-CONTEXT".into(),
                    text: "UK higher education".into(),
                    testable_at: TestableAt::Both,
                },
                Criterion {
                    code: "IN-DESIGN".into(),
                    text: "Qualitative or mixed methods".into(),
                    testable_at: TestableAt::FullText,
                },
            ],
            exclusion_criteria: vec![
                Criterion {
                    code: "EX-POP".into(),
                    text: "Other or mixed nationalities without separate findings".into(),
                    testable_at: TestableAt::Both,
                },
                Criterion {
                    code: "EX-SETTING".into(),
                    text: "Not in the UK".into(),
                    testable_at: TestableAt::TitleAbstract,
                },
            ],
            date_range: DateRange {
                from: 2010,
                to: 2026,
            },
            languages: vec!["English".into()],
            study_designs: vec!["qualitative".into()],
            planned_synthesis: "Thematic synthesis with GRADE-CERQual".into(),
            open_questions: vec![],
        }
    }

    #[test]
    fn validation_catches_bad_codes() {
        let mut p = sample_protocol();
        assert!(p.validate().is_ok());
        p.inclusion_criteria[0].code = "in-pop".into();
        assert!(p.validate().is_err());
        let mut p = sample_protocol();
        p.exclusion_criteria[0].code = "IN-POP".into();
        assert!(p.validate().unwrap_err().contains("EX-"));
        let mut p = sample_protocol();
        p.exclusion_criteria.push(p.exclusion_criteria[0].clone());
        assert!(p.validate().unwrap_err().contains("twice"));
    }

    #[test]
    fn schema_serialises_framework_names_exactly() {
        let s = crate::agents::schema_for::<ProtocolBody>("protocol")
            .schema
            .to_string();
        for f in ["\"PICO\"", "\"PEO\"", "\"SPIDER\"", "\"PICo\"", "\"SPICE\""] {
            assert!(s.contains(f), "{f}");
        }
    }

    #[test]
    fn draft_edit_lock_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let rid = crate::reviews::insert_review(&conn, "q").unwrap();
        store_draft(&conn, &rid, &sample_protocol()).unwrap();
        let mut edited = sample_protocol();
        edited.study_designs.push("mixed methods".into());
        save_draft_edit(&conn, &edited).unwrap();
        assert_eq!(draft(&conn).unwrap().unwrap().body.study_designs.len(), 2);

        let v1 = lock(&conn, true).unwrap();
        assert_eq!(v1.version, 1);
        assert!(v1.locked_at.is_some());
        assert!(
            save_draft_edit(&conn, &edited).is_err(),
            "no edits after lock"
        );
        let lock_events: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM event WHERE decision = 'lock' AND actor_type = 'human'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(lock_events, 1);
        let gate: String = conn
            .query_row(
                "SELECT decision FROM event WHERE stage = 'gate1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(gate, "pass");
    }
}
