//! One small authenticated request per source to check a key works.
//!
//! Endpoints and auth (checked 2026-09-24):
//! - ERIC: `GET https://api.ies.ed.gov/eric/?search=&format=json&rows=` -> `response.numFound`
//!   (https://eric.ed.gov/?api). No key.
//! - OpenAlex: `GET https://api.openalex.org/works?per-page=1`, key as
//!   `Authorization: Bearer` (https://help.openalex.org/api/authentication/) -> `meta.count`.
//! - Web of Science Starter: `GET https://api.clarivate.com/apis/wos-starter/v1/documents?q=&limit=`
//!   with `X-ApiKey` (developer.clarivate.com, clarivate/wosstarter_python_client) -> `metadata.total`.
//! - Scopus: `GET https://api.elsevier.com/content/search/scopus?query=&count=` with
//!   `X-ELS-APIKey`, optional `X-ELS-Insttoken`, `Accept: application/json` -> `search-results`.
//! - PubMed: `GET https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esearch.fcgi?db=pubmed&retmode=json`
//!   with optional `api_key` query parameter (NCBI accepts no header) -> `esearchresult`.
//! - Semantic Scholar: `GET https://api.semanticscholar.org/graph/v1/paper/search?query=&limit=`
//!   with optional `x-api-key` (https://api.semanticscholar.org/api-docs/) -> `total`.
//! - CORE: `GET https://api.core.ac.uk/v3/search/works?q=&limit=` with `Authorization: Bearer`
//!   (https://core.ac.uk/services/api) -> `totalHits`.
//! - Unpaywall: `GET https://api.unpaywall.org/v2/{doi}?email=` (https://unpaywall.org/products/api) -> `doi`.
//! - Elsevier: `GET https://api.elsevier.com/content/serial/title?issn=` with `X-ELS-APIKey`
//!   (Serial Title API, dev.elsevier.com) -> `serial-metadata-response`.
//! - Wiley TDM: `GET https://api.wiley.com/onlinelibrary/tdm/v1/articles/{url-encoded doi}` with
//!   `Wiley-TDM-Client-Token` (github.com/WileyLabs/tdm-client). An unregistered token gets 403
//!   "invalid and not registered"; a valid token without a subscription gets a different 403.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::llm::http::{self as llm_http, RetryPolicy};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceTestResult {
    pub ok: bool,
    pub status: Option<u16>,
    pub message: String,
}

/// Credentials for one test. Values are read from the keychain by the caller.
#[derive(Default)]
pub struct Credentials {
    pub key: Option<String>,
    pub inst_token: Option<String>,
    pub email: Option<String>,
}

pub fn default_base(id: &str) -> Option<&'static str> {
    Some(match id {
        "eric" => "https://api.ies.ed.gov",
        "openalex" => "https://api.openalex.org",
        "wos" => "https://api.clarivate.com",
        "scopus" => "https://api.elsevier.com",
        "pubmed" => "https://eutils.ncbi.nlm.nih.gov",
        "semantic_scholar" => "https://api.semanticscholar.org",
        "core" => "https://api.core.ac.uk",
        "unpaywall" => "https://api.unpaywall.org",
        "elsevier" => "https://api.elsevier.com",
        "wiley" => "https://api.wiley.com",
        _ => return None,
    })
}

/// A well-known open-access DOI used only for the Unpaywall check.
const UNPAYWALL_TEST_DOI: &str = "10.1038/nature12373";

