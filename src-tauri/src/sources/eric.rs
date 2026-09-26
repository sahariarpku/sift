//! ERIC adapter.
//!
//! Checked 2026-09-24 against https://eric.ed.gov/?api and https://eric.ed.gov/api/eric-api.yaml,
//! and by live requests (fixture `tests/fixtures/sources/eric_search.json`):
//! - `GET https://api.ies.ed.gov/eric/?search=&format=json&start=&rows=&fields=`; no key.
//! - `rows` max 2000; paging by `start` offset. Response: `response.numFound`, `response.docs[]`.
//! - Field searches: `title:`, `description:` (abstract), `subject:` (Thesaurus descriptors).
//!   Boolean `AND`/`OR` in capitals, quoted phrases, trailing `*` truncation (verified live).
//! - Years: `publicationdateyear:[2010 TO 2026]` (verified live; not in the docs).
//! - Titles and abstracts contain HTML entities (`&apos;`).
//! - Full text: when `e_fulltextauth` is 1 the PDF is at `https://files.eric.ed.gov/fulltext/{id}.pdf`.
//! - No DOI field in API results.

use async_trait::async_trait;
use serde_json::Value;

use super::adapter::{
    get_json, non_empty, unescape_html, NormalisedRecord, Page, SourceAdapter, SourceResult,
};
use crate::query::{Field, QueryAst, Rendered, Term, TermKind};

const FIELDS: &str = "id,title,description,author,publicationdateyear,subject,source,language,publicationtype,peerreviewed,e_fulltextauth,url";

pub struct Eric {
    pub client: reqwest::Client,
    pub base: String,
}

impl Eric {
    pub fn new(client: reqwest::Client, base: Option<&str>) -> Self {
        Self {
            client,
            base: base
                .unwrap_or("https://api.ies.ed.gov")
                .trim_end_matches('/')
                .to_owned(),
        }
    }

    fn term(t: &Term, simpl: &mut Vec<String>) -> Vec<String> {
        if t.fields.contains(&Field::Subject) {
            return vec![format!("subject:\"{}\"", t.text)];
        }
        let v = match t.kind {
            TermKind::Phrase => {
                if t.text.contains('*') {
                    let s = format!("Truncation removed inside phrase \"{}\" (ERIC phrases cannot be truncated).", t.text);
                    if !simpl.contains(&s) {
                        simpl.push(s);
                    }
                }
                format!("\"{}\"", t.text.replace('*', ""))
            }
            TermKind::Word => t.text.clone(),
            TermKind::Truncated => format!("{}*", t.text),
        };
        vec![format!("title:{v}"), format!("description:{v}")]
    }

    fn get(&self, q: &Rendered, start: u64, rows: u32) -> reqwest::RequestBuilder {
        self.client
            .get(format!("{}/eric/", self.base))
            .timeout(std::time::Duration::from_secs(60))
            .query(&[
                ("search", q.query.as_str()),
                ("format", "json"),
                ("fields", FIELDS),
                ("start", &start.to_string()),
                ("rows", &rows.to_string()),
            ])
    }
}

