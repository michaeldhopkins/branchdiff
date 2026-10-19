//! The allocation bench is a required CI step. The job that runs it has to be
//! bounded, and the compiler has to be the channel `rust-toolchain.toml` pins.
//! `@stable` floats, so a later stable can move std's allocations under the pins.

use std::fs;
use std::path::Path;

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn channel(toolchain_toml: &str) -> String {
    for raw in toolchain_toml.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        let Some(rest) = line.strip_prefix("channel") else {
            continue;
        };
        let value = rest.trim().trim_start_matches('=').trim().trim_matches('"');
        assert!(!value.is_empty(), "rust-toolchain.toml has an empty channel");
        return value.to_string();
    }
    panic!("rust-toolchain.toml has no channel");
}

#[test]
fn alloc_ci_job_is_bounded_and_pins_the_toolchain() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ci = read(&root.join(".github/workflows/ci.yml"));
    let pinned = channel(&read(&root.join("rust-toolchain.toml")));

    assert!(ci.lines().any(|line| line.trim() == "permissions:"), "ci.yml must set permissions");
    assert!(
        ci.lines().any(|line| line.trim() == "contents: read"),
        "ci.yml must set permissions contents: read"
    );
    assert!(ci.lines().any(|line| line.trim() == "concurrency:"), "ci.yml must set concurrency");
    assert!(ci.contains("cargo bench --locked --bench alloc"), "ci.yml has no alloc bench step");
    assert!(
        ci.lines().any(|line| line.trim_start().starts_with("timeout-minutes:")),
        "the job that runs the alloc bench must set timeout-minutes"
    );
    assert!(
        !ci.contains("dtolnay/rust-toolchain@stable"),
        "ci.yml selects the floating stable toolchain"
    );
    let pin_line = format!("toolchain: {pinned}");
    assert!(
        ci.contains(&pin_line),
        "ci.yml must install {pin_line}, the channel in rust-toolchain.toml"
    );
}
