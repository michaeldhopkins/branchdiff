use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::diff::{compute_file_diff_v2, DiffLine, FileDiff, LineSource};
use crate::git;

pub struct RefreshResult {
    pub files: Vec<FileDiff>,
    pub lines: Vec<DiffLine>,
    pub merge_base: String,
    pub current_branch: Option<String>,
}

pub fn compute_refresh(
    repo_path: &Path,
    base_branch: &str,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<RefreshResult> {
    let merge_base = git::get_merge_base(repo_path, base_branch).unwrap_or_default();

    if cancel_flag.load(Ordering::Relaxed) {
        return Err(anyhow::anyhow!("refresh cancelled"));
    }

    let changed_files = git::get_all_changed_files(repo_path, &merge_base)
        .context("Failed to get changed files")?;

    let mut files = Vec::new();
    let mut lines = Vec::new();

    for file in changed_files {
        if cancel_flag.load(Ordering::Relaxed) {
            return Err(anyhow::anyhow!("refresh cancelled"));
        }

        if git::is_binary_file(repo_path, &file.path) {
            lines.push(DiffLine::file_header(&file.path));
            lines.push(DiffLine::new(
                LineSource::Base,
                "[binary file]".to_string(),
                ' ',
                None,
            ));
            continue;
        }

        let base_content = if merge_base.is_empty() {
            None
        } else {
            git::get_file_at_ref(repo_path, &file.path, &merge_base)
                .ok()
                .flatten()
        };

        let head_content = git::get_file_at_ref(repo_path, &file.path, "HEAD")
            .ok()
            .flatten();

        let index_content = git::get_file_at_ref(repo_path, &file.path, "")
            .ok()
            .flatten();

        let working_content = git::get_working_tree_file(repo_path, &file.path)
            .ok()
            .flatten();

        let file_diff = compute_file_diff_v2(
            &file.path,
            base_content.as_deref(),
            head_content.as_deref(),
            index_content.as_deref(),
            working_content.as_deref(),
        );

        lines.extend(file_diff.lines.iter().cloned());
        lines.push(DiffLine::new(LineSource::Base, String::new(), ' ', None));

        files.push(file_diff);
    }

    let current_branch = git::get_current_branch(repo_path).unwrap_or(None);

    Ok(RefreshResult {
        files,
        lines,
        merge_base,
        current_branch,
    })
}