pub async fn test_source(
    client: &reqwest::Client,
    id: &str,
    creds: &Credentials,
    base: Option<&str>,
) -> SourceTestResult {
    let Some(base) = base.or_else(|| default_base(id)) else {
        return fail(None, format!("Unknown source {id}."));
    };
    let key = creds.key.as_deref().filter(|k| !k.is_empty());
    let needs_key = matches!(id, "wos" | "scopus" | "core");
    if needs_key && key.is_none() {
        return fail(None, "Add a key first.".into());
    }
    let (url, expect): (String, &str) = match id {
        "eric" => (
            format!("{base}/eric/?search=education&format=json&rows=1"),
            "/response/numFound",
        ),
        "openalex" => (format!("{base}/works?per-page=1"), "/meta/count"),
        "wos" => (
            format!("{base}/apis/wos-starter/v1/documents?q=PY%3D2020&limit=1"),
            "/metadata/total",
        ),
        "scopus" => (
            format!("{base}/content/search/scopus?query=TITLE-ABS-KEY%28education%29&count=1"),
            "/search-results",
        ),
        "pubmed" => {
            let mut u = format!(
                "{base}/entrez/eutils/esearch.fcgi?db=pubmed&term=education&retmax=0&retmode=json"
            );
            if let Some(k) = key {
                u.push_str("&api_key=");
                u.push_str(k);
            }
            (u, "/esearchresult")
        }
        "semantic_scholar" => (
            format!("{base}/graph/v1/paper/search?query=education&limit=1&fields=title"),
            "/total",
        ),
        "core" => (
            format!("{base}/v3/search/works?q=education&limit=1"),
            "/totalHits",
        ),
        "unpaywall" => {
            let Some(email) = creds.email.as_deref().filter(|e| !e.is_empty()) else {
                return fail(None, "Add your email address first.".into());
            };
            (
                format!("{base}/v2/{UNPAYWALL_TEST_DOI}?email={email}"),
                "/doi",
            )
        }
        "elsevier" => (
            format!("{base}/content/serial/title?issn=0360-1315"),
            "/serial-metadata-response",
        ),
        // Wiley's own example article (text-and-datamining page).
        "wiley" => (
            format!("{base}/onlinelibrary/tdm/v1/articles/10.1111%2F1467-923X.12168"),
            "",
        ),
        other => return fail(None, format!("Unknown source {other}.")),
    };

    let build = || {
        let mut rb = client
            .get(&url)
            .timeout(Duration::from_secs(30))
            .header("accept", "application/json");
        match (id, key) {
            ("openalex" | "core", Some(k)) => rb = rb.bearer_auth(k),
            ("wos", Some(k)) => rb = rb.header("X-ApiKey", k),
            ("scopus", Some(k)) => {
                rb = rb.header("X-ELS-APIKey", k);
                if let Some(t) = creds.inst_token.as_deref().filter(|t| !t.is_empty()) {
                    rb = rb.header("X-ELS-Insttoken", t);
                }
            }
            ("semantic_scholar", Some(k)) => rb = rb.header("x-api-key", k),
            ("elsevier", Some(k)) => rb = rb.header("X-ELS-APIKey", k),
            ("wiley", Some(k)) => rb = rb.header("Wiley-TDM-Client-Token", k),
            _ => {}
        }
        rb
    };
    // One retry at most: this is an interactive check, not a crawl.
    let policy = RetryPolicy {
        max_retries: 1,
        ..RetryPolicy::default()
    };
    // The display URL never includes a key (PubMed carries it in the query string).
    let shown = url.split('?').next().unwrap_or(&url).to_owned();
    let reply = match llm_http::send(&policy, &shown, build).await {
        Ok(r) => r,
        Err(e) => return fail(None, e.to_string()),
    };
    let status = reply.status;
    if id == "wiley" {
        return wiley_verdict(status, &reply.body);
    }
    match status {
        200..=299 => {
            let parsed: Option<Value> = serde_json::from_str(&reply.body).ok();
            if parsed.as_ref().and_then(|v| v.pointer(expect)).is_some() {
                SourceTestResult {
                    ok: true,
                    status: Some(status),
                    message: if key.is_some() {
                        "Key accepted.".into()
                    } else {
                        "Reachable.".into()
                    },
                }
            } else {
                fail(
                    Some(status),
                    "The service answered, but not in the expected format.".into(),
                )
            }
        }
        401 | 403 => fail(Some(status), "The key was rejected.".into()),
        429 => fail(
            Some(status),
            "Rate limited. The key may be fine; try again later.".into(),
        ),
        _ => fail(
            Some(status),
            format!("HTTP {status}: {}", crate::llm::error_message(&reply.body)),
        ),
    }
}

/// Wiley answers the test article with a PDF (subscribed), a 403 naming an
/// unregistered token (bad key), or another 403 (valid token, no subscription).
fn wiley_verdict(status: u16, body: &str) -> SourceTestResult {
    let lower = body.to_ascii_lowercase();
    match status {
        200..=299 => SourceTestResult {
            ok: true,
            status: Some(status),
            message: "Token accepted; the test article downloaded.".into(),
        },
        400 => fail(Some(status), "No token was sent.".into()),
        403 if lower.contains("not registered") || lower.contains("invalid") => {
            fail(Some(status), "The token was rejected.".into())
        }
        403 => SourceTestResult {
            ok: true,
            status: Some(status),
            message: "Token accepted. The test article is not in your subscription from this network, which is normal off campus or off VPN.".into(),
        },
        429 => fail(Some(status), "Rate limited. Try again in a minute.".into()),
        _ => fail(Some(status), format!("HTTP {status}")),
    }
}

