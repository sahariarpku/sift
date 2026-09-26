//! Opt-in: runs the real Laya-MLX helper with the local Python install.
//! `cargo test --test live_laya -- --ignored --nocapture`

use sift_lib::local::laya::{self, Laya};

#[tokio::test]
#[ignore = "needs laya-mlx and downloaded weights on an Apple Silicon Mac"]
async fn real_laya_scores_a_record() {
    let status = laya::detect(None, laya::DEFAULT_MODEL).await;
    println!("{status:?}");
    assert!(status.available, "{}", status.message);
    let dir = tempfile::tempdir().unwrap();
    let python = status.python.unwrap();
    let t = std::time::Instant::now();
    let mut helper = Laya::start(
        std::path::Path::new(&python),
        dir.path(),
        laya::DEFAULT_MODEL,
    )
    .await
    .unwrap();
    println!("loaded in {:?}, laya-mlx {}", t.elapsed(), helper.version);
    let p = serde_json::json!({
        "IN-POP": {"type": "noul", "instructions": "Does this record meet the following eligibility criterion? Participants are Chinese international students."},
        "IN-CONTEXT": {"type": "noul", "instructions": "Does this record meet the following eligibility criterion? The setting is a university in the United Kingdom."}
    });
    let t = std::time::Instant::now();
    let uk = helper
        .ask(
            &laya::state(
                "Chinese international students' sense of belonging at UK universities",
                "Interviews with 20 mainland Chinese postgraduates at two English universities.",
            ),
            &p,
        )
        .await
        .unwrap();
    let us = helper
        .ask(
            &laya::state(
                "Saudi women studying in the United States",
                "A survey of Saudi female students at US colleges.",
            ),
            &p,
        )
        .await
        .unwrap();
    println!("two records in {:?}", t.elapsed());
    let (s_uk, parts) = laya::score(&uk).unwrap();
    let (s_us, _) = laya::score(&us).unwrap();
    println!("UK {s_uk:.3} {parts:?} | US {s_us:.3}");
    assert!(s_uk > s_us);
}
