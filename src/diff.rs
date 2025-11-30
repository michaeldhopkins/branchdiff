use similar::{ChangeTag, TextDiff};

/// The source/provenance of a line
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineSource {
    /// Unchanged from merge-base (context line)
    Base,
    /// Added/changed in commits on feature branch
    Committed,
    /// Staged in index, ready to commit
    Staged,
    /// Unstaged working tree changes
    Unstaged,
    /// Deleted from base (was in merge-base, now gone)
    DeletedBase,
    /// Deleted from committed (was in HEAD, deleted in staged/working)
    DeletedCommitted,
    /// Deleted from staged (was staged, deleted in working tree)
    DeletedStaged,
    /// File header line
    FileHeader,
}

/// A single line in the diff output
#[derive(Debug, Clone)]
pub struct DiffLine {
    pub source: LineSource,
    pub content: String,
    pub prefix: char,
    /// Line number in the current file (if applicable)
    pub line_number: Option<usize>,
}

impl DiffLine {
    pub fn new(source: LineSource, content: String, prefix: char, line_number: Option<usize>) -> Self {
        Self {
            source,
            content,
            prefix,
            line_number,
        }
    }

    pub fn file_header(path: &str) -> Self {
        Self {
            source: LineSource::FileHeader,
            content: path.to_string(),
            prefix: ' ',
            line_number: None,
        }
    }
}

/// Result of diffing a single file across all 4 states
#[derive(Debug)]
pub struct FileDiff {
    pub path: String,
    pub lines: Vec<DiffLine>,
    pub is_binary: bool,
    pub is_new: bool,
    pub is_deleted: bool,
}

/// Compute the 4-way diff for a single file
/// States: base (merge-base) -> head (committed) -> index (staged) -> working
pub fn compute_file_diff(
    path: &str,
    base_content: Option<&str>,
    head_content: Option<&str>,
    index_content: Option<&str>,
    working_content: Option<&str>,
) -> FileDiff {
    let mut lines = Vec::new();

    // Add file header
    lines.push(DiffLine::file_header(path));

    // Determine file state
    let is_new = base_content.is_none() && (head_content.is_some() || index_content.is_some() || working_content.is_some());
    let is_deleted = working_content.is_none() && index_content.is_none() && (base_content.is_some() || head_content.is_some());

    // Get the "current" content (what we're showing as the main view)
    // Priority: working > index > head > base
    let current_content = working_content
        .or(index_content)
        .or(head_content)
        .or(base_content);

    if current_content.is_none() && base_content.is_none() {
        // Nothing to show
        return FileDiff {
            path: path.to_string(),
            lines,
            is_binary: false,
            is_new,
            is_deleted,
        };
    }

    // For deleted files, show deleted content
    if is_deleted {
        let deleted_content = head_content.or(base_content).unwrap_or("");
        for (i, line) in deleted_content.lines().enumerate() {
            let source = if head_content.is_some() && base_content.map(|b| b.contains(line)).unwrap_or(false) {
                LineSource::DeletedBase
            } else if head_content.is_some() {
                LineSource::DeletedCommitted
            } else {
                LineSource::DeletedBase
            };
            lines.push(DiffLine::new(source, line.to_string(), '-', Some(i + 1)));
        }
        return FileDiff {
            path: path.to_string(),
            lines,
            is_binary: false,
            is_new,
            is_deleted,
        };
    }

    // Build unified view by tracking line provenance
    // We diff sequentially: base→head, head→index, index→working
    // and merge the results

    let base = base_content.unwrap_or("");
    let head = head_content.unwrap_or(base);
    let index = index_content.unwrap_or(head);
    let working = working_content.unwrap_or(index);

    // Track line provenance for the final output
    let diff_lines = compute_unified_diff(base, head, index, working);
    lines.extend(diff_lines);

    FileDiff {
        path: path.to_string(),
        lines,
        is_binary: false,
        is_new,
        is_deleted,
    }
}

