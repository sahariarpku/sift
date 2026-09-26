//! CORE adapter (open-access aggregator).
//!
//! Checked 2026-09-24 against https://api.core.ac.uk/docs/v3 and live requests
//! (fixture `tests/fixtures/sources/core_search.json`):
//! - `GET https://api.core.ac.uk/v3/search/works/?q=&limit=&offset=` with `Authorization: Bearer {key}`.
//!   The path needs the trailing slash (without it the API answers 301).
//! - Fields in `q`: `title:`, `abstract:`, `yearPublished>=`, `doi:`; `AND`/`OR`; quoted phrases.
//! - `limit` max 100. Results carry `fullText`, so pages are large; this adapter asks for 50.
//! - Response: `totalHits`, `results[]` with `id`, `doi`, `title`, `abstract`, `authors[].name`,
//!   `yearPublished`, `publisher`, `journals[].title`, `downloadUrl`.
//! - Rate limit headers `x-ratelimit-*`; 429 when exceeded.

use async_trait::async_trait;
use serde_json::Value;

use super::adapter::{
    clean_doi, get_json, non_empty, NormalisedRecord, Page, SourceAdapter, SourceResult,
};
use crate::query::{Field, QueryAst, Rendered, TermKind};

pub struct Core {
    pub client: reqwest::Client,
    pub base: String,
    pub key: String,
}

impl Core {
    pub fn new(client: reqwest::Client, base: Option<&str>, key: String) -> Self {
        Self {
            client,
            base: base
                .unwrap_or("https://api.core.ac.uk")
                .trim_end_matches('/')
                .to_owned(),
            key,
        }
    }

    fn get(&self, q: &Rendered, offset: u64, limit: u32) -> reqwest::RequestBuilder {
        self.client
            .get(format!("{}/v3/search/works/", self.base))
            .timeout(std::time::Duration::from_secs(120))
            .bearer_auth(&self.key)
            .query(&[
                ("q", q.query.as_str()),
                ("limit", &limit.to_string()),
                ("offset", &offset.to_string()),
            ])
    }
}

