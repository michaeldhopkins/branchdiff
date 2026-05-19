use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;

use crate::diff::{DiffLine, FileDiff, LineSource};

use super::{vcs_thread_pool, PARALLEL_THRESHOLD};

/// Result of processing a single file in a VCS refresh.
pub(crate) enum FileProcessResult {
    Diff(FileDiff),
    Binary { path: String },
    Image { path: String },
    /// Skipped because the refresh was cancelled before this file ran. Carries
    /// no diff data — callers throw the entire batch away once they observe
    /// the cancel flag, so the empty payload never reaches the UI.
    Cancelled,
}

/// Assembled output from converting `FileProcessResult`s into flat lines + grouped files.
pub(crate) struct AssembledDiff {
    pub files: Vec<FileDiff>,
    pub lines: Vec<DiffLine>,
}

/// Convert per-file processing results into the flat line list and grouped file diffs
/// needed by `RefreshResult`. Runs cross-file block matching before flattening so
/// that `move_target` annotations propagate to the flat line list.
pub(crate) fn assemble_results(results: Vec<FileProcessResult>) -> AssembledDiff {
    let mut files = Vec::new();

    for result in results {
        match result {
            FileProcessResult::Diff(file_diff) => {
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
                files.push(FileDiff::new(vec![header, marker]));
            }
            FileProcessResult::Image { path } => {
                let header = DiffLine::file_header(&path);
                let marker = DiffLine::image_marker(&path);
                files.push(FileDiff::new(vec![header, marker]));
            }
            FileProcessResult::Cancelled => {}
        }
    }

    // Match moved blocks across files before flattening
    crate::diff::block::match_blocks(&mut files);

    // Flatten into the line list (now includes move_target annotations)
    let mut lines = Vec::new();
    for file in &files {
        lines.extend(file.lines.iter().cloned());
        lines.push(DiffLine::new(LineSource::Base, String::new(), ' ', None));
    }

    AssembledDiff { files, lines }
}

/// Process files using the thread pool when the count exceeds `PARALLEL_THRESHOLD`,
/// falling back to serial iteration for small batches.
///
/// Polls `cancel` before each item so that once a refresh is signalled to abort,
/// the remaining queued files are skipped instantly instead of running their
/// (potentially expensive) per-file work. The diff result for skipped items is
/// a header-only stub; callers discard the whole batch when they observe the
/// cancel flag after this returns.
pub(crate) fn process_files_parallel<T, F>(
    items: &[T],
    cancel: &Arc<AtomicBool>,
    process: F,
) -> Vec<FileProcessResult>
where
    T: Sync,
    F: Fn(&T) -> FileProcessResult + Sync,
{
    let guarded = |item: &T| -> FileProcessResult {
        if cancel.load(Ordering::Relaxed) {
            FileProcessResult::Cancelled
        } else {
            process(item)
        }
    };

    if items.len() >= PARALLEL_THRESHOLD {
        vcs_thread_pool().install(|| items.par_iter().map(&guarded).collect())
    } else {
        items.iter().map(guarded).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffLine, FileDiff, LineSource};

    fn diff_result(path: &str) -> FileProcessResult {
        FileProcessResult::Diff(FileDiff::new(vec![
                DiffLine::file_header(path),
                DiffLine::new(
                    LineSource::Base,
                    "content".to_string(),
                    ' ',
                    None,
                ),
        ]))
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
        // Each file: its lines + 1 trailing separator
        // Diff: header + content + sep = 3, Binary: header + marker + sep = 3, Image: same = 3
        assert_eq!(assembled.lines.len(), 9);
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
        assert_eq!(assembled.lines.len(), 3); // header + marker + trailing separator

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
        assert_eq!(assembled.lines.len(), 3); // header + marker + trailing separator

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
        let cancel = Arc::new(AtomicBool::new(false));
        let results = process_files_parallel(&items, &cancel, |path| diff_result(path));
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_process_files_parallel_parallel_path() {
        let items: Vec<String> = (0..5).map(|i| format!("file{i}.rs")).collect();
        let cancel = Arc::new(AtomicBool::new(false));
        let results = process_files_parallel(&items, &cancel, |path| diff_result(path));
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn test_process_files_parallel_serial_skips_when_cancel_set() {
        // Below PARALLEL_THRESHOLD: serial iteration. With cancel pre-set, the
        // closure must never be invoked — that's the cheap-bail behaviour the
        // watchdog relies on to keep CPU from stacking on huge files.
        use std::sync::atomic::AtomicUsize;
        let items = vec!["a.rs", "b.rs", "c.rs"];
        let cancel = Arc::new(AtomicBool::new(true));
        let invoked = AtomicUsize::new(0);

        let results = process_files_parallel(&items, &cancel, |path| {
            invoked.fetch_add(1, Ordering::Relaxed);
            diff_result(path)
        });

        assert_eq!(invoked.load(Ordering::Relaxed), 0,
            "per-file processor must not be called when cancel is pre-set");
        assert_eq!(results.len(), 3);
        assert!(matches!(results[0], FileProcessResult::Cancelled));
        assert!(matches!(results[1], FileProcessResult::Cancelled));
        assert!(matches!(results[2], FileProcessResult::Cancelled));
    }

    #[test]
    fn test_assemble_drops_cancelled_results() {
        // Cancelled stubs must not surface in the assembled diff — they would
        // overwrite real data with empty placeholders if a stale refresh slipped
        // through. Real callers discard the entire batch when they observe the
        // cancel flag after the fact, but this guard belt-and-braces the path.
        let results = vec![
            diff_result("src/a.rs"),
            FileProcessResult::Cancelled,
            FileProcessResult::Cancelled,
        ];

        let assembled = assemble_results(results);
        assert_eq!(assembled.files.len(), 1);
    }
}
