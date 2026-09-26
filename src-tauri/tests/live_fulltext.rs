//! Opt-in: institutional access against real publishers (needs network; VPN for paywalled ones).
//! `cargo test --test live_fulltext -- --ignored --nocapture`

use sift_lib::pipeline::{env::Env, fulltext_access};

#[tokio::test]
#[ignore = "hits live publisher sites"]
async fn institutional_access_real_dois() {
    let dir = tempfile::tempdir().unwrap();
    let env = Env::detached(dir.path().join("app.db"));
    for doi in [
        "10.3389/fpsyg.2022.942227",
        "10.1057/9780230299481_10",
        "10.1177/1028315318773137",
    ] {
        let got = fulltext_access::via_institution(&env, doi).await;
        println!(
            "{doi}: {:?}",
            got.as_ref().map(|(b, via)| (b.len(), via.clone()))
        );
    }
}
