//! Semantic Scholar adapter (bulk paper search).
//!
//! Checked 2026-09-24 against https://api.semanticscholar.org/api-docs/ and the tutorial at
//! https://www.semanticscholar.org/product/api/tutorial:
//! - `GET https://api.semanticscholar.org/graph/v1/paper/search/bulk?query=&fields=&year=&token=`,
//!   optional `x-api-key` header. Keyless calls share a public pool and are often rate-limited (429).
//! - Query syntax: `+` AND, `|` OR, quoted phrases, trailing `*` prefix match, parentheses.
//! - Response: `total`, `token` (next page), `data[]` with `paperId`, `externalIds.DOI`, `title`,
//!   `abstract`, `authors[].name`, `year`, `venue`, `openAccessPdf.url`.
//!
//! No recorded fixture yet: the live keyless call returned 429 on 2026-09-24. Tests use the documented shape.

use async_trait::async_trait;
use serde_json::Value;

use super::adapter::{
    clean_doi, get_json, non_empty, NormalisedRecord, Page, SourceAdapter, SourceResult,
};
use crate::query::{Field, QueryAst, Rendered, TermKind};

const FIELDS: &str = "title,abstract,authors,year,venue,externalIds,openAccessPdf,url";

pub struct SemanticScholar {
    pub client: reqwest::Client,
    pub base: String,
    pub key: Option<String>,
}

impl SemanticScholar {
    pub fn new(client: reqwest::Client, base: Option<&str>, key: Option<String>) -> Self {
        Self {
            client,
            base: base
                .unwrap_or("https://api.semanticscholar.org")
                .trim_end_matches('/')
                .to_owned(),
            key: key.filter(|k| !k.is_empty()),
        }
    }

    fn req(&self, q: &Rendered, token: Option<&str>) -> reqwest::RequestBuilder {
        let mut params: Vec<(&str, String)> =
            vec![("query", q.query.clone()), ("fields", FIELDS.into())];
        for (k, v) in &q.params {
            params.push((k.as_str(), v.clone()));
        }
        if let Some(t) = token {
            params.push(("token", t.to_owned()));
        }
        let rb = self
            .client
            .get(format!("{}/graph/v1/paper/search/bulk", self.base))
            .timeout(std::time::Duration::from_secs(60))
            .query(&params);
        match &self.key {
            Some(k) => rb.header("x-api-key", k),
            None => rb,
        }
    }
}

#[async_trait]
impl SourceAdapter for SemanticScholar {
    fn id(&self) -> &'static str {
        "semantic_scholar"
    }

    fn page_size(&self) -> u32 {
        1000
    }

    fn render(&self, ast: &QueryAst) -> Rendered {
        let mut simpl: Vec<String> = Vec::new();
        let concepts: Vec<String> = ast
            .concepts
            .iter()
            .map(|c| {
                let terms: Vec<String> = c
                    .terms
                    .iter()
                    .map(|t| {
                        if t.fields.contains(&Field::Subject) {
                            let s = "Thesaurus terms searched as free text.".to_owned();
                            if !simpl.contains(&s) {
                                simpl.push(s);
                            }
                        }
                        match t.kind {
                            TermKind::Phrase => format!("\"{}\"", t.text.replace('*', "")),
                            TermKind::Word => t.text.clone(),
                            TermKind::Truncated => format!("{}*", t.text),
                        }
                    })
                    .collect();
                format!("({})", terms.join(" | "))
            })
            .collect();
        let mut params = Vec::new();
        if let (Some(a), Some(b)) = (ast.filters.year_from, ast.filters.year_to) {
            params.push(("year".to_owned(), format!("{a}-{b}")));
        }
        if !ast.filters.languages.is_empty() {
            simpl.push("Language not filtered: Semantic Scholar has no language filter.".into());
        }
        simpl.push(
            "Searches titles and abstracts together; field restriction is not available.".into(),
        );
        simpl.extend(ast.notes.iter().cloned());
        Rendered {
            query: concepts.join(" + "),
            params,
            simplifications: simpl,
        }
    }

    async fn count(&self, q: &Rendered) -> SourceResult<u64> {
        let v = get_json(
            &self.client,
            &format!("{}/graph/v1/paper/search/bulk", self.base),
            || self.req(q, None),
        )
        .await?;
        Ok(v["total"].as_u64().unwrap_or(0))
    }

    async fn fetch_page(&self, q: &Rendered, cursor: Option<String>) -> SourceResult<Page> {
        let raw = get_json(
            &self.client,
            &format!("{}/graph/v1/paper/search/bulk", self.base),
            || self.req(q, cursor.as_deref()),
        )
        .await?;
        let total = raw["total"].as_u64();
        let next_cursor = non_empty(raw["token"].as_str());
        Ok(Page {
            raw,
            total,
            next_cursor,
        })
    }

    fn normalise(&self, raw: &Value) -> Vec<NormalisedRecord> {
        let Some(data) = raw["data"].as_array() else {
            return Vec::new();
        };
        data.iter()
            .map(|p| NormalisedRecord {
                source_id: non_empty(p["paperId"].as_str()),
                doi: p
                    .pointer("/externalIds/DOI")
                    .and_then(Value::as_str)
                    .and_then(clean_doi),
                title: non_empty(p["title"].as_str()),
                abstract_: non_empty(p["abstract"].as_str()),
                authors: p["authors"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x["name"].as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
                year: p["year"].as_i64().map(|y| y as i32),
                venue: non_empty(p["venue"].as_str()),
                pdf_url: p
                    .pointer("/openAccessPdf/url")
                    .and_then(Value::as_str)
                    .and_then(|u| non_empty(Some(u))),
                landing_url: non_empty(p["url"].as_str()),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_documented_bulk_syntax() {
        let p = crate::agents::protocol::tests::sample_protocol();
        let ast = crate::query::QueryAst::from_protocol(&p, &Default::default());
        let r = SemanticScholar::new(reqwest::Client::new(), None, None).render(&ast);
        assert!(r
            .query
            .contains("(\"Chinese international students\" | \"Chinese students\""));
        assert!(r.query.contains(" + (experience* | belong*)"));
        assert_eq!(r.params, vec![("year".to_owned(), "2010-2026".to_owned())]);
    }

    #[test]
    fn normalises_documented_shape() {
        let raw = serde_json::json!({"total": 1, "token": null, "data": [{
            "paperId": "abc", "externalIds": {"DOI": "10.1/X"}, "title": "T", "abstract": "A",
            "authors": [{"name": "Li, M."}], "year": 2020, "venue": "V", "openAccessPdf": {"url": "https://x/p.pdf"}
        }]});
        let r = &SemanticScholar::new(reqwest::Client::new(), None, None).normalise(&raw)[0];
        assert_eq!(r.doi.as_deref(), Some("10.1/x"));
        assert_eq!(r.pdf_url.as_deref(), Some("https://x/p.pdf"));
    }
}
