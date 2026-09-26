//! Every command registered in `lib.rs` must also be declared in `build.rs`
//! and allowed in `capabilities/default.json`, or the frontend gets
//! "<command> not allowed" at runtime.
#![allow(clippy::expect_used)] // test-only crate

use std::collections::BTreeSet;

fn read(path: &str) -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/").to_owned() + path)
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn registered() -> BTreeSet<String> {
    let lib = read("src/lib.rs");
    let start = lib.find("generate_handler![").expect("handler list");
    let end = start + lib[start..].find(']').expect("end of handler list");
    lib[start..end]
        .split(',')
        .filter_map(|p| p.trim().rsplit("::").next())
        .map(|c| c.trim().to_owned())
        .filter(|c| !c.is_empty() && !c.contains('['))
        .collect()
}

#[test]
fn every_command_is_declared_and_allowed() {
    let build = read("build.rs");
    let caps = read("capabilities/default.json");
    let mut missing = Vec::new();
    for cmd in registered() {
        if !build.contains(&format!("\"{cmd}\"")) {
            missing.push(format!("{cmd}: not in build.rs"));
        }
        let perm = format!("\"allow-{}\"", cmd.replace('_', "-"));
        if !caps.contains(&perm) {
            missing.push(format!("{cmd}: {perm} not in capabilities/default.json"));
        }
    }
    assert!(registered().len() > 50, "handler list not parsed");
    assert!(missing.is_empty(), "{}", missing.join("\n"));
}
