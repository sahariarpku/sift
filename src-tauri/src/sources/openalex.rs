//! OpenAlex adapter: search, and citation chasing (§8.6).
//!
//! Checked 2026-09-24 against https://help.openalex.org/api/authentication/ and live
//! keyless requests (fixture `tests/fixtures/sources/openalex_works.json`):
//! - `GET https://api.openalex.org/works?filter=&per-page=&cursor=`. Key as
//!   `Authorization: Bearer` (optional: keyless calls work on a smaller free budget;
//!   each response reports `meta.cost_usd`).
//! - `filter=title_and_abstract.search:(...)` accepts quoted phrases and AND/OR
//!   (verified live; the API echoes the parsed query in `meta.x_query`). Search is
//!   stemmed, so `*` truncation is not needed. Filters are comma-separated, so commas
//!   inside terms are removed.
//! - `publication_year:2010-2026`, `language:en`. Cursor paging: `cursor=*`, then `meta.next_cursor`.
//! - Works: `id`, `doi`, `title`, `abstract_inverted_index`, `authorships[].author.display_name`,
//!   `publication_year`, `primary_location.source.display_name`, `best_oa_location.pdf_url`,
//!   `referenced_works[]`.
//! - Citation chasing: backward via `referenced_works`, forward via `filter=cites:W...`;
//!   batch lookup via `filter=openalex:W1|W2|...`.

use async_trait::async_trait;
use serde_json::Value;

use super::adapter::{
    clean_doi, get_json, non_empty, NormalisedRecord, Page, SourceAdapter, SourceResult,
};
use crate::query::{language_code, Field, QueryAst, Rendered, TermKind};

pub struct OpenAlex {
    pub client: reqwest::Client,
    pub base: String,
    pub key: Option<String>,
}

impl OpenAlex {
    pub fn new(client: reqwest::Client, base: Option<&str>, key: Option<String>) -> Self {
        Self {
            client,
            base: base
                .unwrap_or("https://api.openalex.org")
                .trim_end_matches('/')
                .to_owned(),
            key: key.filter(|k| !k.is_empty()),
        }
    }

    fn req(&self, params: &[(&str, String)]) -> reqwest::RequestBuilder {
        let rb = self
            .client
            .get(format!("{}/works", self.base))
            .timeout(std::time::Duration::from_secs(60))
            .query(params);
        match &self.key {
            Some(k) => rb.bearer_auth(k),
            None => rb,
        }
    }

    fn filter(q: &Rendered) -> String {
        let mut f = vec![format!("title_and_abstract.search:{}", q.query)];
        f.extend(
            q.params
                .iter()
                .filter(|(k, _)| k == "filter")
                .map(|(_, v)| v.clone()),
        );
        f.join(",")
    }

    async fn works(&self, params: &[(&str, String)]) -> SourceResult<Value> {
        get_json(&self.client, &format!("{}/works", self.base), || {
            self.req(params)
        })
        .await
    }

    /// Looks a work up by DOI. Returns its raw JSON.
    pub async fn by_doi(&self, doi: &str) -> SourceResult<Option<Value>> {
        let v = self
            .works(&[("filter", format!("doi:{doi}")), ("per-page", "1".into())])
            .await?;
        Ok(v["results"].as_array().and_then(|a| a.first()).cloned())
    }

