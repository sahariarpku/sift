//! Search runs, records, reports and studies: storage and lookups.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

use crate::error::Result;
use crate::query::{QueryAst, Rendered};
use crate::reviews::new_id;
use crate::sources::adapter::NormalisedRecord;
use crate::time::now_iso;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecordRow {
    pub id: String,
    pub search_run_id: String,
    pub source: String,
    pub source_id: Option<String>,
    pub doi: Option<String>,
    pub title: Option<String>,
    #[serde(rename = "abstract")]
    pub abstract_: Option<String>,
    pub authors: Vec<String>,
    pub year: Option<i32>,
    pub venue: Option<String>,
    pub pdf_url: Option<String>,
    pub landing_url: Option<String>,
}

impl RecordRow {
    /// Title and abstract as screeners see them.
    pub fn screening_text(&self) -> String {
        let mut s = format!("Title: {}\n", self.title.as_deref().unwrap_or("(no title)"));
        if !self.authors.is_empty() {
            s.push_str(&format!("Authors: {}\n", self.authors.join("; ")));
        }
        if let Some(y) = self.year {
            s.push_str(&format!("Year: {y}\n"));
        }
        if let Some(v) = &self.venue {
            s.push_str(&format!("Source: {v}\n"));
        }
        s.push_str(&format!(
            "Abstract: {}",
            self.abstract_
                .as_deref()
                .unwrap_or("(no abstract available)")
        ));
        s
    }

    /// "Surname Year" for study labels.
    pub fn short_label(&self) -> String {
        let surname = self
            .authors
            .first()
            .map(|a| {
                let s = crate::dedup::surname(a);
                let mut c = s.chars();
                c.next()
                    .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                    .unwrap_or_default()
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Anon".into());
        match self.year {
            Some(y) => format!("{surname} {y}"),
            None => surname,
        }
    }
}

const RECORD_COLS: &str = "r.id, r.search_run_id, run.source, r.source_id, r.doi, r.title, r.abstract, r.authors, r.year, r.venue, r.pdf_url, r.landing_url";

fn read_record(row: &Row<'_>) -> rusqlite::Result<RecordRow> {
    let authors: Option<String> = row.get(7)?;
    Ok(RecordRow {
        id: row.get(0)?,
        search_run_id: row.get(1)?,
        source: row.get(2)?,
        source_id: row.get(3)?,
        doi: row.get(4)?,
        title: row.get(5)?,
        abstract_: row.get(6)?,
        authors: authors
            .and_then(|a| serde_json::from_str(&a).ok())
            .unwrap_or_default(),
        year: row.get(8)?,
        venue: row.get(9)?,
        pdf_url: row.get(10)?,
        landing_url: row.get(11)?,
    })
}

pub fn record(conn: &Connection, id: &str) -> Result<Option<RecordRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {RECORD_COLS} FROM record r JOIN search_run run ON run.id = r.search_run_id WHERE r.id = ?1"),
            [id],
            read_record,
        )
        .optional()?)
}

/// In `v_screenable`: not a duplicate and from a run that counts.
pub fn screenable(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM v_screenable WHERE id = ?1)",
        [id],
        |r| r.get(0),
    )?)
}

/// Records matching a WHERE clause over `r` (record) and `run` (search_run).
pub fn records_where(conn: &Connection, clause: &str) -> Result<Vec<RecordRow>> {
    let sql = format!("SELECT {RECORD_COLS} FROM record r JOIN search_run run ON run.id = r.search_run_id WHERE {clause} ORDER BY r.rowid");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], read_record)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[allow(clippy::too_many_arguments)]
pub fn insert_run(
    conn: &Connection,
    review_id: &str,
    source: &str,
    rendered: &Rendered,
    ast: &QueryAst,
    iteration: i64,
    hits: u64,
    raw_path: Option<&str>,
) -> Result<String> {
    let id = new_id();
    conn.execute(
        "INSERT INTO search_run (id, review_id, source, query_string, query_ast, simplifications, iteration, run_at, hits, raw_path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            id,
            review_id,
            source,
            rendered.query,
            serde_json::to_string(ast)?,
            serde_json::to_string(&rendered.simplifications)?,
            iteration,
            now_iso(),
            hits as i64,
            raw_path,
        ],
    )?;
    Ok(id)
}

