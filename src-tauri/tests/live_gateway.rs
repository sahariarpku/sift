#![allow(clippy::unwrap_used)] // test-only crate
//! Opt-in check against a real OpenAI-compatible server. Never runs by default
//! (§13: no test calls a live API). Run with:
//!
//! SIFT_LIVE_BASE_URL=http://127.0.0.1:31415/v1 SIFT_LIVE_KEY=... \
//!   cargo test --test live_gateway -- --ignored --nocapture

use std::time::Duration;

use serde_json::json;
use sift_lib::llm::{
    build_provider, ChatMessage, CompletionRequest, JsonSchema, Protocol, ProviderConn,
    ProviderOptions, RetryPolicy,
};

#[tokio::test]
#[ignore]
async fn live_openai_compatible_server() {
    let base = std::env::var("SIFT_LIVE_BASE_URL").expect("set SIFT_LIVE_BASE_URL");
    let model = std::env::var("SIFT_LIVE_MODEL").unwrap_or_else(|_| "auto".into());
    let conn = ProviderConn {
        protocol: Protocol::OpenaiCompat,
        base_url: base,
        api_key: std::env::var("SIFT_LIVE_KEY").ok(),
        options: ProviderOptions::default(),
        test_model: Some(model.clone()),
        retry: RetryPolicy {
            max_retries: 1,
            ..RetryPolicy::default()
        },
        timeout: Duration::from_secs(120),
    };
    let p = build_provider(conn, sift_lib::llm::http_client().unwrap());

    let models = p.list_models().await.unwrap();
    println!("list_models: {} models", models.len());
    assert!(!models.is_empty());

    match p.test().await {
        Ok(t) => println!(
            "test(): ok={} model={:?} reported={:?} match={:?} ({} ms) {}",
            t.ok, t.model, t.reported_model, t.model_match, t.latency_ms, t.message
        ),
        Err(e) => println!("test(): error: {e}"),
    }

    let mut req = CompletionRequest::new(model, vec![ChatMessage::user(
        "Record: 'Peer mentoring and sense of belonging among first-generation university students: a randomised trial.' \
         Criterion IN-POP: participants are first-generation students. Decide include, exclude or uncertain.",
    )]);
    req.max_tokens = 400;
    req.json_schema = Some(JsonSchema {
        name: "screen".into(),
        schema: json!({
            "type": "object",
            "properties": {
                "decision": {"type": "string", "enum": ["include", "exclude", "uncertain"]},
                "confidence": {"type": "number"}
            },
            "required": ["decision", "confidence"],
            "additionalProperties": false
        }),
    });
    match p.complete(req).await {
        Ok(r) => println!(
            "complete(json): reported={:?} routed_via={:?} match={:?} structured={} temperature={:?} usage={:?} attempts={}\n  json={:?}\n  json_error={:?}",
            r.reported_model, r.routed_via, r.model_match(), r.structured_output, r.temperature_applied, r.usage, r.attempts, r.json, r.json_error
        ),
        Err(e) => println!("complete(json): error: {e}"),
    }
}
