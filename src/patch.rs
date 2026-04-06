//! Git unified patch format generation.
//!
//! Converts branchdiff's DiffLine representation into standard git patch format
//! suitable for `git apply` or GitHub `.diff` files.

use std::fmt;

use crate::diff::{DiffLine, LineSource};

/// Number of context lines to include around changes in hunks.
const CONTEXT_LINES: usize = 3;

/// A line in a patch with its prefix character.
#[derive(Debug, Clone)]
struct PatchLine {
    /// Prefix character: ' ' for context, '+' for addition, '-' for deletion
    prefix: char,
    /// Line content (without newline)
    content: String,
    /// Original line number in the old file (for context and deletions)
    old_line: Option<usize>,
    /// Line number in the new file (for context and additions)
    new_line: Option<usize>,
}

/// A hunk representing a contiguous section of changes.
#[derive(Debug)]
struct Hunk {
    /// Starting line number in the old file (1-based)
    old_start: usize,
    /// Number of lines from the old file in this hunk
    old_count: usize,
    /// Starting line number in the new file (1-based)
    new_start: usize,
    /// Number of lines from the new file in this hunk
    new_count: usize,
    /// Lines in this hunk
    lines: Vec<PatchLine>,
}

impl Hunk {
    /// Formats the hunk header per git spec: count is omitted when it equals 1.
    fn header(&self) -> String {
        let old_range = format_range(self.old_start, self.old_count);
        let new_range = format_range(self.new_start, self.new_count);
        format!("@@ -{} +{} @@", old_range, new_range)
    }
}

/// Formats a line range for hunk headers. Per git spec, count is omitted when 1.
fn format_range(start: usize, count: usize) -> String {
    if count == 1 {
        start.to_string()
    } else {
        format!("{},{}", start, count)
    }
}

impl fmt::Display for Hunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.header())?;
        for line in &self.lines {
            writeln!(f, "{}{}", line.prefix, line.content)?;
        }
        Ok(())
    }
}

/// A patch for a single file.
#[derive(Debug)]
struct FilePatch {
    path: String,
    hunks: Vec<Hunk>,
    is_new_file: bool,
    is_deleted_file: bool,
}

impl FilePatch {
    fn new(path: String, hunks: Vec<Hunk>) -> Self {
        // Detect new file: all hunks have old_count == 0
        let is_new_file = !hunks.is_empty() && hunks.iter().all(|h| h.old_count == 0);
        // Detect deleted file: all hunks have new_count == 0
        let is_deleted_file = !hunks.is_empty() && hunks.iter().all(|h| h.new_count == 0);

        Self {
            path,
            hunks,
            is_new_file,
            is_deleted_file,
        }
    }
}

impl fmt::Display for FilePatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.hunks.is_empty() {
            return Ok(());
        }

        writeln!(f, "diff --git a/{} b/{}", self.path, self.path)?;

        // Per git spec: new files use /dev/null for ---, deleted files use /dev/null for +++
        if self.is_new_file {
            writeln!(f, "new file mode 100644")?;
            writeln!(f, "--- /dev/null")?;
            writeln!(f, "+++ b/{}", self.path)?;
        } else if self.is_deleted_file {
            writeln!(f, "deleted file mode 100644")?;
            writeln!(f, "--- a/{}", self.path)?;
            writeln!(f, "+++ /dev/null")?;
        } else {
            writeln!(f, "--- a/{}", self.path)?;
            writeln!(f, "+++ b/{}", self.path)?;
        }

        for hunk in &self.hunks {
            write!(f, "{}", hunk)?;
        }

        Ok(())
    }
}

