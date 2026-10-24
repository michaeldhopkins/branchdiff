use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

use crate::vcs::UpstreamDivergence;
use vcs_runner::{Cmd, run_git, run_git_with_timeout};

pub(super) use super::content::{batch_file_contents, get_file_at_ref, get_working_tree_file};
pub use super::content::{
    get_binary_files, get_binary_files_between, get_file_bytes_at_ref, get_working_tree_bytes,
    is_binary_file, is_binary_file_between,
};

/// Git version required for merge-tree --write-tree (conflict detection)
const MERGE_TREE_MIN_VERSION: (u32, u32) = (2, 38);

/// Parsed git version
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl GitVersion {
    /// Check if this version is at least the given major.minor
    pub fn at_least(&self, major: u32, minor: u32) -> bool {
        (self.major, self.minor) >= (major, minor)
    }
}

impl std::fmt::Display for GitVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Detect the installed git version
pub fn get_git_version() -> Result<GitVersion> {
    let output = Cmd::new("git").args(["--version"]).run()?;
    parse_git_version(&output.stdout_lossy())
}

/// Check if an external process holds the git index lock.
///
/// Returns true if `.git/index.lock` exists, indicating another git process
/// (like `git rebase`, `git commit`, etc.) is currently running.
/// When locked, branchdiff should defer refresh to avoid lock collisions.
pub fn is_index_locked(repo_path: &Path) -> bool {
    repo_path.join(".git/index.lock").exists()
}

/// Parse git version from "git version X.Y.Z" string
pub(crate) fn parse_git_version(s: &str) -> Result<GitVersion> {
    // Format: "git version 2.34.1" or "git version 2.50.1 (Apple Git-155)"
    let version_part = s
        .trim()
        .strip_prefix("git version ")
        .ok_or_else(|| anyhow!("Unexpected git version format: {}", s))?;

    // Take the first space-separated part (handles Apple Git suffix)
    let version_num = version_part.split_whitespace().next().unwrap_or(version_part);

    let parts: Vec<&str> = version_num.split('.').collect();
    if parts.len() < 2 {
        return Err(anyhow!("Cannot parse git version: {}", s));
    }

    let major = parts[0].parse().context("Invalid major version")?;
    let minor = parts[1].parse().context("Invalid minor version")?;
    let patch = parts.get(2).and_then(|p| p.parse().ok()).unwrap_or(0);

    Ok(GitVersion { major, minor, patch })
}

/// Get the root directory of the git repository
pub fn get_repo_root(path: &Path) -> Result<PathBuf> {
    let output = run_git(path, &["rev-parse", "--show-toplevel"])?;
    let root = output.stdout_lossy().trim().to_string();
    Ok(PathBuf::from(root))
}

/// Check whether a git ref exists in the repository.
fn ref_exists(repo_path: &Path, git_ref: &str) -> bool {
    run_git(repo_path, &["rev-parse", "--verify", git_ref]).is_ok()
}

/// The commit a ref points at, or `None` if it doesn't resolve.
pub(super) fn rev_id(repo_path: &Path, git_ref: &str) -> Option<String> {
    let output = run_git(repo_path, &["rev-parse", "--verify", git_ref]).ok()?;
    let id = output.stdout_lossy().trim().to_string();
    (!id.is_empty()).then_some(id)
}

/// The remote's default branch, as git itself records it.
///
/// `refs/remotes/origin/HEAD` is a symbolic ref git writes at clone time (and
/// `git remote set-head origin -a` refreshes) naming origin's default branch.
/// It is git's own answer to "what is trunk" — the closest equivalent to jj's
/// `trunk()` — so it beats guessing at names.
fn origin_head_branch(repo_path: &Path) -> Option<String> {
    let output =
        run_git(repo_path, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).ok()?;
    let full = output.stdout_lossy().trim().to_string();
    full.strip_prefix("origin/").map(str::to_string).filter(|s| !s.is_empty())
}

/// Detect the base branch to compare against.
///
/// Follows git's own rule first: `refs/remotes/origin/HEAD`. Probing only
/// `main`/`master` meant any repo whose default branch is something else (a
/// `release`/`develop`/`trunk` default) fell through to a hardcoded "main" that
/// did not exist, and branchdiff silently reported "no changes" for real work.
///
/// `main`/`master` remain the fallback for repos with no `origin/HEAD` — never
/// cloned, or the remote head was never set. Origin remote-tracking refs are
/// preferred over local branches so that a local branch tracking a non-origin
/// remote (e.g. heroku) doesn't win over origin.
pub fn detect_base_branch(repo_path: &Path) -> Result<String> {
    // git's own notion of origin's default branch.
    if let Some(branch) = origin_head_branch(repo_path) {
        return Ok(branch);
    }

    // Prefer origin remote-tracking refs
    for branch in &["main", "master"] {
        if ref_exists(repo_path, &format!("origin/{}", branch)) {
            return Ok(branch.to_string());
        }
    }

    // Fall back to local branches (repos without an origin remote)
    for branch in &["main", "master"] {
        if ref_exists(repo_path, branch) {
            return Ok(branch.to_string());
        }
    }

    Err(anyhow!(
        "Could not determine a base branch: origin has no default branch (origin/HEAD) \
         and there is no 'main' or 'master'. Pass --base to name one explicitly."
    ))
}