#[async_trait]
impl SourceAdapter for Eric {
    fn id(&self) -> &'static str {
        "eric"
    }

    fn page_size(&self) -> u32 {
        200
    }

    fn render(&self, ast: &QueryAst) -> Rendered {
        let mut simpl = Vec::new();
        let mut parts: Vec<String> = ast
            .concepts
            .iter()
            .map(|c| {
                let terms: Vec<String> = c
                    .terms
                    .iter()
                    .flat_map(|t| Self::term(t, &mut simpl))
                    .collect();
                format!("({})", terms.join(" OR "))
            })
            .collect();
        if let (Some(a), Some(b)) = (ast.filters.year_from, ast.filters.year_to) {
            parts.push(format!("publicationdateyear:[{a} TO {b}]"));
        }
        if !ast.filters.languages.is_empty() {
            simpl.push("Language not filtered: ERIC records default to English and the language field is only set for other languages.".into());
        }
        simpl.extend(ast.notes.iter().cloned());
        Rendered {
            query: parts.join(" AND "),
            params: Vec::new(),
            simplifications: simpl,
        }
    }

    async fn count(&self, q: &Rendered) -> SourceResult<u64> {
        let v = get_json(&self.client, &format!("{}/eric/", self.base), || {
            self.get(q, 0, 1)
        })
        .await?;
        Ok(v.pointer("/response/numFound")
            .and_then(Value::as_u64)
            .unwrap_or(0))
    }

    async fn fetch_page(&self, q: &Rendered, cursor: Option<String>) -> SourceResult<Page> {
        let start: u64 = cursor.as_deref().and_then(|c| c.parse().ok()).unwrap_or(0);
        let rows = self.page_size();
        let raw = get_json(&self.client, &format!("{}/eric/", self.base), || {
            self.get(q, start, rows)
        })
        .await?;
        let total = raw.pointer("/response/numFound").and_then(Value::as_u64);
        let got = raw
            .pointer("/response/docs")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0) as u64;
        let next = start + got;
        let next_cursor = (got > 0 && total.is_some_and(|t| next < t)).then(|| next.to_string());
        Ok(Page {
            raw,
            total,
            next_cursor,
        })
    }

    fn normalise(&self, raw: &Value) -> Vec<NormalisedRecord> {
        let Some(docs) = raw.pointer("/response/docs").and_then(Value::as_array) else {
            return Vec::new();
        };
        docs.iter()
            .map(|d| {
                let id = non_empty(d["id"].as_str());
                let full_text = d["e_fulltextauth"].as_i64() == Some(1)
                    || d["e_fulltextauth"].as_str() == Some("1");
                NormalisedRecord {
                    pdf_url: match (&id, full_text) {
                        (Some(i), true) => {
                            Some(format!("https://files.eric.ed.gov/fulltext/{i}.pdf"))
                        }
                        _ => None,
                    },
                    landing_url: id.as_ref().map(|i| format!("https://eric.ed.gov/?id={i}")),
                    source_id: id,
                    doi: None,
                    title: non_empty(d["title"].as_str()).map(|s| unescape_html(&s)),
                    abstract_: non_empty(d["description"].as_str()).map(|s| unescape_html(&s)),
                    authors: d["author"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str())
                                .map(unescape_html)
                                .collect()
                        })
                        .unwrap_or_default(),
                    year: d["publicationdateyear"].as_i64().map(|y| y as i32),
                    venue: non_empty(d["source"].as_str()).map(|s| unescape_html(&s)),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_http::{MockServer, Reply};

    const FIXTURE: &str = include_str!("../../tests/fixtures/sources/eric_search.json");

    #[test]
    fn renders_fields_truncation_vocab_and_years() {
        let p = crate::agents::protocol::tests::sample_protocol();
        let ast = crate::query::QueryAst::from_protocol(&p, &Default::default());
        let r = Eric::new(reqwest::Client::new(), None).render(&ast);
        assert!(r.query.contains("title:\"Chinese international students\" OR description:\"Chinese international students\""));
        assert!(r.query.contains("subject:\"Foreign Students\""));
        assert!(r.query.contains("title:belong* OR description:belong*"));
        assert!(r.query.ends_with("AND publicationdateyear:[2010 TO 2026]"));
        assert!(r
            .simplifications
            .iter()
            .any(|s| s.contains("British universit*")));
        assert!(r
            .simplifications
            .iter()
            .any(|s| s.contains("Language not filtered")));
    }

    #[test]
    fn normalises_recorded_response() {
        let raw: Value = serde_json::from_str(FIXTURE).unwrap();
        let recs = Eric::new(reqwest::Client::new(), None).normalise(&raw);
        assert_eq!(recs.len(), 3);
        let r = &recs[0];
        assert!(r.source_id.as_deref().unwrap().starts_with("E"));
        assert!(r.title.as_deref().unwrap().len() > 10);
        assert!(!r.title.as_deref().unwrap().contains("&apos;"));
        assert!(r.year.is_some());
        assert!(r
            .landing_url
            .as_deref()
            .unwrap()
            .starts_with("https://eric.ed.gov/?id="));
    }

    #[tokio::test]
    async fn pages_by_start_offset() {
        let page = |start: usize, n: usize| {
            let docs: Vec<Value> = (0..n)
                .map(|i| serde_json::json!({"id": format!("EJ{}", start + i), "title": "t"}))
                .collect();
            serde_json::json!({"response": {"numFound": 250, "start": start, "docs": docs}})
                .to_string()
        };
        let server = MockServer::start(vec![
            Reply::json(200, page(0, 200)),
            Reply::json(200, page(200, 50)),
        ])
        .await;
        let e = Eric::new(reqwest::Client::new(), Some(server.base()));
        let q = Rendered {
            query: "title:x".into(),
            ..Default::default()
        };
        let p1 = e.fetch_page(&q, None).await.unwrap();
        assert_eq!(p1.next_cursor.as_deref(), Some("200"));
        let p2 = e.fetch_page(&q, p1.next_cursor).await.unwrap();
        assert_eq!(p2.next_cursor, None);
        let reqs = server.requests();
        assert!(reqs[1].path.contains("start=200"));
        assert!(reqs[0].path.contains("format=json"));
    }
}
