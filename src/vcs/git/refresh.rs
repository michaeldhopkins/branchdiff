use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use rayon::prelude::*;

use crate::diff::{compute_four_way_diff, DiffInput, DiffLine, FileDiff, LineSource};
use crate::file_links::compute_file_links;
use crate::image_diff::is_image_file;
use crate::limits::DiffMetrics;
use crate::vcs::{vcs_thread_pool, RefreshResult, PARALLEL_THRESHOLD};

use super::changed_files::get_all_changed_files;
use super::commands::{
    get_binary_files, get_current_branch, get_file_at_ref, get_merge_base_preferring_origin,
    get_working_tree_file, is_binary_file,
};

enum FileProcessResult {
    Diff(FileDiff),
    Binary { path: String },
    Image { path: String },
}

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

    fn all_equal(&self) -> bool {
        self.base == self.working && self.base == self.head && self.base == self.index
    }
}

fn process_single_file(
    repo_path: &Path,
    file_path: &str,
    old_path: Option<&str>,
    merge_base: &str,
    binary_files: &HashSet<String>,
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

    let contents = FileContents::fetch(repo_path, file_path, old_path, merge_base);
    let file_diff = compute_four_way_diff(DiffInput {
        path: file_path,
        base: contents.base.as_deref(),
        head: contents.head.as_deref(),
        index: contents.index.as_deref(),
        working: contents.working.as_deref(),
        old_path,
    });

    FileProcessResult::Diff(file_diff)
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

    let results: Vec<FileProcessResult> = if changed_files.len() >= PARALLEL_THRESHOLD {
        vcs_thread_pool().install(|| {
            changed_files
                .par_iter()
                .map(|file| process_single_file(repo_path, &file.path, file.old_path.as_deref(), &merge_base, &binary_files))
                .collect()
        })
    } else {
        changed_files
            .iter()
            .map(|file| process_single_file(repo_path, &file.path, file.old_path.as_deref(), &merge_base, &binary_files))
            .collect()
    };

    if cancel_flag.load(Ordering::Relaxed) {
        return Err(anyhow!("refresh cancelled"));
    }

    let mut files = Vec::new();
    let mut lines = Vec::new();

    for result in results {
        match result {
            FileProcessResult::Diff(file_diff) => {
                lines.extend(file_diff.lines.iter().cloned());
                lines.push(DiffLine::new(LineSource::Base, String::new(), ' ', None));
                files.push(file_diff);
            }
            FileProcessResult::Binary { path } => {
                let header = DiffLine::file_header(&path);
                let marker = DiffLine::new(
                    LineSource::Base,
                    "[binary file]".to_string(),
                    ' ',
                    None,
                );
                lines.push(header.clone());
                lines.push(marker.clone());
                files.push(FileDiff {
                    lines: vec![header, marker],
                });
            }
            FileProcessResult::Image { path } => {
                let header = DiffLine::file_header(&path);
                let marker = DiffLine::image_marker(&path);
                lines.push(header.clone());
                lines.push(marker.clone());
                files.push(FileDiff {
                    lines: vec![header, marker],
                });
            }
        }
    }

    let current_branch = get_current_branch(repo_path).unwrap_or(None);

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
        revision_id: None,
    })
}