/// Compute unified diff showing all 4 states
fn compute_unified_diff(base: &str, head: &str, index: &str, working: &str) -> Vec<DiffLine> {
    let mut result = Vec::new();
    let mut line_num = 1usize;

    // If everything is the same, no diff needed
    if base == head && head == index && index == working {
        return result;
    }

    // Strategy: Show the working tree content as the "current" state,
    // and color lines based on where they came from

    // Create sets of lines at each stage for quick lookup
    let base_lines: std::collections::HashSet<&str> = base.lines().collect();
    let head_lines: std::collections::HashSet<&str> = head.lines().collect();
    let index_lines: std::collections::HashSet<&str> = index.lines().collect();

    // First, show any deletions
    // Lines in base but not in working
    let working_lines: std::collections::HashSet<&str> = working.lines().collect();

    for line in base.lines() {
        if !working_lines.contains(line) && !head_lines.contains(line) {
            // Deleted from base and never made it to head
            result.push(DiffLine::new(LineSource::DeletedBase, line.to_string(), '-', None));
        }
    }

    for line in head.lines() {
        if !working_lines.contains(line) && !base_lines.contains(line) && !index_lines.contains(line) {
            // Was added in head but then removed
            result.push(DiffLine::new(LineSource::DeletedCommitted, line.to_string(), '-', None));
        }
    }

    for line in index.lines() {
        if !working_lines.contains(line) && !head_lines.contains(line) {
            // Was staged but then removed from working
            result.push(DiffLine::new(LineSource::DeletedStaged, line.to_string(), '-', None));
        }
    }

    // Now show the working tree content with colors
    for line in working.lines() {
        let source = determine_line_source(line, &base_lines, &head_lines, &index_lines);
        let prefix = if source == LineSource::Base { ' ' } else { '+' };
        result.push(DiffLine::new(source, line.to_string(), prefix, Some(line_num)));
        line_num += 1;
    }

    result
}

/// Determine where a line came from
fn determine_line_source(
    line: &str,
    base_lines: &std::collections::HashSet<&str>,
    head_lines: &std::collections::HashSet<&str>,
    index_lines: &std::collections::HashSet<&str>,
) -> LineSource {
    let in_base = base_lines.contains(line);
    let in_head = head_lines.contains(line);
    let in_index = index_lines.contains(line);

    match (in_base, in_head, in_index) {
        // Line exists in base - it's context
        (true, _, _) => LineSource::Base,
        // Line was added in commits (in head but not base)
        (false, true, _) => LineSource::Committed,
        // Line was staged (in index but not head or base)
        (false, false, true) => LineSource::Staged,
        // Line is only in working tree
        (false, false, false) => LineSource::Unstaged,
    }
}