pub fn insert_records(
    conn: &Connection,
    run_id: &str,
    recs: &[NormalisedRecord],
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "INSERT INTO record (id, search_run_id, source_id, doi, title, abstract, authors, year, venue, pdf_url, landing_url)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?;
    let mut ids = Vec::with_capacity(recs.len());
    for r in recs {
        if r.title.is_none() && r.abstract_.is_none() {
            continue;
        }
        let id = new_id();
        stmt.execute(params![
            id,
            run_id,
            r.source_id,
            r.doi,
            r.title,
            r.abstract_,
            serde_json::to_string(&r.authors)?,
            r.year,
            r.venue,
            r.pdf_url,
            r.landing_url,
        ])?;
        ids.push(id);
    }
    Ok(ids)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchRunRow {
    pub id: String,
    pub source: String,
    pub query_string: String,
    pub simplifications: Vec<String>,
    pub iteration: i64,
    pub run_at: String,
    pub hits: i64,
    pub records: i64,
    pub current: bool,
}

pub fn runs(conn: &Connection) -> Result<Vec<SearchRunRow>> {
    let mut stmt = conn.prepare(
        "SELECT r.id, r.source, r.query_string, r.simplifications, r.iteration, r.run_at, r.hits,
                (SELECT COUNT(*) FROM record x WHERE x.search_run_id = r.id),
                EXISTS(SELECT 1 FROM v_current_run c WHERE c.id = r.id)
         FROM search_run r ORDER BY r.run_at",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let simpl: Option<String> = r.get(3)?;
            Ok(SearchRunRow {
                id: r.get(0)?,
                source: r.get(1)?,
                query_string: r.get(2)?,
                simplifications: simpl
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
                iteration: r.get(4)?,
                run_at: r.get(5)?,
                hits: r.get(6)?,
                records: r.get(7)?,
                current: r.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// PRISMA

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Prisma {
    pub identified_databases: i64,
    pub identified_other: i64,
    pub by_source: Vec<(String, i64)>,
    pub duplicates_removed: i64,
    pub records_screened: i64,
    /// Records left unscreened because the author stopped screening.
    pub records_not_screened: i64,
    pub records_excluded: i64,
    pub reports_sought: i64,
    pub reports_not_retrieved: i64,
    pub reports_assessed: i64,
    /// Retrieved reports left unclassified because the author stopped full-text screening.
    pub reports_awaiting: i64,
    pub reports_excluded: i64,
    pub exclusion_reasons: Vec<(String, i64)>,
    pub studies_included: i64,
    pub reports_included: i64,
}

pub fn prisma(conn: &Connection) -> Result<Prisma> {
    let mut p = conn.query_row("SELECT * FROM v_prisma_counts", [], |r| {
        Ok(Prisma {
            identified_databases: r.get("identified_databases")?,
            identified_other: r.get("identified_other")?,
            duplicates_removed: r.get("duplicates_removed")?,
            records_screened: r.get("records_screened")?,
            records_not_screened: r.get("records_not_screened")?,
            records_excluded: r.get("records_excluded")?,
            reports_sought: r.get("reports_sought")?,
            reports_not_retrieved: r.get("reports_not_retrieved")?,
            reports_assessed: r.get("reports_assessed")?,
            reports_awaiting: r.get("reports_awaiting")?,
            reports_excluded: r.get("reports_excluded")?,
            studies_included: r.get("studies_included")?,
            reports_included: r.get("reports_included")?,
            ..Default::default()
        })
    })?;
    let pairs = |sql: &str| -> Result<Vec<(String, i64)>> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    };
    p.by_source = pairs("SELECT source, n FROM v_prisma_sources ORDER BY n DESC")?;
    p.exclusion_reasons = pairs("SELECT reason_code, n FROM v_prisma_ft_reasons ORDER BY n DESC")?;
    Ok(p)
}

// ---------------------------------------------------------------------------
// Reports and studies

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReportRow {
    pub id: String,
    pub record_id: String,
    pub study_id: Option<String>,
    pub pdf_path: Option<String>,
    pub retrieval_status: String,
    pub pages: i64,
}

pub fn report_for_record(conn: &Connection, record_id: &str) -> Result<Option<ReportRow>> {
    Ok(conn
        .query_row(
            "SELECT id, record_id, study_id, pdf_path, retrieval_status, COALESCE(json_array_length(page_texts), 0)
             FROM report WHERE record_id = ?1",
            [record_id],
            |r| {
                Ok(ReportRow {
                    id: r.get(0)?,
                    record_id: r.get(1)?,
                    study_id: r.get(2)?,
                    pdf_path: r.get(3)?,
                    retrieval_status: r.get(4)?,
                    pages: r.get(5)?,
                })
            },
        )
        .optional()?)
}

pub fn reports(conn: &Connection) -> Result<Vec<ReportRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, record_id, study_id, pdf_path, retrieval_status, COALESCE(json_array_length(page_texts), 0) FROM report ORDER BY rowid",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(ReportRow {
                id: r.get(0)?,
                record_id: r.get(1)?,
                study_id: r.get(2)?,
                pdf_path: r.get(3)?,
                retrieval_status: r.get(4)?,
                pages: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn page_texts(conn: &Connection, report_id: &str) -> Result<Vec<String>> {
    let raw: Option<String> = conn.query_row(
        "SELECT page_texts FROM report WHERE id = ?1",
        [report_id],
        |r| r.get(0),
    )?;
    Ok(raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default())
}

/// Full text with `[[page N]]` markers, as agents read it. Long texts are cut
/// at `max_chars` on a page boundary, and the cut is stated in the text.
pub fn paged_text(pages: &[String], max_chars: usize) -> String {
    let mut out = String::new();
    for (i, p) in pages.iter().enumerate() {
        let block = format!("[[page {}]]\n{}\n\n", i + 1, p.trim());
        if out.len() + block.len() > max_chars && !out.is_empty() {
            out.push_str(&format!(
                "[[text truncated after page {} of {} for length]]",
                i,
                pages.len()
            ));
            break;
        }
        out.push_str(&block);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paged_text_marks_pages_and_truncation() {
        let pages = vec!["one".to_owned(), "two".to_owned(), "x".repeat(500)];
        let t = paged_text(&pages, 100);
        assert!(t.starts_with("[[page 1]]\none"));
        assert!(t.contains("[[page 2]]\ntwo"));
        assert!(t.contains("truncated after page 2 of 3"));
    }

    #[test]
    fn labels() {
        let r = RecordRow {
            id: "x".into(),
            search_run_id: "r".into(),
            source: "eric".into(),
            source_id: None,
            doi: None,
            title: Some("T".into()),
            abstract_: None,
            authors: vec!["Wang, Li".into()],
            year: Some(2021),
            venue: None,
            pdf_url: None,
            landing_url: None,
        };
        assert_eq!(r.short_label(), "Wang 2021");
        assert!(r.screening_text().contains("(no abstract available)"));
    }
}