#[async_trait]
impl SourceAdapter for Core {
    fn id(&self) -> &'static str {
        "core"
    }

    fn page_size(&self) -> u32 {
        50
    }

    fn render(&self, ast: &QueryAst) -> Rendered {
        let mut simpl = Vec::new();
        let mut note = |s: String| {
            if !simpl.contains(&s) {
                simpl.push(s);
            }
        };
        let mut parts: Vec<String> = Vec::new();
        for c in &ast.concepts {
            let mut terms = Vec::new();
            for t in &c.terms {
                if t.fields.contains(&Field::Subject) {
                    note(format!(
                        "Thesaurus term \"{}\" dropped: CORE has no controlled vocabulary.",
                        t.text
                    ));
                    continue;
                }
                let v = match t.kind {
                    TermKind::Truncated => {
                        note("Truncation (*) replaced by the word stem: CORE's truncation support is not documented.".into());
                        t.text.clone()
                    }
                    TermKind::Phrase => {
                        if t.text.contains('*') {
                            note(format!("Truncation removed inside phrase \"{}\".", t.text));
                        }
                        format!("\"{}\"", t.text.replace('*', ""))
                    }
                    TermKind::Word => t.text.clone(),
                };
                terms.push(format!("title:{v}"));
                terms.push(format!("abstract:{v}"));
            }
            if !terms.is_empty() {
                parts.push(format!("({})", terms.join(" OR ")));
            }
        }
        if let Some(a) = ast.filters.year_from {
            parts.push(format!("yearPublished>={a}"));
        }
        if let Some(b) = ast.filters.year_to {
            parts.push(format!("yearPublished<={b}"));
        }
        if !ast.filters.languages.is_empty() {
            note("Language not filtered: CORE's language field is often empty; screening applies the language criterion.".into());
        }
        simpl.extend(ast.notes.iter().cloned());
        Rendered {
            query: parts.join(" AND "),
            params: Vec::new(),
            simplifications: simpl,
        }
    }

    async fn count(&self, q: &Rendered) -> SourceResult<u64> {
        let v = get_json(
            &self.client,
            &format!("{}/v3/search/works/", self.base),
            || self.get(q, 0, 1),
        )
        .await?;
        Ok(v["totalHits"].as_u64().unwrap_or(0))
    }

    async fn fetch_page(&self, q: &Rendered, cursor: Option<String>) -> SourceResult<Page> {
        let offset: u64 = cursor.as_deref().and_then(|c| c.parse().ok()).unwrap_or(0);
        let limit = self.page_size();
        let mut raw = get_json(
            &self.client,
            &format!("{}/v3/search/works/", self.base),
            || self.get(q, offset, limit),
        )
        .await?;
        let total = raw["totalHits"].as_u64();
        let got = raw["results"].as_array().map(Vec::len).unwrap_or(0) as u64;
        // Keep stored raw pages small: full texts are fetched again at retrieval.
        if let Some(results) = raw["results"].as_array_mut() {
            for w in results {
                if let Some(obj) = w.as_object_mut() {
                    if let Some(ft) = obj.get("fullText").and_then(Value::as_str) {
                        let len = ft.len();
                        obj.insert(
                            "fullText".into(),
                            Value::String(format!("[{len} characters omitted]")),
                        );
                    }
                }
            }
        }
        let next = offset + got;
        let next_cursor = (got > 0 && total.is_some_and(|t| next < t)).then(|| next.to_string());
        Ok(Page {
            raw,
            total,
            next_cursor,
        })
    }

    fn normalise(&self, raw: &Value) -> Vec<NormalisedRecord> {
        let Some(results) = raw["results"].as_array() else {
            return Vec::new();
        };
        results
            .iter()
            .map(|w| {
                let id = w["id"]
                    .as_i64()
                    .map(|i| i.to_string())
                    .or_else(|| non_empty(w["id"].as_str()));
                NormalisedRecord {
                    landing_url: id.as_ref().map(|i| format!("https://core.ac.uk/works/{i}")),
                    source_id: id,
                    doi: w["doi"].as_str().and_then(clean_doi),
                    title: non_empty(w["title"].as_str()),
                    abstract_: non_empty(w["abstract"].as_str()),
                    authors: w["authors"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x["name"].as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default(),
                    year: w["yearPublished"].as_i64().map(|y| y as i32),
                    venue: w["journals"]
                        .as_array()
                        .and_then(|j| j.first())
                        .and_then(|j| non_empty(j["title"].as_str()))
                        .or_else(|| non_empty(w["publisher"].as_str())),
                    pdf_url: non_empty(w["downloadUrl"].as_str()),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_http::{MockServer, Reply};

    const FIXTURE: &str = include_str!("../../tests/fixtures/sources/core_search.json");

    #[test]
    fn renders_title_abstract_fields_and_notes_simplifications() {
        let p = crate::agents::protocol::tests::sample_protocol();
        let ast = crate::query::QueryAst::from_protocol(&p, &Default::default());
        let r = Core::new(reqwest::Client::new(), None, "k".into()).render(&ast);
        assert!(r.query.contains("title:\"Chinese international students\" OR abstract:\"Chinese international students\""));
        assert!(r
            .query
            .contains("yearPublished>=2010 AND yearPublished<=2026"));
        assert!(!r.query.contains("subject:"));
        assert!(r
            .simplifications
            .iter()
            .any(|s| s.contains("Foreign Students")));
        assert!(r.simplifications.iter().any(|s| s.contains("Truncation")));
    }

    #[test]
    fn normalises_recorded_response() {
        let raw: Value = serde_json::from_str(FIXTURE).unwrap();
        let recs = Core::new(reqwest::Client::new(), None, "k".into()).normalise(&raw);
        assert_eq!(recs.len(), 2);
        assert!(recs[0]
            .title
            .as_deref()
            .unwrap()
            .starts_with("Study on transformative learning"));
        assert_eq!(recs[0].year, Some(2018));
        assert_eq!(
            recs[0].pdf_url.as_deref(),
            Some("https://core.ac.uk/download/429734608.pdf")
        );
        assert_eq!(recs[0].doi, None, "\"None\" DOIs are dropped");
        assert_eq!(recs[0].authors, vec!["Wang, Yiran".to_string()]);
    }

    #[tokio::test]
    async fn sends_bearer_key_to_trailing_slash_path_and_strips_full_text() {
        let body = serde_json::json!({"totalHits": 1, "results": [{"id": 1, "title": "t", "fullText": "x".repeat(5000)}]});
        let server = MockServer::start(vec![Reply::json(200, body.to_string())]).await;
        let c = Core::new(
            reqwest::Client::new(),
            Some(server.base()),
            "core-key".into(),
        );
        let page = c
            .fetch_page(
                &Rendered {
                    query: "title:x".into(),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(page.next_cursor, None);
        assert_eq!(
            page.raw["results"][0]["fullText"],
            "[5000 characters omitted]"
        );
        let r = &server.requests()[0];
        assert!(r.path.starts_with("/v3/search/works/?"));
        assert_eq!(r.header("authorization"), Some("Bearer core-key"));
    }
}
