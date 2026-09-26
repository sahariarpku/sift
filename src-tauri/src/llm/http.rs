//! Outbound HTTP with retries (§6.4): exponential backoff on 408, 429 and 5xx
//! (including Anthropic's 529), and on connection errors. `Retry-After` in
//! seconds is honoured; a wait longer than `max_delay` is not slept through,
//! the error is returned instead so the job can pause.

use std::time::Duration;

use reqwest::header::HeaderMap;
use reqwest::RequestBuilder;

use super::{error_message, LlmError, LlmResult};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            // About two minutes of backoff in total: free-tier gateways often
            // allow one request per model per second or so.
            max_retries: 6,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(120),
        }
    }
}

impl RetryPolicy {
    /// Tiny delays for tests.
    pub fn fast() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(5),
            max_delay: Duration::from_millis(200),
        }
    }

    /// Backoff before retry number `retry` (1-based), unless the server named a delay.
    pub fn delay(&self, retry: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(d) = retry_after {
            return d;
        }
        let factor = 1u32
            .checked_shl(retry.saturating_sub(1))
            .unwrap_or(u32::MAX);
        self.base_delay.saturating_mul(factor).min(self.max_delay)
    }
}

#[derive(Debug, Clone)]
pub struct HttpReply {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: String,
    /// Requests sent, including retries.
    pub attempts: u32,
}

impl HttpReply {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    }

    /// Maps a non-success reply to an error.
    pub fn into_error(self) -> LlmError {
        let message = error_message(&self.body);
        match self.status {
            401 | 403 => LlmError::Auth {
                status: self.status,
                message,
            },
            404 => LlmError::ModelNotFound {
                status: self.status,
                message,
            },
            s => LlmError::Http {
                status: s,
                message,
                retryable: is_retryable_status(s),
                attempts: self.attempts,
            },
        }
    }
}

pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500..=599)
}

/// `Retry-After` as delta-seconds (integer or decimal). HTTP-date values are
/// ignored and ordinary backoff applies.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let secs: f64 = value.trim().parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs))
}

/// Sends the request built by `build`, retrying as the policy allows. Returns
/// the final reply whatever its status; callers map non-success to errors.
pub async fn send<F>(policy: &RetryPolicy, url: &str, build: F) -> LlmResult<HttpReply>
where
    F: Fn() -> RequestBuilder,
{
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        let can_retry = attempts <= policy.max_retries;
        match build().send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let headers = resp.headers().clone();
                let body = resp.text().await.map_err(|e| LlmError::Unreachable {
                    url: url.to_owned(),
                    message: format!("reading the response failed: {}", e.without_url()),
                })?;
                let reply = HttpReply {
                    status,
                    headers,
                    body,
                    attempts,
                };
                if !is_retryable_status(status) || !can_retry {
                    return Ok(reply);
                }
                let retry_after = reply
                    .header("retry-after")
                    .as_deref()
                    .and_then(parse_retry_after);
                let wait = policy.delay(attempts, retry_after);
                if wait > policy.max_delay {
                    return Ok(reply);
                }
                tokio::time::sleep(jittered(wait)).await;
            }
            Err(e) => {
                let transient = e.is_connect() || e.is_timeout() || e.is_request();
                if !(transient && can_retry) {
                    return Err(LlmError::Unreachable {
                        url: url.to_owned(),
                        message: describe(e),
                    });
                }
                tokio::time::sleep(jittered(policy.delay(attempts, None))).await;
            }
        }
    }
}

/// Adds up to 50% random extra wait so concurrent jobs that hit the same
/// rate limit do not all retry at the same instant.
fn jittered(wait: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let fraction = f64::from(nanos % 1000) / 2000.0;
    wait.saturating_add(wait.mul_f64(fraction))
}

