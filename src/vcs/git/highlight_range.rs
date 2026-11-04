use std::path::Path;

use anyhow::{Context, Result};
use vcs_runner::run_git;

use super::commands::{get_merge_base_between, rev_id};

/// `Ok(false)` only for git's "not an ancestor" exit (1); any other failure is an error.
pub(super) fn is_ancestor(repo_path: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    match run_git(repo_path, &["merge-base", "--is-ancestor", ancestor, descendant]) {
        Ok(_) => Ok(true),
        Err(vcs_runner::RunError::NonZeroExit { status, .. }) if status.code() == Some(1) => {
            Ok(false)
        }
        Err(err) => Err(err).context("git merge-base --is-ancestor failed for --highlight"),
    }
}

pub(super) fn commit_parents(repo_path: &Path, commit_sha: &str) -> Result<Vec<String>> {
    let out = run_git(repo_path, &["rev-list", "--parents", "-n", "1", commit_sha])
        .context("git rev-list failed for --highlight")?;
    let line = out.stdout_lossy();
    Ok(line
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .skip(1)
        .map(String::from)
        .collect())
}

pub(super) fn clamp_range(
    repo_path: &Path,
    from_sha: &str,
    to_ref: &str,
    merge_base: &str,
    effective_head: &str,
) -> Result<(String, String)> {
    let to_sha = rev_id(repo_path, to_ref).unwrap_or_else(|| to_ref.to_string());
    let to_sha = if is_ancestor(repo_path, &to_sha, effective_head)? {
        to_sha
    } else if let Ok(mb) = get_merge_base_between(repo_path, &to_sha, effective_head)
        && is_ancestor(repo_path, from_sha, &mb)?
    {
        mb
    } else {
        from_sha.to_string()
    };
    if !merge_base.is_empty() {
        if is_ancestor(repo_path, &to_sha, merge_base)? {
            return Ok((to_sha.clone(), to_sha));
        }
        if is_ancestor(repo_path, from_sha, merge_base)?
            && is_ancestor(repo_path, merge_base, &to_sha)?
        {
            return Ok((merge_base.to_string(), to_sha));
        }
    }
    Ok((from_sha.to_string(), to_sha))
}
