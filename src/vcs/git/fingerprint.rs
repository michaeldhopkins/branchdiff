//! Inputs for `--fingerprint`: ids and working state, read without rendering a diff.

use std::path::Path;

use anyhow::Result;

use super::{get_all_changed_files, get_merge_base_preferring_origin};
use crate::fingerprint::FingerprintInputs;

pub(super) fn inputs(repo_path: &Path, base_branch: &str) -> Result<FingerprintInputs> {
    // No commits yet: there is no merge base and no HEAD, which is a state like any other.
    let base = get_merge_base_preferring_origin(repo_path, base_branch).unwrap_or_default();
    let run = |args: &[&str]| {
        vcs_runner::run_git_with_retry(repo_path, args, vcs_runner::is_transient_error)
            .map(|o| o.stdout_lossy().trim().to_string())
    };
    let head = run(&["rev-parse", "HEAD"]).unwrap_or_default();
    // The index decides which layer a change is drawn in, so staging alone changes the diff.
    let state = vcs_runner::run_git_with_retry(
        repo_path,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        vcs_runner::is_transient_error,
    )?
    .stdout_lossy()
    .into_owned();
    let changed = get_all_changed_files(repo_path, &base)?.into_iter().map(|f| f.path).collect();
    Ok(FingerprintInputs { base, head, state, changed })
}
