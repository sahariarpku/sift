//! Subscription full texts through the user's institution (§8.5).
//!
//! Tried after the open-access sources, only for DOIs:
//! 1. Publisher text-and-data-mining APIs, where the user has added a key.
//!    Entitlement is decided by the institution's IP range (campus network or VPN).
//!    - Elsevier Article Retrieval API: `GET https://api.elsevier.com/content/article/doi/{doi}`,
//!      header `X-ELS-APIKey`, `Accept: application/pdf`
//!      (https://dev.elsevier.com/documentation/ArticleRetrievalAPI.wadl, checked 2026-09-25).
//!    - Wiley TDM API: `GET https://api.wiley.com/onlinelibrary/tdm/v1/articles/{url-encoded doi}`,
//!      header `Wiley-TDM-Client-Token`; Wiley asks for about 10 s between requests
//!      (https://github.com/WileyLabs/tdm-client, checked 2026-09-25).
//! 2. Institutional access (opt-in setting): follow `https://doi.org/{doi}` to the
//!    publisher's page and fetch the PDF it declares in its `citation_pdf_url` meta
//!    tag (the Highwire tag publishers expose for indexing). Requests identify
//!    themselves as Sift and are spaced per host (2 s between PDFs from one host). Pages that refuse automated
//!    clients (HTTP 403, script challenges) are left for the browser-assisted
//!    upload; nothing tries to look like a browser.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::pipeline::env::Env;

const MAX_PDF_BYTES: usize = 60 * 1024 * 1024;

/// Waits so that requests to one host are at least `gap` apart.
async fn pace(host: &str, gap: Duration) {
    static LAST: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    let wait = {
        let mut map = LAST
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        let next = map.get(host).map_or(now, |t| (*t + gap).max(now));
        map.insert(host.to_owned(), next);
        next - now
    };
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
}

fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or(url)
        .to_ascii_lowercase()
}

fn is_pdf(bytes: &[u8]) -> bool {
    bytes.len() <= MAX_PDF_BYTES && bytes.starts_with(b"%PDF")
}

async fn get_pdf(req: reqwest::RequestBuilder) -> Option<Vec<u8>> {
    let resp = req.timeout(Duration::from_secs(90)).send().await.ok()?;
    if !resp.status().is_success()
        || resp
            .content_length()
            .is_some_and(|l| l as usize > MAX_PDF_BYTES)
    {
        return None;
    }
    let bytes = resp.bytes().await.ok()?;
    is_pdf(&bytes).then(|| bytes.to_vec())
}

/// Percent-encodes a DOI for use as one path segment.
fn encode_doi(doi: &str) -> String {
    doi.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Which TDM API serves a DOI, by its registrant prefix.
pub fn publisher_api_for(doi: &str) -> Option<&'static str> {
    let prefix = doi.split('/').next().unwrap_or("");
    match prefix {
        "10.1016" => Some("elsevier"),
        "10.1002" | "10.1111" => Some("wiley"),
        _ => None,
    }
}

/// Tries the publisher's TDM API for a DOI. Returns the PDF and a description.
pub async fn via_publisher_api(env: &Env, doi: &str) -> Option<(Vec<u8>, String)> {
    let api = publisher_api_for(doi)?;
    let key = env
        .secrets
        .get(&crate::sources::key_ref(api))
        .ok()
        .flatten()?;
    match api {
        "elsevier" => {
            let base = env
                .source_base("elsevier")
                .unwrap_or("https://api.elsevier.com");
            let url = format!("{base}/content/article/doi/{doi}");
            pace("api.elsevier.com", Duration::from_secs(1)).await;
            let bytes = get_pdf(
                env.http
                    .get(&url)
                    .header("X-ELS-APIKey", &key)
                    .header("accept", "application/pdf"),
            )
            .await?;
            Some((bytes, format!("Elsevier Article Retrieval API: {url}")))
        }
        "wiley" => {
            let base = env.source_base("wiley").unwrap_or("https://api.wiley.com");
            let url = format!("{base}/onlinelibrary/tdm/v1/articles/{}", encode_doi(doi));
            pace("api.wiley.com", Duration::from_secs(10)).await;
            let bytes = get_pdf(env.http.get(&url).header("Wiley-TDM-Client-Token", &key)).await?;
            Some((bytes, format!("Wiley TDM API: {url}")))
        }
        _ => None,
    }
}

/// A DOI from Crossref for an exact title (Jaro-Winkler >= 0.95 on the
/// normalised title), published within a year of `year`. Crossref REST API:
/// `GET https://api.crossref.org/works?query.bibliographic=&rows=&select=`
/// (api.crossref.org/swagger-ui, checked 2026-09-25).
pub async fn crossref_doi(env: &Env, title: &str, year: Option<i32>) -> Option<String> {
    let want = crate::dedup::normalise_title(title);
    if want.split(' ').count() < 4 {
        return None;
    }
    let base = env
        .source_base("crossref")
        .unwrap_or("https://api.crossref.org");
    let url = format!("{base}/works");
    let v = crate::sources::adapter::get_json_with(
        &crate::sources::adapter::QUICK,
        &env.http,
        &url,
        || {
            env.http.get(&url).query(&[
                ("query.bibliographic", title),
                ("rows", "3"),
                ("select", "DOI,title,issued"),
            ])
        },
    )
    .await
    .ok()?;
    best_crossref_match(&v, &want, year)
}

fn best_crossref_match(v: &serde_json::Value, want: &str, year: Option<i32>) -> Option<String> {
    v["message"]["items"].as_array()?.iter().find_map(|w| {
        let t = crate::dedup::normalise_title(w["title"].as_array()?.first()?.as_str()?);
        let y = w["issued"]["date-parts"][0][0].as_i64().map(|y| y as i32);
        let year_ok = match (year, y) {
            (Some(a), Some(b)) => (a - b).abs() <= 1,
            _ => true,
        };
        (year_ok && strsim::jaro_winkler(&t, want) >= 0.95)
            .then(|| w["DOI"].as_str().map(str::to_ascii_lowercase))
            .flatten()
    })
}

