use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};

use crate::diff::{compute_four_way_diff, DiffInput, FileDiff};
use crate::file_links::compute_file_links;
use crate::image_diff::is_image_file;
use crate::limits::DiffMetrics;
use crate::vcs::shared::{assemble_results, process_files_parallel, FileProcessResult};
use crate::vcs::RefreshResult;

use super::changed_files::get_all_changed_files;
use super::commands::{
    batch_file_contents, compute_upstream_divergence, get_binary_files, get_current_branch,
    get_file_at_ref, get_merge_base_preferring_origin, get_working_tree_file, is_binary_file,
};

struct FileContents {
    base: Option<String>,
    head: Option<String>,
    index: Option<String>,
    working: Option<String>,
}

impl FileContents {
    fn fetch(repo_path: &Path, file_path: &str, old_path: Option<&str>, merge_base: &str) -> Self {
        let base_path = old_path.unwrap_or(file_path);

        let base = if merge_base.is_empty() {
            None
        } else {
            get_file_at_ref(repo_path, base_path, merge_base)
                .ok()
                .flatten()
        };

        let head = get_file_at_ref(repo_path, file_path, "HEAD")
            .ok()
            .flatten()
            .or_else(|| {
                old_path.and_then(|p| get_file_at_ref(repo_path, p, "HEAD").ok().flatten())
            });

        let index = get_file_at_ref(repo_path, file_path, "")
            .ok()
            .flatten()
            .or_else(|| {
                old_path.and_then(|p| get_file_at_ref(repo_path, p, "").ok().flatten())
            });

        Self {
            base,
            head,
            index,
            working: get_working_tree_file(repo_path, file_path)
                .ok()
                .flatten(),
        }
    }

    fn from_batch(
        file_path: &str,
        old_path: Option<&str>,
        base_contents: &HashMap<String, String>,
        head_contents: &HashMap<String, String>,
        index_contents: &HashMap<String, String>,
        repo_path: &Path,
    ) -> Self {
        let base_path = old_path.unwrap_or(file_path);
        Self {
            base: base_contents.get(base_path).cloned(),
            head: head_contents
                .get(file_path)
                .cloned()
                .or_else(|| old_path.and_then(|p| head_contents.get(p).cloned())),
            index: index_contents
                .get(file_path)
                .cloned()
                .or_else(|| old_path.and_then(|p| index_contents.get(p).cloned())),
            working: get_working_tree_file(repo_path, file_path).ok().flatten(),
        }
    }

    fn all_equal(&self) -> bool {
        self.base == self.working && self.base == self.head && self.base == self.index
    }
}

fn process_single_file_batched(
    repo_path: &Path,
    file_path: &str,
    old_path: Option<&str>,
    binary_files: &HashSet<String>,
    base_contents: &HashMap<String, String>,
    head_contents: &HashMap<String, String>,
    index_contents: &HashMap<String, String>,
) -> FileProcessResult {
    if binary_files.contains(file_path) {
        if is_image_file(file_path) {
            return FileProcessResult::Image {
                path: file_path.to_string(),
            };
        }
        return FileProcessResult::Binary {
            path: file_path.to_string(),
        };
    }

    let contents = FileContents::from_batch(
        file_path,
        old_path,
        base_contents,
        head_contents,
        index_contents,
        repo_path,
    );
    FileProcessResult::Diff(compute_four_way_diff(DiffInput {
        path: file_path,
        base: contents.base.as_deref(),
        head: contents.head.as_deref(),
        index: contents.index.as_deref(),
        working: contents.working.as_deref(),
        old_path,
    }))
}

pub(super) fn git_compute_single_file_diff(
    repo_path: &Path,
    file_path: &str,
    old_path: Option<&str>,
    merge_base: &str,
) -> Option<FileDiff> {
    if is_binary_file(repo_path, file_path) {
        return None;
    }

    let contents = FileContents::fetch(repo_path, file_path, old_path, merge_base);

    if contents.all_equal() {
        return None;
    }

    Some(compute_four_way_diff(DiffInput {
        path: file_path,
        base: contents.base.as_deref(),
        head: contents.head.as_deref(),
        index: contents.index.as_deref(),
        working: contents.working.as_deref(),
        old_path,
    }))
}

