//! Small revset queries shared by the jj backend's base detection.

use std::path::Path;

use vcs_runner::run_jj;

use super::jj::TRUNK_BOOKMARK_CANDIDATES;

/// Whether a revset resolves to at least one commit.
pub(super) fn revset_resolves(repo_path: &Path, revset: &str) -> bool {
    run_jj(repo_path, &[
        "--ignore-working-copy", "log", "-r", revset, "--no-graph", "--limit", "1", "-T", r#""x""#,
    ])
    .map(|o| !o.stdout_lossy().trim().is_empty())
    .unwrap_or(false)
}

/// How many commits a revset resolves to, counting no further than 2.
///
/// A caller that needs "exactly one" cannot use [`revset_resolves`]: `--limit 1`
/// answers ">= 1", and `jj diff --from` takes exactly one revision. Limiting to
/// 2 is enough to tell none / one / many apart without walking a large set.
pub(super) fn revset_commit_count(repo_path: &Path, revset: &str) -> usize {
    run_jj(repo_path, &[
        "--ignore-working-copy", "log", "-r", revset, "--no-graph", "--limit", "2", "-T", r#""x\n""#,
    ])
    .map(|o| o.stdout_lossy().lines().filter(|l| !l.trim().is_empty()).count())
    .unwrap_or(0)
}

/// Committer timestamp (epoch seconds) of the commit a revset resolves to, or
/// `None` if it resolves to nothing.
pub(super) fn revset_committer_epoch(repo_path: &Path, revset: &str) -> Option<i64> {
    let output = run_jj(repo_path, &[
        "--ignore-working-copy", "log", "-r", revset, "--no-graph", "--limit", "1",
        "-T", r#"committer.timestamp().utc().format("%s")"#,
    ])
    .ok()?;
    output.stdout_lossy().trim().parse::<i64>().ok()
}

/// The local `main`/`master`/`trunk` bookmark to use as base when `trunk()`
/// resolves to nothing (a repo with no remote).
///
/// Without a remote the only other fallback is `@-`, which in a workspace
/// parked at a feature bookmark is the feature's own commit: the diff then
/// shows nothing but uncommitted changes. A conflicted bookmark (more than one
/// commit) is skipped.
pub(super) fn local_trunk_bookmark(repo_path: &Path) -> Option<String> {
    TRUNK_BOOKMARK_CANDIDATES
        .iter()
        .find(|name| revset_commit_count(repo_path, &format!(r#"bookmarks(exact:"{name}")"#)) == 1)
        .map(|name| (*name).to_string())
}
