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
    assert!(
        ci.contains("cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}"),
        "ci.yml must cancel in-progress runs only off main"
    );
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

#[test]
fn ci_installs_pinned_jj_and_runs_perf_rules() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ci = read(&root.join(".github/workflows/ci.yml"));
    let mutants = read(&root.join(".github/workflows/mutants.yml"));

    for env_line in
        ["JJ_VERSION: v0.45.1", "JJ_USER: branchdiff-ci", "JJ_EMAIL: ci@example.invalid"]
    {
        assert!(mutants.contains(env_line), "mutants.yml missing {env_line}");
        assert!(ci.contains(env_line), "ci.yml must set {env_line} matching mutants.yml");
    }
    assert!(ci.contains("fetch-depth: 0"), "ci.yml must checkout full history for origin/main");
    assert!(
        ci.contains("jj-${JJ_VERSION}-x86_64-unknown-linux-musl.tar.gz"),
        "ci.yml must install pinned jj binary from release archive"
    );
    assert!(
        ci.contains("cargo test --locked --test perf_rules"),
        "ci.yml must run cargo test --locked --test perf_rules"
    );
    assert!(ci.contains("--test ci_alloc_job"), "ci.yml must run --test ci_alloc_job");
    assert!(
        ci.contains("PERF_BASELINE_REV:")
            && ci.contains("github.event.pull_request.base.sha")
            && ci.contains("github.event.before")
            && ci.contains("git merge-base HEAD origin/main"),
        "ci.yml must resolve PERF_BASELINE_REV from PR base sha, push before sha, or git merge-base HEAD origin/main"
    );
    assert!(
        mutants.lines().any(|line| line.trim() == "concurrency:")
            && mutants.contains("group: mutants-${{ github.ref }}")
            && mutants.contains("cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}"),
        "mutants.yml must define a concurrency group that cancels in-progress runs off main"
    );
    assert!(
        !mutants.contains("ci.yml does not install jj")
            && !mutants.contains("it never installs jj"),
        "mutants.yml must not claim ci.yml never installs jj"
    );
}

#[test]
fn ci_workflow_step_resolves_perf_baseline_rev_for_new_branch_and_force_push() {
    use std::process::Command;
    use tempfile::TempDir;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ci = read(&root.join(".github/workflows/ci.yml"));
    let (_, after_step) = ci
        .split_once("- name: cargo test (performance budgets)")
        .expect("perf budgets step in ci.yml");
    let (_, after_run) = after_step.split_once("run: |\n").expect("multiline run in perf step");
    let mut script = String::new();
    for line in after_run.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("cargo test") {
            break;
        }
        script.push_str(trimmed);
        script.push('\n');
    }
    script.push_str("printf '%s' \"$PERF_BASELINE_REV\"\n");

    let tmp = TempDir::new().expect("tempdir");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(tmp.path())
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("utf8").trim().to_string()
    };

    git(&["init", "-q", "-b", "main"]);
    fs::write(tmp.path().join("f.txt"), "base\n").expect("write");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "main base"]);
    let main_sha = git(&["rev-parse", "HEAD"]);
    git(&["update-ref", "refs/remotes/origin/main", &main_sha]);

    git(&["checkout", "-q", "-b", "feature"]);
    fs::write(tmp.path().join("f.txt"), "feature 1\n").expect("write");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "feature 1"]);
    let feature_1_sha = git(&["rev-parse", "HEAD"]);
    fs::write(tmp.path().join("f.txt"), "feature 2\n").expect("write");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "feature 2"]);

    let run_step = |event: &str, gref: &str, before: &str| -> String {
        let out = Command::new("sh")
            .arg("-c")
            .arg(&script)
            .current_dir(tmp.path())
            .env("GITHUB_EVENT_NAME", event)
            .env("GITHUB_REF", gref)
            .env("PERF_BASELINE_REV", before)
            .output()
            .expect("sh");
        assert!(
            out.status.success(),
            "step script failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("utf8")
    };

    let zero = "0000000000000000000000000000000000000000";
    let unreachable = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

    assert_eq!(run_step("push", "refs/heads/feature", zero), main_sha);
    assert_eq!(run_step("push", "refs/heads/feature", unreachable), main_sha);
    assert_eq!(run_step("push", "refs/heads/feature", &feature_1_sha), main_sha);
    assert_eq!(run_step("pull_request", "refs/pull/1/merge", &feature_1_sha), feature_1_sha);
    assert_eq!(run_step("pull_request", "refs/pull/1/merge", unreachable), main_sha);
    assert_eq!(run_step("push", "refs/heads/main", zero), zero);
    assert_eq!(run_step("push", "refs/heads/main", unreachable), unreachable);
}