fn describe(e: reqwest::Error) -> String {
    if e.is_connect() {
        "connection refused or host not found".into()
    } else if e.is_timeout() {
        "timed out".into()
    } else {
        // Drop the URL: some APIs (PubMed) carry the key in the query string.
        e.without_url().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_http::{MockServer, Reply};

    #[test]
    fn backoff_doubles_and_caps() {
        let p = RetryPolicy {
            max_retries: 5,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(5),
        };
        assert_eq!(p.delay(1, None), Duration::from_secs(1));
        assert_eq!(p.delay(2, None), Duration::from_secs(2));
        assert_eq!(p.delay(3, None), Duration::from_secs(4));
        assert_eq!(p.delay(4, None), Duration::from_secs(5));
        assert_eq!(p.delay(40, None), Duration::from_secs(5));
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(9))),
            Duration::from_secs(9)
        );
    }

    #[test]
    fn jitter_stays_within_half_again() {
        let w = Duration::from_secs(2);
        for _ in 0..50 {
            let j = jittered(w);
            assert!(j >= w && j <= w.mul_f64(1.5));
        }
    }

    #[test]
    fn retry_after_parsing() {
        assert_eq!(parse_retry_after("3"), Some(Duration::from_secs(3)));
        assert_eq!(parse_retry_after("0.05"), Some(Duration::from_millis(50)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("-1"), None);
    }

    #[tokio::test]
    async fn retries_429_then_succeeds_honouring_retry_after() {
        let server = MockServer::start(vec![
            Reply::json(429, r#"{"error":{"message":"slow down"}}"#).header("retry-after", "0.01"),
            Reply::json(503, "{}"),
            Reply::json(200, r#"{"ok":true}"#),
        ])
        .await;
        let client = reqwest::Client::new();
        let url = server.url("/x");
        let reply = send(&RetryPolicy::fast(), &url, || client.get(&url))
            .await
            .unwrap();
        assert_eq!((reply.status, reply.attempts), (200, 3));
        assert_eq!(server.requests().len(), 3);
    }

    #[tokio::test]
    async fn gives_up_after_max_retries() {
        let server = MockServer::start(vec![
            Reply::json(
                529,
                r#"{"error":{"message":"Overloaded"}}"#
            );
            10
        ])
        .await;
        let client = reqwest::Client::new();
        let url = server.url("/x");
        let reply = send(&RetryPolicy::fast(), &url, || client.get(&url))
            .await
            .unwrap();
        assert_eq!(reply.attempts, RetryPolicy::fast().max_retries + 1);
        let err = reply.into_error();
        assert!(err.is_retryable());
        assert!(err.to_string().contains("Overloaded"));
    }

    #[tokio::test]
    async fn long_retry_after_is_not_slept_through() {
        let server =
            MockServer::start(vec![Reply::json(429, "{}").header("retry-after", "3600")]).await;
        let client = reqwest::Client::new();
        let url = server.url("/x");
        let reply = send(&RetryPolicy::fast(), &url, || client.get(&url))
            .await
            .unwrap();
        assert_eq!((reply.status, reply.attempts), (429, 1));
    }

    #[tokio::test]
    async fn does_not_retry_client_errors() {
        let server = MockServer::start(vec![Reply::json(
            401,
            r#"{"error":{"message":"invalid x-api-key"}}"#,
        )])
        .await;
        let client = reqwest::Client::new();
        let url = server.url("/x");
        let reply = send(&RetryPolicy::fast(), &url, || client.get(&url))
            .await
            .unwrap();
        assert_eq!(reply.attempts, 1);
        assert!(matches!(
            reply.into_error(),
            LlmError::Auth { status: 401, .. }
        ));
    }

    #[tokio::test]
    async fn unreachable_host_is_reported() {
        let client = reqwest::Client::new();
        // Port 9 (discard) on localhost is closed on a normal machine.
        let url = "http://127.0.0.1:9/v1/models".to_string();
        let err = send(&RetryPolicy::fast(), &url, || client.get(&url))
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Unreachable { .. }));
    }
}