fn fail(status: Option<u16>, message: String) -> SourceTestResult {
    SourceTestResult {
        ok: false,
        status,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_http::{MockServer, Reply};

    #[test]
    fn wiley_token_verdicts() {
        assert!(wiley_verdict(200, "%PDF").ok);
        assert!(
            !wiley_verdict(
                403,
                "TDM Client Token that you have provided is invalid and not registered"
            )
            .ok
        );
        assert!(
            wiley_verdict(403, "Access denied").ok,
            "valid token, no subscription"
        );
        assert!(!wiley_verdict(400, "No TDM Client Token was found in the request").ok);
    }

    fn creds(key: &str) -> Credentials {
        Credentials {
            key: Some(key.into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn eric_needs_no_key() {
        let server = MockServer::start(vec![Reply::json(
            200,
            r#"{"response":{"numFound":123,"docs":[]}}"#,
        )])
        .await;
        let r = test_source(
            &reqwest::Client::new(),
            "eric",
            &Credentials::default(),
            Some(server.base()),
        )
        .await;
        assert!(r.ok, "{r:?}");
        assert!(server.requests()[0].path.starts_with("/eric/?search="));
    }

    #[tokio::test]
    async fn each_keyed_source_sends_its_documented_header() {
        let cases = [
            (
                "openalex",
                r#"{"meta":{"count":1}}"#,
                "authorization",
                "Bearer k-123",
            ),
            (
                "core",
                r#"{"totalHits":5}"#,
                "authorization",
                "Bearer k-123",
            ),
            ("wos", r#"{"metadata":{"total":9}}"#, "x-apikey", "k-123"),
            (
                "scopus",
                r#"{"search-results":{}}"#,
                "x-els-apikey",
                "k-123",
            ),
            (
                "semantic_scholar",
                r#"{"total":1,"data":[]}"#,
                "x-api-key",
                "k-123",
            ),
        ];
        for (id, body, header, value) in cases {
            let server = MockServer::start(vec![Reply::json(200, body)]).await;
            let r = test_source(
                &reqwest::Client::new(),
                id,
                &creds("k-123"),
                Some(server.base()),
            )
            .await;
            assert!(r.ok, "{id}: {r:?}");
            assert_eq!(server.requests()[0].header(header), Some(value), "{id}");
        }
    }

    #[tokio::test]
    async fn scopus_sends_institution_token_when_set() {
        let server = MockServer::start(vec![Reply::json(200, r#"{"search-results":{}}"#)]).await;
        let c = Credentials {
            key: Some("k".into()),
            inst_token: Some("tok".into()),
            email: None,
        };
        test_source(&reqwest::Client::new(), "scopus", &c, Some(server.base())).await;
        assert_eq!(server.requests()[0].header("x-els-insttoken"), Some("tok"));
    }

    #[tokio::test]
    async fn pubmed_key_goes_in_query_and_is_optional() {
        let server =
            MockServer::start(vec![Reply::json(200, r#"{"esearchresult":{"count":"1"}}"#)]).await;
        assert!(
            test_source(
                &reqwest::Client::new(),
                "pubmed",
                &Credentials::default(),
                Some(server.base())
            )
            .await
            .ok
        );
        assert!(
            test_source(
                &reqwest::Client::new(),
                "pubmed",
                &creds("pm-key"),
                Some(server.base())
            )
            .await
            .ok
        );
        let reqs = server.requests();
        assert!(!reqs[0].path.contains("api_key"));
        assert!(reqs[1].path.contains("api_key=pm-key"));
    }

    #[tokio::test]
    async fn rejected_key_and_missing_key_are_explained() {
        let server = MockServer::start(vec![Reply::json(403, r#"{"message":"Forbidden"}"#)]).await;
        let r = test_source(
            &reqwest::Client::new(),
            "wos",
            &creds("bad"),
            Some(server.base()),
        )
        .await;
        assert_eq!(
            (r.ok, r.status, r.message.as_str()),
            (false, Some(403), "The key was rejected.")
        );

        let r = test_source(
            &reqwest::Client::new(),
            "core",
            &Credentials::default(),
            Some(server.base()),
        )
        .await;
        assert_eq!(r.message, "Add a key first.");
    }

    #[tokio::test]
    async fn unpaywall_uses_email_and_known_doi() {
        let server = MockServer::start(vec![Reply::json(
            200,
            r#"{"doi":"10.1038/nature12373","is_oa":true}"#,
        )])
        .await;
        let c = Credentials {
            email: Some("me@uni.ac.uk".into()),
            ..Default::default()
        };
        assert!(
            test_source(
                &reqwest::Client::new(),
                "unpaywall",
                &c,
                Some(server.base())
            )
            .await
            .ok
        );
        assert_eq!(
            server.requests()[0].path,
            "/v2/10.1038/nature12373?email=me@uni.ac.uk"
        );
    }

    #[tokio::test]
    async fn unexpected_shape_is_not_reported_as_success() {
        let server = MockServer::start(vec![Reply::json(200, r#"<html>login</html>"#)]).await;
        let r = test_source(
            &reqwest::Client::new(),
            "core",
            &creds("k"),
            Some(server.base()),
        )
        .await;
        assert!(!r.ok);
    }
}
