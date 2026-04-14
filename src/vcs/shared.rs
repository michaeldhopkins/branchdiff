use rayon::prelude::*;

use crate::diff::{DiffLine, FileDiff, LineSource};

use super::{vcs_thread_pool, PARALLEL_THRESHOLD};

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

}
