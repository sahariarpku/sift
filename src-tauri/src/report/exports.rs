//! RIS of included studies and CSV of every event (§8.11).

use rusqlite::Connection;

use crate::error::Result;
use crate::records;

/// RIS (tagged format, one record per included report).
pub fn ris(conn: &Connection) -> Result<String> {
    ris_where(conn, "r.id IN (SELECT record_id FROM v_included_report)")
}

/// RIS of reports still waiting for a PDF, for importing into a reference
/// manager that can look for PDFs (e.g. Zotero's "Find Available PDF").
pub fn missing_ris(conn: &Connection) -> Result<String> {
    ris_where(
        conn,
        "r.id IN (SELECT rep.record_id FROM report rep JOIN v_latest_retrieval l ON l.report_id = rep.id WHERE l.decision = 'queued_for_upload')",
    )
}

fn ris_where(conn: &Connection, filter: &str) -> Result<String> {
    let recs = records::records_where(conn, filter)?;
    let mut out = String::new();
    for r in recs {
        out.push_str("TY  - JOUR\r\n");
        if let Some(t) = &r.title {
            out.push_str(&format!("TI  - {t}\r\n"));
        }
        for a in &r.authors {
            out.push_str(&format!("AU  - {a}\r\n"));
        }
        if let Some(y) = r.year {
            out.push_str(&format!("PY  - {y}\r\n"));
        }
        if let Some(v) = &r.venue {
            out.push_str(&format!("T2  - {v}\r\n"));
        }
        if let Some(d) = &r.doi {
            out.push_str(&format!("DO  - {d}\r\n"));
        }
        if let Some(u) = &r.landing_url {
            out.push_str(&format!("UR  - {u}\r\n"));
        }
        if let Some(a) = &r.abstract_ {
            out.push_str(&format!("AB  - {}\r\n", a.replace(['\r', '\n'], " ")));
        }
        if let Some(id) = &r.source_id {
            out.push_str(&format!("AN  - {}:{id}\r\n", r.source));
        }
        out.push_str("ER  - \r\n\r\n");
    }
    Ok(out)
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

pub const EVENT_COLUMNS: [&str; 21] = [
    "id",
    "review_id",
    "target_type",
    "target_id",
    "stage",
    "actor_type",
    "actor_id",
    "provider",
    "model_id",
    "prompt_version",
    "decision",
    "reason_code",
    "rationale",
    "evidence_quote",
    "confidence",
    "blinded",
    "supersedes",
    "tokens_in",
    "tokens_out",
    "cost_usd",
    "created_at",
];

/// Every event, in order, RFC 4180 quoting.
pub fn events_csv(conn: &Connection) -> Result<String> {
    let mut out = EVENT_COLUMNS.join(",");
    out.push_str("\r\n");
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM event ORDER BY id",
        EVENT_COLUMNS.join(", ")
    ))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let mut fields = Vec::with_capacity(EVENT_COLUMNS.len());
        for i in 0..EVENT_COLUMNS.len() {
            let v: rusqlite::types::Value = row.get(i)?;
            fields.push(match v {
                rusqlite::types::Value::Null => String::new(),
                rusqlite::types::Value::Integer(n) => n.to_string(),
                rusqlite::types::Value::Real(f) => f.to_string(),
                rusqlite::types::Value::Text(t) => csv_field(&t),
                rusqlite::types::Value::Blob(_) => String::new(),
            });
        }
        out.push_str(&fields.join(","));
        out.push_str("\r\n");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_quotes_fields_that_need_it() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a, b"), "\"a, b\"");
        assert_eq!(csv_field("say \"hi\"\nnow"), "\"say \"\"hi\"\"\nnow\"");
    }

    #[test]
    fn events_csv_has_header_and_rows() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_review(&dir.path().join("r.sift")).unwrap();
        let rid = crate::reviews::insert_review(&conn, "q").unwrap();
        crate::events::append(
            &conn,
            &crate::events::NewEvent {
                review_id: rid,
                target_type: "record".into(),
                target_id: "r1".into(),
                stage: "ta_screen".into(),
                actor_id: "human".into(),
                actor_type: Some(crate::events::ActorType::Human),
                decision: "exclude".into(),
                rationale: Some("Wrong population, \"clearly\"".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let csv = events_csv(&conn).unwrap();
        let lines: Vec<&str> = csv.split("\r\n").collect();
        assert!(lines[0].starts_with("id,review_id,target_type"));
        assert!(lines[1].contains("\"Wrong population, \"\"clearly\"\"\""));
    }
}
