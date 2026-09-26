//! Provider configuration, task assignments and prices in `app.db`.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::cost::Price;
use super::{ModelInfo, Protocol, ProviderOptions};
use crate::error::{Error, Result};
use crate::time::now_iso;

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRecord {
    pub id: String,
    pub preset: String,
    pub display_name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub api_key_ref: Option<String>,
    pub models: Vec<ModelInfo>,
    pub data_policy_warning: bool,
    pub concurrency: u32,
    pub options: ProviderOptions,
    pub last_test_status: Option<String>,
    pub last_test_message: Option<String>,
    pub last_test_at: Option<String>,
}

fn read(r: &Row<'_>) -> rusqlite::Result<(ProviderRecord, String, String, String)> {
    Ok((
        ProviderRecord {
            id: r.get("id")?,
            preset: r.get("preset")?,
            display_name: r.get("display_name")?,
            protocol: Protocol::OpenaiCompat, // replaced below from the raw column
            base_url: r.get("base_url")?,
            api_key_ref: r.get("api_key_ref")?,
            models: Vec::new(),
            data_policy_warning: r.get("data_policy_warning")?,
            concurrency: r.get("concurrency")?,
            options: ProviderOptions::default(),
            last_test_status: r.get("last_test_status")?,
            last_test_message: r.get("last_test_message")?,
            last_test_at: r.get("last_test_at")?,
        },
        r.get("protocol")?,
        r.get("models")?,
        r.get("options")?,
    ))
}

fn finish(raw: (ProviderRecord, String, String, String)) -> Result<ProviderRecord> {
    let (mut rec, protocol, models, options) = raw;
    rec.protocol = Protocol::parse(&protocol)
        .ok_or_else(|| Error::Invalid(format!("unknown protocol {protocol}")))?;
    rec.models = serde_json::from_str(&models)?;
    rec.options = serde_json::from_str(&options)?;
    Ok(rec)
}