/// Determines the patch prefix for a given LineSource.
/// Returns None for lines that should be skipped.
fn line_source_to_prefix(source: &LineSource) -> Option<char> {
    match source {
        // Context lines
        LineSource::Base => Some(' '),

        // Additions
        LineSource::Committed | LineSource::Staged | LineSource::Unstaged => Some('+'),

        // Deletions
        LineSource::DeletedBase | LineSource::DeletedCommitted | LineSource::DeletedStaged => {
            Some('-')
        }

        // Skip these
        LineSource::CanceledCommitted
        | LineSource::CanceledStaged
        | LineSource::FileHeader
        | LineSource::Elided => None,
    }
}

/// Converts DiffLines into PatchLines, filtering out non-patch lines.
fn diff_lines_to_patch_lines(lines: &[DiffLine]) -> Vec<PatchLine> {
    let mut patch_lines = Vec::new();
    let mut old_line_num = 0usize;
    let mut new_line_num = 0usize;

    for diff_line in lines {
        let Some(prefix) = line_source_to_prefix(&diff_line.source) else {
            continue;
        };

        // Track line numbers based on prefix
        let (old_line, new_line) = match prefix {
            ' ' => {
                // Context: appears in both old and new
                old_line_num += 1;
                new_line_num += 1;
                (Some(old_line_num), Some(new_line_num))
            }
            '-' => {
                // Deletion: only in old
                old_line_num += 1;
                (Some(old_line_num), None)
            }
            '+' => {
                // Addition: only in new
                new_line_num += 1;
                (None, Some(new_line_num))
            }
            _ => (None, None),
        };

        patch_lines.push(PatchLine {
            prefix,
            content: diff_line.content.clone(),
            old_line,
            new_line,
        });
    }

    patch_lines
}

/// Identifies indices of lines that are changes (additions or deletions).
fn find_change_indices(lines: &[PatchLine]) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.prefix == '+' || line.prefix == '-')
        .map(|(i, _)| i)
        .collect()
}

/// Builds hunks from patch lines with appropriate context.
fn build_hunks(lines: &[PatchLine]) -> Vec<Hunk> {
    if lines.is_empty() {
        return Vec::new();
    }

    let change_indices = find_change_indices(lines);
    if change_indices.is_empty() {
        return Vec::new();
    }

    // Determine which lines to include in hunks (changes + context)
    let mut included = vec![false; lines.len()];

    for &idx in &change_indices {
        // Include the change itself
        included[idx] = true;

        // Include context before
        let start = idx.saturating_sub(CONTEXT_LINES);
        for item in included.iter_mut().take(idx).skip(start) {
            *item = true;
        }

        // Include context after
        let end = (idx + CONTEXT_LINES + 1).min(lines.len());
        for item in included.iter_mut().take(end).skip(idx + 1) {
            *item = true;
        }
    }

    // Group consecutive included lines into hunks
    let mut hunks = Vec::new();
    let mut hunk_start: Option<usize> = None;

    for (i, &inc) in included.iter().enumerate() {
        match (inc, hunk_start) {
            (true, None) => {
                hunk_start = Some(i);
            }
            (false, Some(start)) => {
                hunks.push(create_hunk(&lines[start..i]));
                hunk_start = None;
            }
            _ => {}
        }
    }

    // Handle final hunk
    if let Some(start) = hunk_start {
        hunks.push(create_hunk(&lines[start..]));
    }

    hunks
}

/// Creates a Hunk from a slice of PatchLines.
fn create_hunk(lines: &[PatchLine]) -> Hunk {
    let mut old_count = 0;
    let mut new_count = 0;
    let mut old_start = None;
    let mut new_start = None;

    for line in lines {
        match line.prefix {
            ' ' => {
                old_count += 1;
                new_count += 1;
                if old_start.is_none() {
                    old_start = line.old_line;
                }
                if new_start.is_none() {
                    new_start = line.new_line;
                }
            }
            '-' => {
                old_count += 1;
                if old_start.is_none() {
                    old_start = line.old_line;
                }
            }
            '+' => {
                new_count += 1;
                if new_start.is_none() {
                    new_start = line.new_line;
                }
            }
            _ => {}
        }
    }

    // Per git spec:
    // - New files (no old lines): old_start = 0, old_count = 0
    // - Deleted files (no new lines): new_start = 0, new_count = 0
    // - Otherwise: start defaults to 1 if somehow unset
    let old_start = if old_count == 0 { 0 } else { old_start.unwrap_or(1) };
    let new_start = if new_count == 0 { 0 } else { new_start.unwrap_or(1) };

    Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        lines: lines.to_vec(),
    }
}