/// Alternative: Use actual diff algorithm for more accurate change tracking
pub fn compute_file_diff_v2(
    path: &str,
    base_content: Option<&str>,
    head_content: Option<&str>,
    index_content: Option<&str>,
    working_content: Option<&str>,
) -> FileDiff {
    let mut lines = Vec::new();
    lines.push(DiffLine::file_header(path));

    let base = base_content.unwrap_or("");
    let head = head_content.unwrap_or(base);
    let index = index_content.unwrap_or(head);
    let working = working_content.unwrap_or(index);

    let is_new = base_content.is_none() && head_content.is_none();
    let is_deleted = working_content.is_none() && index_content.is_none();

    if is_deleted {
        // Show deleted content
        let to_delete = head_content.or(base_content).unwrap_or("");
        for (i, line) in to_delete.lines().enumerate() {
            lines.push(DiffLine::new(
                LineSource::DeletedBase,
                line.to_string(),
                '-',
                Some(i + 1),
            ));
        }
        return FileDiff {
            path: path.to_string(),
            lines,
            is_binary: false,
            is_new,
            is_deleted,
        };
    }

    // Use similar crate for proper diff
    // We'll do a 3-stage diff and merge the results

    // Diff 1: base -> head (committed changes)
    let diff_base_head = TextDiff::from_lines(base, head);

    // Diff 2: head -> index (staged changes)
    let diff_head_index = TextDiff::from_lines(head, index);

    // Diff 3: index -> working (unstaged changes)
    let diff_index_working = TextDiff::from_lines(index, working);

    // Build a map of line -> source
    let mut line_sources: std::collections::HashMap<String, LineSource> = std::collections::HashMap::new();

    // Mark committed additions
    for change in diff_base_head.iter_all_changes() {
        if change.tag() == ChangeTag::Insert {
            line_sources.insert(change.value().trim_end().to_string(), LineSource::Committed);
        }
    }

    // Mark staged additions (overrides committed if same line)
    for change in diff_head_index.iter_all_changes() {
        if change.tag() == ChangeTag::Insert {
            let line = change.value().trim_end().to_string();
            if !line_sources.contains_key(&line) || line_sources.get(&line) == Some(&LineSource::Committed) {
                line_sources.insert(line, LineSource::Staged);
            }
        }
    }

    // Mark unstaged additions (overrides all)
    for change in diff_index_working.iter_all_changes() {
        if change.tag() == ChangeTag::Insert {
            line_sources.insert(change.value().trim_end().to_string(), LineSource::Unstaged);
        }
    }

    // Show deletions first
    for change in diff_base_head.iter_all_changes() {
        if change.tag() == ChangeTag::Delete {
            let line_content = change.value().trim_end().to_string();
            // Only show if it's truly deleted (not in working)
            if !working.lines().any(|l| l == line_content) {
                lines.push(DiffLine::new(LineSource::DeletedBase, line_content, '-', None));
            }
        }
    }

    for change in diff_head_index.iter_all_changes() {
        if change.tag() == ChangeTag::Delete {
            let line_content = change.value().trim_end().to_string();
            if !working.lines().any(|l| l == line_content) && !base.lines().any(|l| l == line_content) {
                lines.push(DiffLine::new(LineSource::DeletedCommitted, line_content, '-', None));
            }
        }
    }

    for change in diff_index_working.iter_all_changes() {
        if change.tag() == ChangeTag::Delete {
            let line_content = change.value().trim_end().to_string();
            if !head.lines().any(|l| l == line_content) && !base.lines().any(|l| l == line_content) {
                lines.push(DiffLine::new(LineSource::DeletedStaged, line_content, '-', None));
            }
        }
    }

    // Now output the working tree with proper colors
    let mut line_num = 1;
    for line in working.lines() {
        let trimmed = line.to_string();
        let source = line_sources.get(&trimmed).copied().unwrap_or(LineSource::Base);
        let prefix = if source == LineSource::Base { ' ' } else { '+' };
        lines.push(DiffLine::new(source, trimmed, prefix, Some(line_num)));
        line_num += 1;
    }

    FileDiff {
        path: path.to_string(),
        lines,
        is_binary: false,
        is_new,
        is_deleted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_changes() {
        let content = "line1\nline2\nline3";
        let diff = compute_file_diff("test.txt", Some(content), Some(content), Some(content), Some(content));

        // Should only have file header, no changes
        assert_eq!(diff.lines.len(), 1);
        assert_eq!(diff.lines[0].source, LineSource::FileHeader);
    }

    #[test]
    fn test_committed_addition() {
        let base = "line1\nline2";
        let head = "line1\nline2\nline3";

        let diff = compute_file_diff("test.txt", Some(base), Some(head), Some(head), Some(head));

        // Find the committed line
        let committed_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::Committed)
            .collect();

        assert!(!committed_lines.is_empty());
        assert!(committed_lines.iter().any(|l| l.content == "line3"));
    }

    #[test]
    fn test_unstaged_addition() {
        let content = "line1\nline2";
        let working = "line1\nline2\nline3";

        let diff = compute_file_diff("test.txt", Some(content), Some(content), Some(content), Some(working));

        let unstaged_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::Unstaged)
            .collect();

        assert!(!unstaged_lines.is_empty());
        assert!(unstaged_lines.iter().any(|l| l.content == "line3"));
    }

    #[test]
    fn test_staged_addition() {
        let base = "line1\nline2";
        let index = "line1\nline2\nline3";

        let diff = compute_file_diff("test.txt", Some(base), Some(base), Some(index), Some(index));

        let staged_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::Staged)
            .collect();

        assert!(!staged_lines.is_empty());
        assert!(staged_lines.iter().any(|l| l.content == "line3"));
    }

    #[test]
    fn test_new_file() {
        let working = "line1\nline2";

        let diff = compute_file_diff("test.txt", None, None, None, Some(working));

        assert!(diff.is_new);

        // All lines should be unstaged
        let unstaged_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::Unstaged)
            .collect();

        assert_eq!(unstaged_lines.len(), 2);
    }

    #[test]
    fn test_deleted_file() {
        let base = "line1\nline2";

        let diff = compute_file_diff("test.txt", Some(base), Some(base), None, None);

        assert!(diff.is_deleted);

        // Should show deleted lines
        let deleted_lines: Vec<_> = diff.lines.iter()
            .filter(|l| matches!(l.source, LineSource::DeletedBase | LineSource::DeletedCommitted))
            .collect();

        assert!(!deleted_lines.is_empty());
    }
}