pub fn list(conn: &Connection) -> Result<Vec<ProviderRecord>> {
    let mut stmt = conn.prepare("SELECT * FROM provider ORDER BY created_at, id")?;
    let raws = stmt
        .query_map([], read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter().map(finish).collect()
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<ProviderRecord>> {
    conn.query_row("SELECT * FROM provider WHERE id = ?1", [id], read)
        .optional()?
        .map(finish)
        .transpose()
}

pub fn upsert(conn: &Connection, p: &ProviderRecord) -> Result<()> {
    let now = now_iso();
    conn.execute(
        "INSERT INTO provider (id, preset, display_name, protocol, base_url, api_key_ref, models,
            data_policy_warning, concurrency, options, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
         ON CONFLICT(id) DO UPDATE SET preset = excluded.preset, display_name = excluded.display_name,
            protocol = excluded.protocol, base_url = excluded.base_url, api_key_ref = excluded.api_key_ref,
            models = excluded.models, data_policy_warning = excluded.data_policy_warning,
            concurrency = excluded.concurrency, options = excluded.options, updated_at = excluded.updated_at",
        params![
            p.id,
            p.preset,
            p.display_name,
            p.protocol.as_str(),
            p.base_url,
            p.api_key_ref,
            serde_json::to_string(&p.models)?,
            p.data_policy_warning,
            p.concurrency,
            serde_json::to_string(&p.options)?,
            now,
        ],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM provider WHERE id = ?1", [id])?;
    Ok(())
}

pub fn set_models(conn: &Connection, id: &str, models: &[ModelInfo]) -> Result<()> {
    conn.execute(
        "UPDATE provider SET models = ?1, updated_at = ?2 WHERE id = ?3",
        params![serde_json::to_string(models)?, now_iso(), id],
    )?;
    Ok(())
}

pub fn set_test_result(conn: &Connection, id: &str, status: &str, message: &str) -> Result<()> {
    conn.execute(
        "UPDATE provider SET last_test_status = ?1, last_test_message = ?2, last_test_at = ?3 WHERE id = ?4",
        params![status, message, now_iso(), id],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tasks (§6.4 per-task assignment)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Task {
    ScreenerA,
    ScreenerB,
    Arbiter,
    Extraction,
    Appraisal,
    ProtocolSearch,
    Synthesis,
}

impl Task {
    pub const ALL: [Task; 7] = [
        Task::ScreenerA,
        Task::ScreenerB,
        Task::Arbiter,
        Task::Extraction,
        Task::Appraisal,
        Task::ProtocolSearch,
        Task::Synthesis,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Task::ScreenerA => "screener_a",
            Task::ScreenerB => "screener_b",
            Task::Arbiter => "arbiter",
            Task::Extraction => "extraction",
            Task::Appraisal => "appraisal",
            Task::ProtocolSearch => "protocol_search",
            Task::Synthesis => "synthesis",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Task::ScreenerA => "Screener A",
            Task::ScreenerB => "Screener B",
            Task::Arbiter => "Arbiter",
            Task::Extraction => "Extraction",
            Task::Appraisal => "Appraisal",
            Task::ProtocolSearch => "Protocol and search",
            Task::Synthesis => "Synthesis",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Task::ALL.into_iter().find(|t| t.as_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Assignment {
    pub task: Task,
    pub label: &'static str,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
}

pub fn assignments(conn: &Connection) -> Result<Vec<Assignment>> {
    let mut stmt =
        conn.prepare("SELECT provider_id, model_id FROM task_assignment WHERE task = ?1")?;
    Task::ALL
        .into_iter()
        .map(|task| {
            let row: Option<(String, String)> = stmt
                .query_row([task.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            Ok(Assignment {
                task,
                label: task.label(),
                provider_id: row.as_ref().map(|r| r.0.clone()),
                model_id: row.map(|r| r.1),
            })
        })
        .collect()
}

pub fn assignment(conn: &Connection, task: Task) -> Result<Option<(String, String)>> {
    Ok(conn
        .query_row(
            "SELECT provider_id, model_id FROM task_assignment WHERE task = ?1",
            [task.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

pub fn set_assignment(conn: &Connection, task: Task, target: Option<(&str, &str)>) -> Result<()> {
    match target {
        Some((provider_id, model_id)) => {
            if model_id.trim().is_empty() {
                return Err(Error::Invalid("Choose a model.".into()));
            }
            conn.execute(
                "INSERT INTO task_assignment (task, provider_id, model_id, updated_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(task) DO UPDATE SET provider_id = excluded.provider_id,
                    model_id = excluded.model_id, updated_at = excluded.updated_at",
                params![task.as_str(), provider_id, model_id.trim(), now_iso()],
            )?;
        }
        None => {
            conn.execute(
                "DELETE FROM task_assignment WHERE task = ?1",
                [task.as_str()],
            )?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Prices

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceRow {
    pub provider_id: String,
    pub model_id: String,
    pub price: Price,
}

pub fn price(conn: &Connection, provider_id: &str, model_id: &str) -> Result<Option<Price>> {
    Ok(conn
        .query_row(
            "SELECT input_per_mtok, output_per_mtok FROM model_price WHERE provider_id = ?1 AND model_id = ?2",
            [provider_id, model_id],
            |r| Ok(Price { input_per_mtok: r.get(0)?, output_per_mtok: r.get(1)? }),
        )
        .optional()?)
}

pub fn prices(conn: &Connection) -> Result<Vec<PriceRow>> {
    let mut stmt =
        conn.prepare("SELECT provider_id, model_id, input_per_mtok, output_per_mtok FROM model_price ORDER BY 1, 2")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(PriceRow {
                provider_id: r.get(0)?,
                model_id: r.get(1)?,
                price: Price {
                    input_per_mtok: r.get(2)?,
                    output_per_mtok: r.get(3)?,
                },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn set_price(
    conn: &Connection,
    provider_id: &str,
    model_id: &str,
    price: Option<Price>,
) -> Result<()> {
    match price {
        Some(p) => {
            if !(p.input_per_mtok.is_finite() && p.output_per_mtok.is_finite())
                || p.input_per_mtok < 0.0
                || p.output_per_mtok < 0.0
            {
                return Err(Error::Invalid("Prices must be zero or more.".into()));
            }
            conn.execute(
                "INSERT INTO model_price (provider_id, model_id, input_per_mtok, output_per_mtok) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(provider_id, model_id) DO UPDATE SET input_per_mtok = excluded.input_per_mtok,
                    output_per_mtok = excluded.output_per_mtok",
                params![provider_id, model_id, p.input_per_mtok, p.output_per_mtok],
            )?;
        }
        None => {
            conn.execute(
                "DELETE FROM model_price WHERE provider_id = ?1 AND model_id = ?2",
                [provider_id, model_id],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn record(id: &str) -> ProviderRecord {
        ProviderRecord {
            id: id.into(),
            preset: "openrouter".into(),
            display_name: "OpenRouter".into(),
            protocol: Protocol::OpenaiCompat,
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key_ref: Some(format!("provider:{id}")),
            models: vec![ModelInfo {
                id: "m1".into(),
                display_name: None,
                structured_output: None,
            }],
            data_policy_warning: true,
            concurrency: 4,
            options: ProviderOptions::default(),
            last_test_status: None,
            last_test_message: None,
            last_test_at: None,
        }
    }

    #[test]
    fn provider_round_trip_and_cascade() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open_app(&dir.path().join("app.db")).unwrap();
        upsert(&conn, &record("p1")).unwrap();
        let mut got = get(&conn, "p1").unwrap().unwrap();
        assert_eq!(got, record("p1"));
        got.display_name = "Renamed".into();
        upsert(&conn, &got).unwrap();
        assert_eq!(list(&conn).unwrap()[0].display_name, "Renamed");

        set_test_result(&conn, "p1", "ok", "Connected").unwrap();
        assert_eq!(
            get(&conn, "p1")
                .unwrap()
                .unwrap()
                .last_test_status
                .as_deref(),
            Some("ok")
        );

        set_assignment(&conn, Task::ScreenerA, Some(("p1", "m1"))).unwrap();
        set_price(
            &conn,
            "p1",
            "m1",
            Some(Price {
                input_per_mtok: 1.0,
                output_per_mtok: 2.0,
            }),
        )
        .unwrap();
        assert_eq!(
            assignment(&conn, Task::ScreenerA).unwrap(),
            Some(("p1".into(), "m1".into()))
        );
        delete(&conn, "p1").unwrap();
        assert_eq!(
            assignment(&conn, Task::ScreenerA).unwrap(),
            None,
            "assignments go with the provider"
        );
        assert!(prices(&conn).unwrap().is_empty());
    }

    #[test]
    fn assignments_cover_all_seven_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open_app(&dir.path().join("app.db")).unwrap();
        let all = assignments(&conn).unwrap();
        assert_eq!(all.len(), 7);
        assert!(all.iter().all(|a| a.provider_id.is_none()));
        assert_eq!(Task::parse("protocol_search"), Some(Task::ProtocolSearch));
    }

    #[test]
    fn rejects_negative_prices_and_empty_models() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open_app(&dir.path().join("app.db")).unwrap();
        upsert(&conn, &record("p1")).unwrap();
        assert!(set_price(
            &conn,
            "p1",
            "m1",
            Some(Price {
                input_per_mtok: -1.0,
                output_per_mtok: 0.0
            })
        )
        .is_err());
        assert!(set_assignment(&conn, Task::Arbiter, Some(("p1", " "))).is_err());
    }
}