/// Get the merge-base between HEAD and the base branch, preferring origin
pub fn get_merge_base_preferring_origin(repo_path: &Path, base_branch: &str) -> Result<String> {
    get_merge_base_preferring_origin_for(repo_path, base_branch, "HEAD")
}

/// Get the merge-base between `head_ref` and the base branch, preferring origin
pub fn get_merge_base_preferring_origin_for(
    repo_path: &Path,
    base_branch: &str,
    head_ref: &str,
) -> Result<String> {
    let remote_ref = format!("origin/{}", base_branch);
    get_merge_base_between(repo_path, &remote_ref, head_ref)
        .or_else(|_| get_merge_base_between(repo_path, base_branch, head_ref))
}

/// Get the merge-base between the base branch and HEAD
#[cfg(test)]
pub(super) fn get_merge_base(repo_path: &Path, base_branch: &str) -> Result<String> {
    get_merge_base_between(repo_path, base_branch, "HEAD")
}

/// Get the merge-base between `base_branch` and `head_ref`
pub(super) fn get_merge_base_between(
    repo_path: &Path,
    base_branch: &str,
    head_ref: &str,
) -> Result<String> {
    let output = run_git(repo_path, &["merge-base", base_branch, head_ref])?;
    Ok(output.stdout_lossy().trim().to_string())
}

/// Compute upstream divergence: how many commits and which files have changed
/// on origin/{base_branch} since the merge-base.
pub fn compute_upstream_divergence(
    repo_path: &Path,
    merge_base: &str,
    base_branch: &str,
) -> Option<UpstreamDivergence> {
    if merge_base.is_empty() {
        return None;
    }

    let remote_ref = format!("origin/{}", base_branch);
    if !ref_exists(repo_path, &remote_ref) {
        return None;
    }

    let behind_count = rev_list_count(repo_path, merge_base, &remote_ref).unwrap_or(0);
    if behind_count == 0 {
        return None;
    }

    let upstream_files =
        upstream_changed_files(repo_path, merge_base, &remote_ref).unwrap_or_default();

    Some(UpstreamDivergence { behind_count, upstream_files })
}

/// Count commits reachable from `to` but not from `from`.
fn rev_list_count(repo_path: &Path, from: &str, to: &str) -> Result<usize> {
    let range = format!("{}..{}", from, to);
    let output = run_git(repo_path, &["rev-list", "--count", &range])?;
    output.stdout_lossy().trim().parse::<usize>().context("Failed to parse rev-list count")
}

/// Get files changed between two refs.
fn upstream_changed_files(repo_path: &Path, from: &str, to: &str) -> Result<HashSet<String>> {
    let range = format!("{}..{}", from, to);
    let output = run_git(repo_path, &["diff", "--name-only", &range])?;
    Ok(output.stdout_lossy().lines().filter(|l| !l.is_empty()).map(|l| l.to_string()).collect())
}

pub fn fetch_base_branch(repo_path: &Path, base_branch: &str) -> Result<()> {
    let current = get_current_branch(repo_path).ok().flatten();
    let on_base_branch = current.as_deref() == Some(base_branch);

    let refspec = format!("{}:{}", base_branch, base_branch);
    let fetch_arg = if on_base_branch { base_branch } else { &refspec };

    run_git_with_timeout(
        repo_path,
        &["-c", "gc.auto=0", "fetch", "--no-tags", "origin", fetch_arg],
        std::time::Duration::from_secs(30),
    )?;
    Ok(())
}

/// Check for merge conflicts using git merge-tree.
/// Requires Git 2.38+ for --write-tree flag; returns Ok(false) on older versions.
pub fn has_merge_conflicts(
    repo_path: &Path,
    base_branch: &str,
    git_version: &GitVersion,
) -> Result<bool> {
    // merge-tree --write-tree requires Git 2.38+
    if !git_version.at_least(MERGE_TREE_MIN_VERSION.0, MERGE_TREE_MIN_VERSION.1) {
        return Ok(false);
    }

    let remote_ref = format!("origin/{}", base_branch);

    if !ref_exists(repo_path, &remote_ref) {
        return Ok(false);
    }

    // git merge-tree exits non-zero iff conflicts are detected. Distinguish
    // "conflict reported" (non-zero exit) from "command couldn't run" (spawn).
    match run_git(repo_path, &["merge-tree", "--write-tree", &remote_ref, "HEAD"]) {
        Ok(_) => Ok(false),
        Err(e) if e.is_non_zero_exit() => Ok(true),
        Err(e) => Err(e.into()),
    }
}

/// Get the current branch name
pub fn get_current_branch(repo_path: &Path) -> Result<Option<String>> {
    let output = match run_git(repo_path, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(o) => o,
        Err(_) => return Ok(None),
    };

    let branch = output.stdout_lossy().trim().to_string();
    if branch == "HEAD" { Ok(None) } else { Ok(Some(branch)) }
}