/// Generates a git unified patch from DiffLines.
///
/// The output is suitable for `git apply` or as a `.diff` file.
/// Lines without a file_path are skipped since patches require file context.
pub fn generate_patch(lines: &[DiffLine]) -> String {
    // Group lines by file path, skipping lines without a path
    let mut files: Vec<(String, Vec<&DiffLine>)> = Vec::new();
    let mut current_path: Option<String> = None;

    for line in lines {
        let path = match &line.file_path {
            Some(p) if !p.is_empty() => p.clone(),
            _ => continue, // Skip lines without a valid file path
        };

        if current_path.as_ref() != Some(&path) {
            files.push((path.clone(), Vec::new()));
            current_path = Some(path);
        }

        if let Some((_, file_lines)) = files.last_mut() {
            file_lines.push(line);
        }
    }

    // Generate patch for each file
    let mut output = String::new();

    for (path, file_lines) in files {
        let owned_lines: Vec<DiffLine> = file_lines.into_iter().cloned().collect();
        let patch_lines = diff_lines_to_patch_lines(&owned_lines);
        let hunks = build_hunks(&patch_lines);

        let file_patch = FilePatch::new(path, hunks);
        output.push_str(&file_patch.to_string());
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_diff_line(source: LineSource, content: &str, file_path: &str) -> DiffLine {
        DiffLine {
            source,
            content: content.to_string(),
            prefix: match source {
                LineSource::Base => ' ',
                LineSource::Committed | LineSource::Staged | LineSource::Unstaged => '+',
                LineSource::DeletedBase
                | LineSource::DeletedCommitted
                | LineSource::DeletedStaged => '-',
                _ => ' ',
            },
            line_number: None,
            file_path: Some(file_path.to_string()),
            inline_spans: Vec::new(),
            old_content: None,
            change_source: None,
            in_current_bookmark: None,
            block_idx: None,
            move_target: None,
        }
    }

    #[test]
    fn test_simple_addition() {
        let lines = vec![
            make_diff_line(LineSource::Base, "line 1", "test.txt"),
            make_diff_line(LineSource::Base, "line 2", "test.txt"),
            make_diff_line(LineSource::Base, "line 3", "test.txt"),
            make_diff_line(LineSource::Committed, "new line", "test.txt"),
            make_diff_line(LineSource::Base, "line 4", "test.txt"),
            make_diff_line(LineSource::Base, "line 5", "test.txt"),
            make_diff_line(LineSource::Base, "line 6", "test.txt"),
        ];

        let patch = generate_patch(&lines);

        assert!(patch.contains("diff --git a/test.txt b/test.txt"));
        assert!(patch.contains("--- a/test.txt"));
        assert!(patch.contains("+++ b/test.txt"));
        assert!(patch.contains("+new line"));
        assert!(patch.contains("@@ -"));
    }

    #[test]
    fn test_simple_deletion() {
        let lines = vec![
            make_diff_line(LineSource::Base, "line 1", "test.txt"),
            make_diff_line(LineSource::Base, "line 2", "test.txt"),
            make_diff_line(LineSource::Base, "line 3", "test.txt"),
            make_diff_line(LineSource::DeletedCommitted, "deleted line", "test.txt"),
            make_diff_line(LineSource::Base, "line 4", "test.txt"),
            make_diff_line(LineSource::Base, "line 5", "test.txt"),
            make_diff_line(LineSource::Base, "line 6", "test.txt"),
        ];

        let patch = generate_patch(&lines);

        assert!(patch.contains("-deleted line"));
    }

    #[test]
    fn test_mixed_changes() {
        let lines = vec![
            make_diff_line(LineSource::Base, "context", "test.txt"),
            make_diff_line(LineSource::DeletedStaged, "old line", "test.txt"),
            make_diff_line(LineSource::Staged, "new line", "test.txt"),
            make_diff_line(LineSource::Base, "more context", "test.txt"),
        ];

        let patch = generate_patch(&lines);

        assert!(patch.contains("-old line"));
        assert!(patch.contains("+new line"));
    }

    #[test]
    fn test_multiple_files() {
        let lines = vec![
            make_diff_line(LineSource::Base, "file1 line", "file1.txt"),
            make_diff_line(LineSource::Committed, "file1 addition", "file1.txt"),
            make_diff_line(LineSource::Base, "file2 line", "file2.txt"),
            make_diff_line(LineSource::Unstaged, "file2 addition", "file2.txt"),
        ];

        let patch = generate_patch(&lines);

        assert!(patch.contains("diff --git a/file1.txt b/file1.txt"));
        assert!(patch.contains("diff --git a/file2.txt b/file2.txt"));
        assert!(patch.contains("+file1 addition"));
        assert!(patch.contains("+file2 addition"));
    }

    #[test]
    fn test_skips_canceled_lines() {
        let lines = vec![
            make_diff_line(LineSource::Base, "context", "test.txt"),
            make_diff_line(LineSource::CanceledCommitted, "canceled", "test.txt"),
            make_diff_line(LineSource::Committed, "actual change", "test.txt"),
        ];

        let patch = generate_patch(&lines);

        assert!(!patch.contains("canceled"));
        assert!(patch.contains("+actual change"));
    }

    #[test]
    fn test_skips_file_header() {
        let lines = vec![
            make_diff_line(LineSource::FileHeader, "src/test.txt", "test.txt"),
            make_diff_line(LineSource::Base, "context", "test.txt"),
            make_diff_line(LineSource::Committed, "change", "test.txt"),
        ];

        let patch = generate_patch(&lines);

        // FileHeader content should not appear as a change line
        let lines: Vec<&str> = patch.lines().collect();
        assert!(!lines.iter().any(|l| *l == "+src/test.txt" || *l == "-src/test.txt"));
    }

    #[test]
    fn test_empty_diff() {
        let lines: Vec<DiffLine> = vec![];
        let patch = generate_patch(&lines);
        assert!(patch.is_empty());
    }

    #[test]
    fn test_no_changes() {
        let lines = vec![
            make_diff_line(LineSource::Base, "line 1", "test.txt"),
            make_diff_line(LineSource::Base, "line 2", "test.txt"),
        ];

        let patch = generate_patch(&lines);

        // No hunks should be generated for files with no changes
        assert!(!patch.contains("@@"));
    }

    #[test]
    fn test_hunk_header_format_with_counts() {
        let hunk = Hunk {
            old_start: 10,
            old_count: 5,
            new_start: 12,
            new_count: 7,
            lines: vec![],
        };

        assert_eq!(hunk.header(), "@@ -10,5 +12,7 @@");
    }

    #[test]
    fn test_hunk_header_omits_count_when_one() {
        // Per git spec, count is omitted when it equals 1
        let hunk = Hunk {
            old_start: 5,
            old_count: 1,
            new_start: 7,
            new_count: 1,
            lines: vec![],
        };

        assert_eq!(hunk.header(), "@@ -5 +7 @@");
    }

    #[test]
    fn test_hunk_header_mixed_counts() {
        let hunk = Hunk {
            old_start: 10,
            old_count: 1,
            new_start: 12,
            new_count: 3,
            lines: vec![],
        };

        assert_eq!(hunk.header(), "@@ -10 +12,3 @@");
    }

    #[test]
    fn test_hunk_header_zero_counts() {
        // Zero counts for new/deleted files
        let hunk = Hunk {
            old_start: 0,
            old_count: 0,
            new_start: 1,
            new_count: 5,
            lines: vec![],
        };

        assert_eq!(hunk.header(), "@@ -0,0 +1,5 @@");
    }

    #[test]
    fn test_context_limiting() {
        // Create a file with many lines and one change in the middle
        let mut lines = Vec::new();
        for i in 1..=20 {
            lines.push(make_diff_line(
                LineSource::Base,
                &format!("line {}", i),
                "test.txt",
            ));
        }
        // Insert a change at position 10
        lines.insert(
            10,
            make_diff_line(LineSource::Committed, "new line", "test.txt"),
        );

        let patch = generate_patch(&lines);

        // Should have context but not all 20+ lines
        let line_count = patch.lines().count();
        // Header (3) + hunk header (1) + 3 context before + 1 change + 3 context after = 11
        assert!(line_count < 15, "Patch should be limited: {}", line_count);
    }

    #[test]
    fn test_all_change_types_combined() {
        let lines = vec![
            make_diff_line(LineSource::Base, "context 1", "test.txt"),
            make_diff_line(LineSource::DeletedBase, "deleted base", "test.txt"),
            make_diff_line(LineSource::Committed, "committed add", "test.txt"),
            make_diff_line(LineSource::Base, "context 2", "test.txt"),
            make_diff_line(LineSource::DeletedCommitted, "deleted committed", "test.txt"),
            make_diff_line(LineSource::Staged, "staged add", "test.txt"),
            make_diff_line(LineSource::Base, "context 3", "test.txt"),
            make_diff_line(LineSource::DeletedStaged, "deleted staged", "test.txt"),
            make_diff_line(LineSource::Unstaged, "unstaged add", "test.txt"),
        ];

        let patch = generate_patch(&lines);

        assert!(patch.contains("-deleted base"));
        assert!(patch.contains("+committed add"));
        assert!(patch.contains("-deleted committed"));
        assert!(patch.contains("+staged add"));
        assert!(patch.contains("-deleted staged"));
        assert!(patch.contains("+unstaged add"));
    }

    #[test]
    fn test_new_file_uses_dev_null() {
        // A new file has only additions (no context, no deletions)
        let lines = vec![
            make_diff_line(LineSource::Committed, "line 1", "new_file.txt"),
            make_diff_line(LineSource::Committed, "line 2", "new_file.txt"),
            make_diff_line(LineSource::Committed, "line 3", "new_file.txt"),
        ];

        let patch = generate_patch(&lines);

        assert!(patch.contains("new file mode 100644"));
        assert!(patch.contains("--- /dev/null"));
        assert!(patch.contains("+++ b/new_file.txt"));
        assert!(patch.contains("@@ -0,0 +1,3 @@"));
    }

    #[test]
    fn test_deleted_file_uses_dev_null() {
        // A deleted file has only deletions (no context, no additions)
        let lines = vec![
            make_diff_line(LineSource::DeletedCommitted, "line 1", "deleted.txt"),
            make_diff_line(LineSource::DeletedCommitted, "line 2", "deleted.txt"),
        ];

        let patch = generate_patch(&lines);

        assert!(patch.contains("deleted file mode 100644"));
        assert!(patch.contains("--- a/deleted.txt"));
        assert!(patch.contains("+++ /dev/null"));
        assert!(patch.contains("@@ -1,2 +0,0 @@"));
    }

    #[test]
    fn test_line_numbers_are_correct() {
        // Verify exact line numbers in the generated hunk header
        let lines = vec![
            make_diff_line(LineSource::Base, "line 1", "test.txt"),
            make_diff_line(LineSource::Base, "line 2", "test.txt"),
            make_diff_line(LineSource::Base, "line 3", "test.txt"),
            make_diff_line(LineSource::Committed, "inserted", "test.txt"),
            make_diff_line(LineSource::Base, "line 4", "test.txt"),
        ];

        let patch = generate_patch(&lines);

        // Hunk contains:
        // - Old file: 4 lines (line 1, 2, 3, 4 - context around insertion)
        // - New file: 5 lines (line 1, 2, 3, inserted, 4)
        assert!(
            patch.contains("@@ -1,4 +1,5 @@"),
            "Unexpected hunk header in:\n{}",
            patch
        );
    }

    #[test]
    fn test_deletion_line_numbers() {
        let lines = vec![
            make_diff_line(LineSource::Base, "keep 1", "test.txt"),
            make_diff_line(LineSource::Base, "keep 2", "test.txt"),
            make_diff_line(LineSource::DeletedCommitted, "removed", "test.txt"),
            make_diff_line(LineSource::Base, "keep 3", "test.txt"),
            make_diff_line(LineSource::Base, "keep 4", "test.txt"),
        ];

        let patch = generate_patch(&lines);

        // Old file: 5 lines (4 kept + 1 deleted)
        // New file: 4 lines (the kept ones)
        assert!(
            patch.contains("@@ -1,5 +1,4 @@"),
            "Unexpected hunk header in:\n{}",
            patch
        );
    }

    #[test]
    fn test_path_with_spaces() {
        // Git diff format doesn't escape spaces in paths
        let lines = vec![
            make_diff_line(LineSource::Base, "content", "path with spaces/file name.txt"),
            make_diff_line(LineSource::Committed, "new", "path with spaces/file name.txt"),
        ];

        let patch = generate_patch(&lines);

        assert!(patch.contains("diff --git a/path with spaces/file name.txt b/path with spaces/file name.txt"));
        assert!(patch.contains("--- a/path with spaces/file name.txt"));
        assert!(patch.contains("+++ b/path with spaces/file name.txt"));
    }

    #[test]
    fn test_lines_without_file_path_are_skipped() {
        fn make_line_no_path(source: LineSource, content: &str) -> DiffLine {
            DiffLine {
                source,
                content: content.to_string(),
                prefix: ' ',
                line_number: None,
                file_path: None,
                inline_spans: Vec::new(),
                old_content: None,
                change_source: None,
                in_current_bookmark: None,
                block_idx: None,
                move_target: None,
            }
        }

        let lines = vec![
            make_line_no_path(LineSource::Base, "orphan line"),
            make_diff_line(LineSource::Base, "context", "test.txt"),
            make_diff_line(LineSource::Committed, "change", "test.txt"),
        ];

        let patch = generate_patch(&lines);

        // Orphan line should not appear in output
        assert!(!patch.contains("orphan"));
        // But the file with path should be processed
        assert!(patch.contains("+change"));
    }

    #[test]
    fn test_lines_with_empty_file_path_are_skipped() {
        fn make_line_empty_path(source: LineSource, content: &str) -> DiffLine {
            DiffLine {
                source,
                content: content.to_string(),
                prefix: ' ',
                line_number: None,
                file_path: Some(String::new()),
                inline_spans: Vec::new(),
                old_content: None,
                change_source: None,
                in_current_bookmark: None,
                block_idx: None,
                move_target: None,
            }
        }

        let lines = vec![
            make_line_empty_path(LineSource::Committed, "orphan"),
            make_diff_line(LineSource::Committed, "real change", "test.txt"),
        ];

        let patch = generate_patch(&lines);

        assert!(!patch.contains("orphan"));
        assert!(patch.contains("+real change"));
    }

    #[test]
    fn test_format_range_helper() {
        assert_eq!(format_range(1, 1), "1");
        assert_eq!(format_range(5, 1), "5");
        assert_eq!(format_range(1, 3), "1,3");
        assert_eq!(format_range(10, 0), "10,0");
        assert_eq!(format_range(0, 0), "0,0");
    }
}
