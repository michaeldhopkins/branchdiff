use std::path::Path;
use std::process::{Command, Output};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::diff::{DiffLine, FileDiff, LineSource};

use super::{vcs_thread_pool, PARALLEL_THRESHOLD};

const MAX_RETRIES: u32 = 3;
const BASE_RETRY_DELAY_MS: u64 = 100;

/// Check whether a formatted VCS error message indicates a transient condition
/// that may resolve on its own (e.g., "The working copy is stale").
pub fn is_transient_vcs_error(error_msg: &str) -> bool {
    error_msg.contains("stale")
}

/// Run a VCS command with exponential backoff on transient errors.
///
/// Retries up to `MAX_RETRIES` times with 100ms/200ms/400ms delays when
/// `is_transient` returns true for the stderr output.
pub(crate) fn run_vcs_with_retry(
    program: &str,
    repo_path: &Path,
    args: &[&str],
    is_transient: fn(&str) -> bool,
) -> Result<Output> {
    let context_msg = format!("failed to run {program}");
    for attempt in 0..=MAX_RETRIES {
        let output = Command::new(program)
            .args(args)
            .current_dir(repo_path)
            .output()
            .context(context_msg.clone())?;

        if output.status.success() {
            return Ok(output);
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        if !is_transient(&stderr) || attempt == MAX_RETRIES {
            return Ok(output);
        }

        let delay = Duration::from_millis(BASE_RETRY_DELAY_MS * (1 << attempt));
        thread::sleep(delay);
    }

    Command::new(program)
        .args(args)
        .current_dir(repo_path)
        .output()
        .context(context_msg)
}

/// Result of processing a single file in a VCS refresh.
pub(crate) enum FileProcessResult {
    Diff(FileDiff),
    Binary { path: String },
    Image { path: String },
}

/// Assembled output from converting `FileProcessResult`s into flat lines + grouped files.
pub(crate) struct AssembledDiff {
    pub files: Vec<FileDiff>,
    pub lines: Vec<DiffLine>,
}

/// Convert per-file processing results into the flat line list and grouped file diffs
/// needed by `RefreshResult`.
pub(crate) fn assemble_results(results: Vec<FileProcessResult>) -> AssembledDiff {
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

    AssembledDiff { files, lines }
}

/// Process files using the thread pool when the count exceeds `PARALLEL_THRESHOLD`,
/// falling back to serial iteration for small batches.
pub(crate) fn process_files_parallel<T, F>(items: &[T], process: F) -> Vec<FileProcessResult>
where
    T: Sync,
    F: Fn(&T) -> FileProcessResult + Sync,
{
    if items.len() >= PARALLEL_THRESHOLD {
        vcs_thread_pool().install(|| items.par_iter().map(&process).collect())
    } else {
        items.iter().map(process).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffLine, FileDiff, LineSource};

    fn diff_result(path: &str) -> FileProcessResult {
        FileProcessResult::Diff(FileDiff {
            lines: vec![
                DiffLine::file_header(path),
                DiffLine::new(
                    LineSource::Base,
                    "content".to_string(),
                    ' ',
                    None,
                ),
            ],
        })
    }

    #[test]
    fn test_assemble_diff_results() {
        let results = vec![
            diff_result("src/a.rs"),
            FileProcessResult::Binary {
                path: "data.bin".to_string(),
            },
            FileProcessResult::Image {
                path: "logo.png".to_string(),
            },
        ];

        let assembled = assemble_results(results);
        assert_eq!(assembled.files.len(), 3);
        // Diff file: header + content + trailing blank = 3 lines
        // Binary: header + marker = 2 lines
        // Image: header + marker = 2 lines
        assert_eq!(assembled.lines.len(), 7);
    }

    #[test]
    fn test_assemble_empty_results() {
        let assembled = assemble_results(vec![]);
        assert!(assembled.files.is_empty());
        assert!(assembled.lines.is_empty());
    }

    #[test]
    fn test_assemble_binary_creates_header_and_marker() {
        let results = vec![FileProcessResult::Binary {
            path: "data.bin".to_string(),
        }];

        let assembled = assemble_results(results);
        assert_eq!(assembled.files.len(), 1);
        assert_eq!(assembled.lines.len(), 2);

        let file = &assembled.files[0];
        assert_eq!(file.lines.len(), 2);
        assert_eq!(
            file.lines[0].file_path.as_deref(),
            Some("data.bin"),
        );
        assert_eq!(file.lines[1].content, "[binary file]");
    }

    #[test]
    fn test_assemble_image_creates_header_and_marker() {
        let results = vec![FileProcessResult::Image {
            path: "logo.png".to_string(),
        }];

        let assembled = assemble_results(results);
        assert_eq!(assembled.files.len(), 1);
        assert_eq!(assembled.lines.len(), 2);

        let file = &assembled.files[0];
        assert_eq!(file.lines.len(), 2);
        assert_eq!(
            file.lines[0].file_path.as_deref(),
            Some("logo.png"),
        );
        assert_eq!(file.lines[1].content, "[image]");
    }

    #[test]
    fn test_process_files_parallel_serial_path() {
        let items = vec!["a.rs", "b.rs"];
        let results = process_files_parallel(&items, |path| {
            diff_result(path)
        });
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_process_files_parallel_parallel_path() {
        let items: Vec<String> = (0..5).map(|i| format!("file{i}.rs")).collect();
        let results = process_files_parallel(&items, |path| {
            diff_result(path)
        });
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn test_is_transient_vcs_error_detects_stale() {
        assert!(is_transient_vcs_error("The working copy is stale"));
        assert!(is_transient_vcs_error(
            "jj diff --summary failed: Error: The working copy is stale (not updated since op 291e6b6bf66c)"
        ));
    }

    #[test]
    fn test_is_transient_vcs_error_rejects_non_transient() {
        assert!(!is_transient_vcs_error("Config error: no such revision"));
        assert!(!is_transient_vcs_error(""));
        assert!(!is_transient_vcs_error("jj not found"));
    }
}
