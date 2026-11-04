use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, anyhow};

use crate::diff::{DiffInput, FileDiff, compute_four_way_diff, compute_four_way_diff_cancellable};
use crate::file_links::compute_file_links;
use crate::image_diff::is_image_file;
use crate::limits::DiffMetrics;
use crate::vcs::shared::{FileProcessResult, assemble_results, process_files_parallel};
use crate::vcs::{RefreshResult, UpstreamDivergence};

use super::changed_files::{ChangedFile, get_all_changed_files, get_changed_files_between};
use super::commands::{
    batch_file_contents, compute_upstream_divergence, get_binary_files, get_binary_files_between,
    get_current_branch, get_file_at_ref, get_merge_base_preferring_origin_for,
    get_working_tree_file, is_binary_file, is_binary_file_between,
};
use super::highlight::{GitHighlightBatch, GitHighlightSpec, compute_single_file_highlight};

struct FileContents {
    base: Option<String>,
    head: Option<String>,
    index: Option<String>,
    working: Option<String>,
}

impl FileContents {
    fn fetch(
        repo_path: &Path,
        file_path: &str,
        old_path: Option<&str>,
        merge_base: &str,
        head_ref: Option<&str>,
    ) -> Self {
        let base_path = old_path.unwrap_or(file_path);

        let base = if merge_base.is_empty() {
            None
        } else {
            get_file_at_ref(repo_path, base_path, merge_base).ok().flatten()
        };

        if let Some(target_ref) = head_ref {
            let head = get_file_at_ref(repo_path, file_path, target_ref).ok().flatten();
            return Self { base, head: head.clone(), index: head.clone(), working: head };
        }

        let head = get_file_at_ref(repo_path, file_path, "HEAD").ok().flatten().or_else(|| {
            old_path.and_then(|p| get_file_at_ref(repo_path, p, "HEAD").ok().flatten())
        });

        let index = get_file_at_ref(repo_path, file_path, "")
            .ok()
            .flatten()
            .or_else(|| old_path.and_then(|p| get_file_at_ref(repo_path, p, "").ok().flatten()));

        Self {
            base,
            head,
            index,
            working: get_working_tree_file(repo_path, file_path).ok().flatten(),
        }
    }

