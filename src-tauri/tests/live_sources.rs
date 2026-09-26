#![allow(clippy::unwrap_used)] // test-only crate
//! Opt-in live check of the source key tests. Never runs by default (§13).
//! Keyless sources always run; keyed ones run when SIFT_KEY_<SOURCE> is set,
//! e.g. SIFT_KEY_OPENALEX. Unpaywall runs when SIFT_UNPAYWALL_EMAIL is set.
//!
//!   cargo test --test live_sources -- --ignored --nocapture

use sift_lib::sources::keytest::{test_source, Credentials};

#[tokio::test]
#[ignore]
async fn live_source_checks() {
    let client = sift_lib::llm::http_client().unwrap();
    for id in [
        "eric",
        "openalex",
        "wos",
        "scopus",
        "pubmed",
        "semantic_scholar",
        "core",
        "unpaywall",
    ] {
        let env = format!("SIFT_KEY_{}", id.to_uppercase());
        let creds = Credentials {
            key: std::env::var(&env).ok(),
            inst_token: std::env::var("SIFT_KEY_SCOPUS_INSTTOKEN").ok(),
            email: std::env::var("SIFT_UNPAYWALL_EMAIL").ok(),
        };
        let keyless = matches!(id, "eric" | "pubmed" | "semantic_scholar");
        if !keyless && creds.key.is_none() && !(id == "unpaywall" && creds.email.is_some()) {
            println!("{id:17} skipped (no credential in {env})");
            continue;
        }
        let r = test_source(&client, id, &creds, None).await;
        println!("{id:17} ok={} status={:?} {}", r.ok, r.status, r.message);
    }
}