    /// The DOI of the work with this exact title (Jaro-Winkler >= 0.95 on the
    /// normalised title) published within a year of `year`, if OpenAlex has one.
    pub async fn doi_by_title(
        &self,
        title: &str,
        year: Option<i32>,
    ) -> SourceResult<Option<String>> {
        // The filter syntax separates filters with commas and values with colons.
        let clean: String = title
            .chars()
            .map(|c| {
                if matches!(c, ',' | ':' | '|' | '"') {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        let mut filter = format!(
            "title.search:{}",
            clean.split_whitespace().collect::<Vec<_>>().join(" ")
        );
        if let Some(y) = year {
            filter.push_str(&format!(",publication_year:{}-{}", y - 1, y + 1));
        }
        let v = self
            .works(&[("filter", filter), ("per-page", "5".into())])
            .await?;
        Ok(best_title_match(&v, title, year))
    }

    /// Works citing `id` (forward citations), up to `max`.
    pub async fn citing(&self, id: &str, max: usize) -> SourceResult<Vec<Value>> {
        let mut out = Vec::new();
        let mut cursor = "*".to_owned();
        while out.len() < max {
            let v = self
                .works(&[
                    ("filter", format!("cites:{id}")),
                    ("per-page", "200".into()),
                    ("cursor", cursor.clone()),
                ])
                .await?;
            let batch = v["results"].as_array().cloned().unwrap_or_default();
            if batch.is_empty() {
                break;
            }
            out.extend(batch);
            match v.pointer("/meta/next_cursor").and_then(Value::as_str) {
                Some(c) => cursor = c.to_owned(),
                None => break,
            }
        }
        out.truncate(max);
        Ok(out)
    }

    /// Fetches works by OpenAlex id, 50 per request.
    pub async fn by_ids(&self, ids: &[String]) -> SourceResult<Vec<Value>> {
        let mut out = Vec::new();
        for chunk in ids.chunks(50) {
            let short: Vec<&str> = chunk
                .iter()
                .map(|i| i.rsplit('/').next().unwrap_or(i))
                .collect();
            let v = self
                .works(&[
                    ("filter", format!("openalex:{}", short.join("|"))),
                    ("per-page", "50".into()),
                ])
                .await?;
            out.extend(v["results"].as_array().cloned().unwrap_or_default());
        }
        Ok(out)
    }

    pub fn normalise_work(w: &Value) -> NormalisedRecord {
        NormalisedRecord {
            source_id: non_empty(w["id"].as_str())
                .map(|s| s.rsplit('/').next().unwrap_or(&s).to_owned()),
            doi: w["doi"].as_str().and_then(clean_doi),
            title: non_empty(w["title"].as_str()).or_else(|| non_empty(w["display_name"].as_str())),
            abstract_: reconstruct_abstract(&w["abstract_inverted_index"]),
            authors: w["authorships"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| {
                            x.pointer("/author/display_name")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .collect()
                })
                .unwrap_or_default(),
            year: w["publication_year"].as_i64().map(|y| y as i32),
            venue: w
                .pointer("/primary_location/source/display_name")
                .and_then(Value::as_str)
                .map(str::to_owned),
            pdf_url: w
                .pointer("/best_oa_location/pdf_url")
                .and_then(Value::as_str)
                .map(str::to_owned),
            landing_url: w
                .pointer("/primary_location/landing_page_url")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| w["doi"].as_str().map(str::to_owned)),
        }
    }
}

/// Rebuilds abstract text from OpenAlex's inverted index (word -> positions).
/// Picks the DOI of the search result whose title matches exactly (after normalising).
pub fn best_title_match(v: &Value, title: &str, year: Option<i32>) -> Option<String> {
    let want = crate::dedup::normalise_title(title);
    if want.split(' ').count() < 4 {
        return None;
    }
    v["results"].as_array()?.iter().find_map(|w| {
        let t = crate::dedup::normalise_title(w["title"].as_str()?);
        let y = w["publication_year"].as_i64().map(|y| y as i32);
        let year_ok = match (year, y) {
            (Some(a), Some(b)) => (a - b).abs() <= 1,
            _ => true,
        };
        (year_ok && strsim::jaro_winkler(&t, &want) >= 0.95)
            .then(|| {
                w["doi"]
                    .as_str()
                    .and_then(crate::sources::adapter::clean_doi)
            })
            .flatten()
    })
}

pub fn reconstruct_abstract(index: &Value) -> Option<String> {
    let map = index.as_object()?;
    let mut words: Vec<(u64, &str)> = Vec::new();
    for (word, positions) in map {
        for p in positions.as_array()? {
            words.push((p.as_u64()?, word.as_str()));
        }
    }
    if words.is_empty() {
        return None;
    }
    words.sort_by_key(|w| w.0);
    Some(words.into_iter().map(|w| w.1).collect::<Vec<_>>().join(" "))
}

#[async_trait]
impl SourceAdapter for OpenAlex {
    fn id(&self) -> &'static str {
        "openalex"
    }

    fn page_size(&self) -> u32 {
        200
    }

    fn render(&self, ast: &QueryAst) -> Rendered {
        let mut simpl: Vec<String> = Vec::new();
        let mut note = |s: String| {
            if !simpl.contains(&s) {
                simpl.push(s);
            }
        };
        let mut concepts = Vec::new();
        for c in &ast.concepts {
            let mut terms = Vec::new();
            for t in &c.terms {
                if t.fields.contains(&Field::Subject) {
                    note(format!(
                        "Thesaurus term \"{}\" searched as free text in titles and abstracts.",
                        t.text
                    ));
                }
                let clean = t
                    .text
                    .replace([',', '*'], " ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                match t.kind {
                    TermKind::Truncated => {
                        note("Truncation (*) not used: OpenAlex search is stemmed.".into());
                        terms.push(clean);
                    }
                    TermKind::Phrase => terms.push(format!("\"{clean}\"")),
                    TermKind::Word => terms.push(clean),
                }
            }
            if !terms.is_empty() {
                concepts.push(format!("({})", terms.join(" OR ")));
            }
        }
        let mut params = Vec::new();
        if let (Some(a), Some(b)) = (ast.filters.year_from, ast.filters.year_to) {
            params.push(("filter".to_owned(), format!("publication_year:{a}-{b}")));
        }
        let codes: Vec<&str> = ast
            .filters
            .languages
            .iter()
            .filter_map(|l| language_code(l))
            .collect();
        if !codes.is_empty() {
            params.push(("filter".to_owned(), format!("language:{}", codes.join("|"))));
        }
        simpl.extend(ast.notes.iter().cloned());
        Rendered {
            query: format!("({})", concepts.join(" AND ")),
            params,
            simplifications: simpl,
        }
    }

    async fn count(&self, q: &Rendered) -> SourceResult<u64> {
        let v = self
            .works(&[("filter", Self::filter(q)), ("per-page", "1".into())])
            .await?;
        Ok(v.pointer("/meta/count")
            .and_then(Value::as_u64)
            .unwrap_or(0))
    }

    async fn fetch_page(&self, q: &Rendered, cursor: Option<String>) -> SourceResult<Page> {
        let raw = self
            .works(&[
                ("filter", Self::filter(q)),
                ("per-page", self.page_size().to_string()),
                ("cursor", cursor.unwrap_or_else(|| "*".into())),
            ])
            .await?;
        let total = raw.pointer("/meta/count").and_then(Value::as_u64);
        let has = raw["results"].as_array().is_some_and(|a| !a.is_empty());
        let next_cursor = raw
            .pointer("/meta/next_cursor")
            .and_then(Value::as_str)
            .filter(|_| has)
            .map(str::to_owned);
        Ok(Page {
            raw,
            total,
            next_cursor,
        })
    }

    fn normalise(&self, raw: &Value) -> Vec<NormalisedRecord> {
        raw["results"]
            .as_array()
            .map(|a| a.iter().map(Self::normalise_work).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn title_lookup_needs_an_exact_title_and_nearby_year() {
        let v = serde_json::json!({"results": [
            {"title": "Chinese students' perceptions of the effects of Western university examination formats", "publication_year": 2010, "doi": "https://doi.org/10.1111/j.1468-2273.2010.00462.x"},
            {"title": "Something else entirely about exams", "publication_year": 2010, "doi": "https://doi.org/10.1/x"}
        ]});
        let t = "Chinese Students' Perceptions of the Effects of Western University Examination Formats";
        assert_eq!(
            best_title_match(&v, t, Some(2010)).as_deref(),
            Some("10.1111/j.1468-2273.2010.00462.x")
        );
        assert_eq!(best_title_match(&v, t, Some(2015)), None, "year too far");
        assert_eq!(
            best_title_match(&v, "Exams", None),
            None,
            "too short to trust"
        );
    }
    use super::*;
    use crate::test_http::{MockServer, Reply};

    const FIXTURE: &str = include_str!("../../tests/fixtures/sources/openalex_works.json");

    #[test]
    fn renders_boolean_search_and_filters() {
        let p = crate::agents::protocol::tests::sample_protocol();
        let ast = crate::query::QueryAst::from_protocol(&p, &Default::default());
        let r = OpenAlex::new(reqwest::Client::new(), None, None).render(&ast);
        assert!(r
            .query
            .starts_with("((\"Chinese international students\" OR"));
        assert!(r.query.contains(" AND (experience OR belong)"));
        assert!(
            r.query.contains("\"British universit\""),
            "no * left inside phrases: {}",
            r.query
        );
        assert!(r
            .params
            .contains(&("filter".into(), "publication_year:2010-2026".into())));
        assert!(r.params.contains(&("filter".into(), "language:en".into())));
        assert!(r.simplifications.iter().any(|s| s.contains("stemmed")));
    }

    #[test]
    fn normalises_recorded_response() {
        let raw: Value = serde_json::from_str(FIXTURE).unwrap();
        let recs = OpenAlex::new(reqwest::Client::new(), None, None).normalise(&raw);
        assert_eq!(recs.len(), 1);
        let r = &recs[0];
        assert_eq!(r.source_id.as_deref(), Some("W2310935343"));
        assert_eq!(r.doi.as_deref(), Some("10.1080/03075079.2016.1152466"));
        assert!(r
            .title
            .as_deref()
            .unwrap()
            .contains("Chinese international students"));
        assert!(!r.authors.is_empty());
    }

    #[test]
    fn rebuilds_abstracts() {
        let idx = serde_json::json!({"belonging": [2], "Students": [0], "feel": [1]});
        assert_eq!(
            reconstruct_abstract(&idx).as_deref(),
            Some("Students feel belonging")
        );
        assert_eq!(reconstruct_abstract(&Value::Null), None);
    }

    #[tokio::test]
    async fn follows_cursor_and_sends_key_when_set() {
        let p1 = serde_json::json!({"meta": {"count": 2, "next_cursor": "abc"}, "results": [{"id": "https://openalex.org/W1"}]});
        let p2 = serde_json::json!({"meta": {"count": 2, "next_cursor": null}, "results": [{"id": "https://openalex.org/W2"}]});
        let server = MockServer::start(vec![
            Reply::json(200, p1.to_string()),
            Reply::json(200, p2.to_string()),
        ])
        .await;
        let oa = OpenAlex::new(
            reqwest::Client::new(),
            Some(server.base()),
            Some("oa-key".into()),
        );
        let q = Rendered {
            query: "(x)".into(),
            ..Default::default()
        };
        let a = oa.fetch_page(&q, None).await.unwrap();
        assert_eq!(a.next_cursor.as_deref(), Some("abc"));
        let b = oa.fetch_page(&q, a.next_cursor).await.unwrap();
        assert_eq!(b.next_cursor, None);
        let reqs = server.requests();
        assert!(reqs[0].path.contains("cursor=%2A") || reqs[0].path.contains("cursor=*"));
        assert!(reqs[1].path.contains("cursor=abc"));
        assert_eq!(reqs[0].header("authorization"), Some("Bearer oa-key"));
    }
}