    fn from_batch(
        file_path: &str,
        old_path: Option<&str>,
        base_contents: &HashMap<String, String>,
        head_contents: &HashMap<String, String>,
        index_contents: &HashMap<String, String>,
        repo_path: &Path,
        working_from_head: bool,
    ) -> Self {
        let base_path = old_path.unwrap_or(file_path);
        let base = base_contents.get(base_path).cloned();
        if working_from_head {
            let head = head_contents.get(file_path).cloned();
            return Self { base, head: head.clone(), index: head.clone(), working: head };
        }
        Self {
            base,
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
    working_from_head: bool,
    highlight_batch: Option<&GitHighlightBatch>,
    cancel: &AtomicBool,
) -> FileProcessResult {
    if cancel.load(Ordering::Relaxed) {
        return FileProcessResult::Cancelled;
    }

    if binary_files.contains(file_path) {
        if is_image_file(file_path) {
            return FileProcessResult::Image { path: file_path.to_string() };
        }
        return FileProcessResult::Binary { path: file_path.to_string() };
    }

    let contents = FileContents::from_batch(
        file_path, old_path, base_contents, head_contents, index_contents, repo_path,
        working_from_head,
    );
    let mut file_diff = compute_four_way_diff_cancellable(
        DiffInput {
            path: file_path,
            base: contents.base.as_deref(),
            head: contents.head.as_deref(),
            index: contents.index.as_deref(),
            working: contents.working.as_deref(),
            old_path,
        },
        cancel,
    );
    if let Some(hl) = highlight_batch {
        file_diff.highlight = hl.compute_for_file(
            &file_diff,
            file_path,
            old_path,
            contents.base.as_deref(),
            contents.working.as_deref(),
        );
    }
    FileProcessResult::Diff(file_diff)
}

pub(super) fn git_compute_single_file_diff(
    repo_path: &Path,
    file_path: &str,
    old_path: Option<&str>,
    merge_base: &str,
    head_ref: Option<&str>,
    highlight_spec: Option<&GitHighlightSpec>,
) -> Option<FileDiff> {
    let is_binary = match head_ref {
        Some(target_ref) => is_binary_file_between(repo_path, file_path, merge_base, target_ref),
        None => is_binary_file(repo_path, file_path),
    };
    if is_binary {
        return None;
    }

    let contents = FileContents::fetch(repo_path, file_path, old_path, merge_base, head_ref);
    if contents.all_equal() {
        return None;
    }

    let mut file_diff = compute_four_way_diff(DiffInput {
        path: file_path,
        base: contents.base.as_deref(),
        head: contents.head.as_deref(),
        index: contents.index.as_deref(),
        working: contents.working.as_deref(),
        old_path,
    });
    if let Some(spec) = highlight_spec {
        file_diff.highlight = compute_single_file_highlight(
            repo_path,
            spec,
            merge_base,
            head_ref,
            &file_diff,
            (file_path, old_path),
            (contents.base.as_deref(), contents.working.as_deref()),
        )
        .unwrap_or_else(|_| super::highlight::unhighlighted(&spec.label));
    }
    Some(file_diff)
}

/// File paths needing content fetch (primary + old_path for renames), excluding binary files.
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

struct RefreshContext<'a> {
    repo_path: &'a Path,
    base_branch: &'a str,
    head_ref: &'a str,
    head_target: Option<(&'a str, &'a str)>,
    highlight_spec: Option<&'a GitHighlightSpec>,
}

struct RefreshInputs {
    base_contents: HashMap<String, String>,
    head_contents: HashMap<String, String>,
    index_contents: HashMap<String, String>,
    current_branch: Option<String>,
    divergence: Option<UpstreamDivergence>,
    highlight_batch: Option<GitHighlightBatch>,
}

/// Batch pre-fetch (cat-file --batch, not N git show calls); branch, divergence and highlight git calls overlap it.
fn prefetch_refresh_inputs(
    ctx: &RefreshContext<'_>,
    merge_base: &str,
    fetch_paths: &[&str],
    binary_files: &HashSet<String>,
) -> Result<RefreshInputs> {
    let RefreshContext { repo_path, base_branch, head_ref, head_target, highlight_spec } = *ctx;
    std::thread::scope(|s| {
        let branch_handle = s.spawn(|| match head_target {
            Some((_, label)) => Some(label.to_string()),
            None => get_current_branch(repo_path).unwrap_or(None),
        });
        let divergence_handle =
            s.spawn(|| compute_upstream_divergence(repo_path, merge_base, base_branch));
        let base_handle = s.spawn(|| {
            if merge_base.is_empty() {
                HashMap::new()
            } else {
                batch_file_contents(repo_path, fetch_paths, merge_base)
            }
        });
        let head_handle = s.spawn(|| batch_file_contents(repo_path, fetch_paths, head_ref));
        let index_handle = s.spawn(|| {
            if head_target.is_some() {
                HashMap::new()
            } else {
                batch_file_contents(repo_path, fetch_paths, "")
            }
        });
        let highlight_handle = s.spawn(|| {
            highlight_spec.map(|spec| {
                GitHighlightBatch::fetch(
                    repo_path,
                    spec,
                    merge_base,
                    head_target.map(|(r, _)| r),
                    binary_files,
                )
            })
        });

        let highlight_batch =
            highlight_handle.join().expect("highlight batch thread panicked").transpose()?;
        Ok(RefreshInputs {
            base_contents: base_handle.join().expect("base batch thread panicked"),
            head_contents: head_handle.join().expect("head batch thread panicked"),
            index_contents: index_handle.join().expect("index batch thread panicked"),
            current_branch: branch_handle.join().expect("current branch thread panicked"),
            divergence: divergence_handle.join().expect("divergence thread panicked"),
            highlight_batch,
        })
    })
}

fn list_changed_and_binary(
    repo_path: &Path,
    merge_base: &str,
    head_target: Option<(&str, &str)>,
) -> Result<(Vec<ChangedFile>, HashSet<String>)> {
    let (changed_files_result, binary_files) = std::thread::scope(|s| {
        let changed_handle = s.spawn(|| match head_target {
            Some((target_ref, _)) => get_changed_files_between(repo_path, merge_base, target_ref),
            None => get_all_changed_files(repo_path, merge_base),
        });
        let binary_handle = s.spawn(|| match head_target {
            Some((target_ref, _)) => get_binary_files_between(repo_path, merge_base, target_ref),
            None => get_binary_files(repo_path, merge_base),
        });

        (
            changed_handle.join().expect("changed files thread panicked"),
            binary_handle.join().expect("binary files thread panicked"),
        )
    });
    Ok((changed_files_result.context("Failed to get changed files")?, binary_files))
}

pub(super) fn git_compute_refresh(
    repo_path: &Path,
    base_branch: &str,
    base_label: &str,
    head_target: Option<(&str, &str)>,
    highlight_spec: Option<&GitHighlightSpec>,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<RefreshResult> {
    let head_ref = head_target.map_or("HEAD", |(r, _)| r);
    let merge_base =
        get_merge_base_preferring_origin_for(repo_path, base_branch, head_ref).unwrap_or_default();

    if cancel_flag.load(Ordering::Relaxed) {
        return Err(anyhow!("refresh cancelled"));
    }

    let (changed_files, binary_files) =
        list_changed_and_binary(repo_path, &merge_base, head_target)?;

    if cancel_flag.load(Ordering::Relaxed) {
        return Err(anyhow!("refresh cancelled"));
    }

    let fetch_paths = collect_fetch_paths(&changed_files, &binary_files);
    let inputs = prefetch_refresh_inputs(
        &RefreshContext { repo_path, base_branch, head_ref, head_target, highlight_spec },
        &merge_base,
        &fetch_paths,
        &binary_files,
    )?;
    let RefreshInputs {
        base_contents,
        head_contents,
        index_contents,
        current_branch,
        divergence,
        highlight_batch,
    } = inputs;

    let working_from_head = head_target.is_some();
    let results = process_files_parallel(&changed_files, cancel_flag, |file| {
        process_single_file_batched(
            repo_path,
            &file.path,
            file.old_path.as_deref(),
            &binary_files,
            &base_contents,
            &head_contents,
            &index_contents,
            working_from_head,
            highlight_batch.as_ref(),
            cancel_flag,
        )
    });

    if cancel_flag.load(Ordering::Relaxed) {
        return Err(anyhow!("refresh cancelled"));
    }

    let assembled = assemble_results(results);
    let mut files = assembled.files;
    if let Some(hl) = &highlight_batch {
        hl.stamp_assembled_files(&mut files);
    }
    let lines = assembled.lines;

    let metrics = DiffMetrics { total_lines: lines.len(), file_count: files.len() };

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
        base_label: Some(base_label.to_string()),
        current_branch,
        metrics,
        file_links,
        stack_position: None,
        bookmark_name: None,
        revision_id: None,
        divergence,
        // git has no divergence concept; the trait default returns Ok(empty).
    })
}
