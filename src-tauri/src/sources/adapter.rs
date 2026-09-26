//! The source adapter trait (§7) and shared record normalisation.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::llm::http::{self as llm_http, RetryPolicy};
use crate::query::{QueryAst, Rendered};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NormalisedRecord {
    pub source_id: Option<String>,
    pub doi: Option<String>,
    pub title: Option<String>,
    pub abstract_: Option<String>,
    pub authors: Vec<String>,
    pub year: Option<i32>,
    pub venue: Option<String>,
    pub pdf_url: Option<String>,
    pub landing_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Page {
    pub raw: Value,
    pub total: Option<u64>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("{0}")]
    Retryable(String),
    #[error("{0}")]
    Auth(String),
    #[error("{0}")]
    Fatal(String),
}

pub type SourceResult<T> = std::result::Result<T, SourceError>;

#[async_trait]
pub trait SourceAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn render(&self, ast: &QueryAst) -> Rendered;
    async fn count(&self, q: &Rendered) -> SourceResult<u64>;
    async fn fetch_page(&self, q: &Rendered, cursor: Option<String>) -> SourceResult<Page>;
    fn normalise(&self, raw: &Value) -> Vec<NormalisedRecord>;
    /// Records per page this adapter requests.
    fn page_size(&self) -> u32;
}

/// GET with the shared retry policy; maps statuses to source errors.
pub async fn get_json(
    client: &reqwest::Client,
    url: &str,
    build: impl Fn() -> reqwest::RequestBuilder,
) -> SourceResult<Value> {
    let policy = RetryPolicy {
        max_retries: 4,
        ..RetryPolicy::default()
    };
    get_json_with(&policy, client, url, build).await
}

/// For optional lookups (full-text retrieval): one quick retry, then give up
/// rather than wait out a rate limit.
pub const QUICK: RetryPolicy = RetryPolicy {
    max_retries: 1,
    base_delay: std::time::Duration::from_secs(1),
    max_delay: std::time::Duration::from_secs(5),
};

pub async fn get_json_with(
    policy: &RetryPolicy,
    client: &reqwest::Client,
    url: &str,
    build: impl Fn() -> reqwest::RequestBuilder,
) -> SourceResult<Value> {
    let shown = url.split('?').next().unwrap_or(url).to_owned();
    let _ = client;
    let reply = llm_http::send(policy, &shown, build)
        .await
        .map_err(|e| SourceError::Retryable(e.to_string()))?;
    match reply.status {
        200..=299 => serde_json::from_str(&reply.body).map_err(|e| {
            SourceError::Fatal(format!("{shown} returned something other than JSON: {e}"))
        }),
        401 | 403 => Err(SourceError::Auth(format!(
            "{shown} rejected the key (HTTP {}).",
            reply.status
        ))),
        429 | 500..=599 => Err(SourceError::Retryable(format!(
            "{shown} answered HTTP {}: {}",
            reply.status,
            crate::llm::error_message(&reply.body)
        ))),
        s => Err(SourceError::Fatal(format!(
            "{shown} answered HTTP {s}: {}",
            crate::llm::error_message(&reply.body)
        ))),
    }
}

/// Lower-cases a DOI and strips resolver prefixes.
pub fn clean_doi(raw: &str) -> Option<String> {
    let d = raw.trim().to_ascii_lowercase();
    let d = d
        .trim_start_matches("https://doi.org/")
        .trim_start_matches("http://doi.org/")
        .trim_start_matches("https://dx.doi.org/")
        .trim_start_matches("doi:")
        .trim();
    (d.starts_with("10.") && d.contains('/')).then(|| d.to_owned())
}

/// Decodes the HTML entities ERIC puts in titles and abstracts.
pub fn unescape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(end) = tail.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn non_empty(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|s| !s.is_empty() && *s != "None")
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dois_are_cleaned() {
        assert_eq!(
            clean_doi("https://doi.org/10.1080/03075079.2016.1152466").as_deref(),
            Some("10.1080/03075079.2016.1152466")
        );
        assert_eq!(clean_doi("DOI:10.1/ABC").as_deref(), Some("10.1/abc"));
        assert_eq!(clean_doi("None"), None);
    }

    #[test]
    fn html_entities_decode() {
        assert_eq!(
            unescape_html("Portraying the &apos;Chinese&apos; &amp; more &#8211; &#x2019;x"),
            "Portraying the 'Chinese' & more – ’x"
        );
        assert_eq!(unescape_html("AT&T & co"), "AT&T & co");
    }
}
