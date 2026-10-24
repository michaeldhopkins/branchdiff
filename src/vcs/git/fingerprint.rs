//! Inputs for `--fingerprint`: ids and working state, read without rendering a diff.

use std::path::Path;

use anyhow::Result;

use super::{
    changed_files::get_changed_files_between, commands::get_merge_base_preferring_origin_for,
    get_all_changed_files,
};
use crate::fingerprint::FingerprintInputs;

pub(super) fn inputs(
    repo_path: &Path,
    base_branch: &str,
    head_ref: Option<&str>,
) -> Result<FingerprintInputs> {
    let target_ref = head_ref.unwrap_or("HEAD");
    // No commits yet: there is no merge base and no HEAD, which is a state like any other.
    let base = get_merge_base_preferring_origin_for(repo_path, base_branch, target_ref)
        .unwrap_or_default();
    let run = |args: &[&str]| {
        vcs_runner::run_git_with_retry(repo_path, args, vcs_runner::is_transient_error)
            .map(|o| o.stdout_lossy().trim().to_string())
    };
    let head = run(&["rev-parse", target_ref]).unwrap_or_default();
    if let Some(hr) = head_ref {
        let changed =
            get_changed_files_between(repo_path, &base, hr)?.into_iter().map(|f| f.path).collect();
        return Ok(FingerprintInputs { base, head, state: String::new(), changed });
    }
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