/// Collect all file paths that need content fetching (primary + old_path for renames),
/// excluding binary files.
pub(super) fn collect_fetch_paths<'a>(
    changed_files: &'a [super::changed_files::ChangedFile],
    binary_files: &HashSet<String>,
) -> Vec<&'a str> {
    let mut paths = Vec::new();
    for file in changed_files {
        if binary_files.contains(&file.path) {
            continue;
        }
        paths.push(file.path.as_str());
        if let Some(old) = &file.old_path {
            paths.push(old.as_str());
        }
    }
    paths.sort_unstable();
    paths.dedup();
    paths
}

pub(super) fn git_compute_refresh(
    repo_path: &Path,
    base_branch: &str,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<RefreshResult> {
    let merge_base = get_merge_base_preferring_origin(repo_path, base_branch)
        .unwrap_or_default();

    if cancel_flag.load(Ordering::Relaxed) {
        return Err(anyhow!("refresh cancelled"));
    }

    let (changed_files_result, binary_files) = std::thread::scope(|s| {
        let changed_handle = s.spawn(|| get_all_changed_files(repo_path, &merge_base));
        let binary_handle = s.spawn(|| get_binary_files(repo_path, &merge_base));

        (
            changed_handle.join().expect("changed files thread panicked"),
            binary_handle.join().expect("binary files thread panicked"),
        )
    });

    let changed_files = changed_files_result.context("Failed to get changed files")?;

    if cancel_flag.load(Ordering::Relaxed) {
        return Err(anyhow!("refresh cancelled"));
    }

    // Batch pre-fetch: 3 cat-file --batch calls instead of 3*N git show calls.
    let fetch_paths = collect_fetch_paths(&changed_files, &binary_files);
    let (base_contents, head_contents, index_contents) = std::thread::scope(|s| {
        let base_handle = s.spawn(|| {
            if merge_base.is_empty() {
                HashMap::new()
            } else {
                batch_file_contents(repo_path, &fetch_paths, &merge_base)
            }
        });
        let head_handle = s.spawn(|| batch_file_contents(repo_path, &fetch_paths, "HEAD"));
        let index_handle = s.spawn(|| batch_file_contents(repo_path, &fetch_paths, ""));

        (
            base_handle.join().expect("base batch thread panicked"),
            head_handle.join().expect("head batch thread panicked"),
            index_handle.join().expect("index batch thread panicked"),
        )
    });

    let results = process_files_parallel(&changed_files, |file| {
        process_single_file_batched(
            repo_path,
            &file.path,
            file.old_path.as_deref(),
            &binary_files,
            &base_contents,
            &head_contents,
            &index_contents,
        )
    });

    if cancel_flag.load(Ordering::Relaxed) {
        return Err(anyhow!("refresh cancelled"));
    }

    let assembled = assemble_results(results);
    let files = assembled.files;
    let lines = assembled.lines;

    let current_branch = get_current_branch(repo_path).unwrap_or(None);
    let divergence = compute_upstream_divergence(repo_path, &merge_base, base_branch);

    let metrics = DiffMetrics {
        total_lines: lines.len(),
        file_count: files.len(),
    };

    let file_paths: Vec<&str> = files
        .iter()
        .filter_map(|f| f.lines.first())
        .filter_map(|l| l.file_path.as_deref())
        .collect();
    let file_links = compute_file_links(&file_paths);

    Ok(RefreshResult {
        files,
        lines,
        base_identifier: merge_base,
        base_label: Some(base_branch.to_string()),
        current_branch,
        metrics,
        file_links,
        stack_position: None,
        bookmark_name: None,
        revision_id: None,
        divergence,
    })
}