/// The PDF link a publisher page declares in `<meta name="citation_pdf_url">`,
/// resolved against the page URL.
pub fn citation_pdf_url(html: &str, page_url: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find("<meta") {
        let start = from + pos;
        let end = start + lower[start..].find('>')?;
        let tag = &html[start..end];
        let tag_lower = &lower[start..end];
        from = end;
        if !(tag_lower.contains("name=\"citation_pdf_url\"")
            || tag_lower.contains("name='citation_pdf_url'"))
        {
            continue;
        }
        let i = tag_lower.find("content=")? + "content=".len();
        let quote = tag[i..].chars().next()?;
        if quote != '"' && quote != '\'' {
            continue;
        }
        let rest = &tag[i + 1..];
        let value = &rest[..rest.find(quote)?];
        let value = value.replace("&amp;", "&");
        return Some(resolve(&value, page_url));
    }
    None
}

fn resolve(link: &str, page_url: &str) -> String {
    if link.starts_with("http://") || link.starts_with("https://") {
        return link.to_owned();
    }
    let scheme = page_url.split("://").next().unwrap_or("https");
    if let Some(rest) = link.strip_prefix("//") {
        return format!("{scheme}://{rest}");
    }
    let origin = format!("{scheme}://{}", host_of(page_url));
    if link.starts_with('/') {
        format!("{origin}{link}")
    } else {
        let dir = page_url.rsplit_once('/').map_or(page_url, |(d, _)| d);
        format!("{dir}/{link}")
    }
}

/// Follows the DOI to the publisher over the user's network and fetches the PDF it declares.
pub async fn via_institution(env: &Env, doi: &str) -> Option<(Vec<u8>, String)> {
    let doi_url = format!("https://doi.org/{doi}");
    pace("doi.org", Duration::from_secs(1)).await;
    let resp = env
        .http
        .get(&doi_url)
        .header("accept", "text/html,application/pdf;q=0.9")
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .ok()?;
    let page_url = resp.url().to_string();
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.bytes().await.ok()?;
    if is_pdf(&body) {
        return Some((body.to_vec(), format!("institutional access: {page_url}")));
    }
    let html = String::from_utf8_lossy(&body);
    let pdf_url = citation_pdf_url(&html, &page_url)?;
    pace(&host_of(&pdf_url), Duration::from_secs(2)).await;
    let bytes = get_pdf(
        env.http
            .get(&pdf_url)
            .header("accept", "application/pdf")
            .header("referer", &page_url),
    )
    .await?;
    Some((bytes, format!("institutional access: {pdf_url}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_citation_pdf_url_in_either_attribute_order() {
        let a = r#"<head><meta name="citation_title" content="X"><meta name="citation_pdf_url" content="https://www.frontiersin.org/articles/10.3389/x/pdf"></head>"#;
        assert_eq!(
            citation_pdf_url(a, "https://www.frontiersin.org/articles/10.3389/x/full").as_deref(),
            Some("https://www.frontiersin.org/articles/10.3389/x/pdf")
        );
        let b =
            r#"<META content='/content/pdf/10.1057/abc.pdf?a=1&amp;b=2' name='citation_pdf_url'/>"#;
        assert_eq!(
            citation_pdf_url(b, "https://link.springer.com/chapter/10.1057/abc").as_deref(),
            Some("https://link.springer.com/content/pdf/10.1057/abc.pdf?a=1&b=2")
        );
        assert!(citation_pdf_url(
            "<meta name=\"citation_doi\" content=\"10.1/x\">",
            "https://a.org/"
        )
        .is_none());
    }

    #[test]
    fn crossref_match_needs_exact_title_and_year() {
        let v = serde_json::json!({"message": {"items": [
            {"DOI": "10.26686/wgtn.16971196", "title": ["Testing Times: exam formats"], "issued": {"date-parts": [[2021]]}},
            {"DOI": "10.1111/J.1468-2273.2010.00462.X", "title": ["Chinese Students' Perceptions of the Effects of Western University Examination Formats on their Learning"], "issued": {"date-parts": [[2010, 7]]}}
        ]}});
        let want = crate::dedup::normalise_title("Chinese Students' Perceptions of the Effects of Western University Examination Formats on Their Learning");
        assert_eq!(
            best_crossref_match(&v, &want, Some(2010)).as_deref(),
            Some("10.1111/j.1468-2273.2010.00462.x")
        );
        assert_eq!(best_crossref_match(&v, &want, Some(2016)), None);
    }

    #[test]
    fn routes_dois_to_publisher_apis() {
        assert_eq!(
            publisher_api_for("10.1016/j.nedt.2017.05.019"),
            Some("elsevier")
        );
        assert_eq!(publisher_api_for("10.1111/1467-923X.12168"), Some("wiley"));
        assert_eq!(publisher_api_for("10.1177/1028315318773137"), None);
        assert_eq!(
            encode_doi("10.1111/1467-923X.12168"),
            "10.1111%2F1467-923X.12168"
        );
    }

    #[tokio::test]
    async fn pacing_spaces_requests_per_host() {
        let t = Instant::now();
        pace("test.invalid", Duration::from_millis(60)).await;
        pace("test.invalid", Duration::from_millis(60)).await;
        pace("other.invalid", Duration::from_millis(60)).await;
        let e = t.elapsed();
        assert!(
            e >= Duration::from_millis(55) && e < Duration::from_millis(200),
            "{e:?}"
        );
    }
}
