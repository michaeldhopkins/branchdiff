use std::collections::HashMap;

use super::{DiffLine, LineSource};

/// Determine where a base line was deleted (in commit, staging, or working)
pub(super) fn determine_deletion_source(
    base_idx: usize,
    _base_lines: &[&str],
    _head_lines: &[&str],
    _index_lines: &[&str],
    head_from_base: &[Option<usize>],
    index_from_head: &[Option<usize>],
) -> LineSource {
    // Check if base line still exists in head (by provenance, not content)
    // A base line exists in head if some head line traces back to this base line
    let in_head = head_from_base.contains(&Some(base_idx));

    if !in_head {
        return LineSource::DeletedBase;  // Deleted in commit
    }

    // Find which head line came from this base line
    let head_idx = head_from_base.iter().position(|&opt| opt == Some(base_idx));
    if let Some(head_idx) = head_idx {
        // Check if this head line still exists in index
        let in_index = index_from_head.contains(&Some(head_idx));

        if !in_index {
            return LineSource::DeletedCommitted;  // Deleted in staging
        }
    }

    LineSource::DeletedStaged  // Deleted in working tree
}

/// Build the output line for a working line, handling modifications
pub(super) fn build_working_line_output<F1, F2>(
    working_idx: usize,
    working_content: &str,
    source: LineSource,
    line_num: usize,
    path: &str,
    working_from_index: &[Option<usize>],
    index_from_head: &[Option<usize>],
    _head_from_base: &[Option<usize>],
    index_working_mods: &HashMap<usize, (usize, &str)>,
    base_head_mods: &HashMap<usize, (usize, &str)>,
    head_index_mods: &HashMap<usize, (usize, &str)>,
    _index_lines: &[&str],
    _head_lines: &[&str],
    trace_index_source: &F1,
    trace_head_source: &F2,
) -> DiffLine
where
    F1: Fn(usize) -> LineSource,
    F2: Fn(usize) -> LineSource,
{
    let content = working_content.to_string();

    // Default output: simple line with source
    let default_line = || {
        let prefix = if source == LineSource::Base { ' ' } else { '+' };
        DiffLine::new(source, content.clone(), prefix, Some(line_num)).with_file_path(path)
    };

    match source {
        LineSource::Unstaged => {
            if let Some((index_idx, old_content)) = index_working_mods.get(&working_idx) {
                let original_source = trace_index_source(*index_idx);
                return DiffLine::new(original_source, content, ' ', Some(line_num))
                    .with_file_path(path)
                    .with_old_content(old_content)
                    .with_change_source(LineSource::Unstaged);
            }
            default_line()
        }

        LineSource::Committed => {
            if let Some(index_idx) = working_from_index.get(working_idx).copied().flatten()
                && let Some(head_idx) = index_from_head.get(index_idx).copied().flatten()
                && let Some((_base_idx, old_content)) = base_head_mods.get(&head_idx)
            {
                return DiffLine::new(LineSource::Base, content, ' ', Some(line_num))
                    .with_file_path(path)
                    .with_old_content(old_content)
                    .with_change_source(LineSource::Committed);
            }
            default_line()
        }

        LineSource::Staged => {
            if let Some(index_idx) = working_from_index.get(working_idx).copied().flatten()
                && let Some((_head_idx, old_content)) = head_index_mods.get(&index_idx)
            {
                let original_source = if let Some(head_idx) = index_from_head.get(index_idx).copied().flatten() {
                    trace_head_source(head_idx)
                } else {
                    LineSource::Base
                };

                return DiffLine::new(original_source, content, ' ', Some(line_num))
                    .with_file_path(path)
                    .with_old_content(old_content)
                    .with_change_source(LineSource::Staged);
            }
            default_line()
        }

        LineSource::Base => default_line(),

        _ => default_line(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Tests for determine_deletion_source ===

    #[test]
    fn test_deletion_source_deleted_in_commit() {
        // Base line doesn't exist in head (deleted during commit)
        let base_lines = &["line0", "line1", "line2"];
        let head_lines = &["line0", "line2"]; // line1 deleted
        let index_lines = &["line0", "line2"];

        // head_from_base: head[0]=base[0], head[1]=base[2] (line1 not in head)
        let head_from_base = vec![Some(0), Some(2)];
        let index_from_head = vec![Some(0), Some(1)];

        let source = determine_deletion_source(
            1, // base_idx for "line1"
            base_lines,
            head_lines,
            index_lines,
            &head_from_base,
            &index_from_head,
        );

        assert_eq!(source, LineSource::DeletedBase);
    }

    #[test]
    fn test_deletion_source_deleted_in_staging() {
        // Base line exists in head but not in index (deleted during staging)
        let base_lines = &["line0", "line1", "line2"];
        let head_lines = &["line0", "line1", "line2"];
        let index_lines = &["line0", "line2"]; // line1 deleted from index

        // head_from_base: all lines map 1:1
        let head_from_base = vec![Some(0), Some(1), Some(2)];
        // index_from_head: index[0]=head[0], index[1]=head[2] (head[1] not in index)
        let index_from_head = vec![Some(0), Some(2)];

        let source = determine_deletion_source(
            1, // base_idx for "line1"
            base_lines,
            head_lines,
            index_lines,
            &head_from_base,
            &index_from_head,
        );

        assert_eq!(source, LineSource::DeletedCommitted);
    }

    #[test]
    fn test_deletion_source_deleted_in_working() {
        // Base line exists in head and index but not in working (deleted in working tree)
        let base_lines = &["line0", "line1", "line2"];
        let head_lines = &["line0", "line1", "line2"];
        let index_lines = &["line0", "line1", "line2"];

        // All lines map 1:1 through head and index
        let head_from_base = vec![Some(0), Some(1), Some(2)];
        let index_from_head = vec![Some(0), Some(1), Some(2)];

        let source = determine_deletion_source(
            1, // base_idx for "line1" - exists in head and index
            base_lines,
            head_lines,
            index_lines,
            &head_from_base,
            &index_from_head,
        );

        assert_eq!(source, LineSource::DeletedStaged);
    }

    #[test]
    fn test_deletion_source_first_line() {
        // Test deletion of first line
        let base_lines = &["first", "second"];
        let head_lines = &["second"]; // first deleted

        let head_from_base = vec![Some(1)]; // only "second" remains
        let index_from_head = vec![Some(0)];

        let source = determine_deletion_source(
            0, // first line deleted
            base_lines,
            head_lines,
            &["second"],
            &head_from_base,
            &index_from_head,
        );

        assert_eq!(source, LineSource::DeletedBase);
    }

    #[test]
    fn test_deletion_source_last_line() {
        // Test deletion of last line
        let base_lines = &["first", "last"];
        let head_lines = &["first"]; // last deleted

        let head_from_base = vec![Some(0)]; // only "first" remains
        let index_from_head = vec![Some(0)];

        let source = determine_deletion_source(
            1, // last line deleted
            base_lines,
            head_lines,
            &["first"],
            &head_from_base,
            &index_from_head,
        );

        assert_eq!(source, LineSource::DeletedBase);
    }

    // === Tests for build_working_line_output ===

    #[test]
    fn test_build_output_base_line() {
        let working_from_index = vec![Some(0)];
        let index_from_head = vec![Some(0)];
        let head_from_base = vec![Some(0)];
        let index_working_mods = HashMap::new();
        let base_head_mods = HashMap::new();
        let head_index_mods = HashMap::new();

        let line = build_working_line_output(
            0,
            "content",
            LineSource::Base,
            1,
            "test.rs",
            &working_from_index,
            &index_from_head,
            &head_from_base,
            &index_working_mods,
            &base_head_mods,
            &head_index_mods,
            &["content"],
            &["content"],
            &|_| LineSource::Base,
            &|_| LineSource::Base,
        );

        assert_eq!(line.source, LineSource::Base);
        assert_eq!(line.content, "content");
        assert_eq!(line.prefix, ' ');
        assert_eq!(line.line_number, Some(1));
        assert_eq!(line.file_path, Some("test.rs".to_string()));
    }

    #[test]
    fn test_build_output_unstaged_addition() {
        let working_from_index: Vec<Option<usize>> = vec![None]; // not from index
        let index_from_head = vec![];
        let head_from_base = vec![];
        let index_working_mods = HashMap::new();
        let base_head_mods = HashMap::new();
        let head_index_mods = HashMap::new();

        let line = build_working_line_output(
            0,
            "new line",
            LineSource::Unstaged,
            5,
            "test.rs",
            &working_from_index,
            &index_from_head,
            &head_from_base,
            &index_working_mods,
            &base_head_mods,
            &head_index_mods,
            &[],
            &[],
            &|_| LineSource::Base,
            &|_| LineSource::Base,
        );

        assert_eq!(line.source, LineSource::Unstaged);
        assert_eq!(line.content, "new line");
        assert_eq!(line.prefix, '+');
        assert_eq!(line.line_number, Some(5));
    }

    #[test]
    fn test_build_output_unstaged_modification() {
        let working_from_index = vec![Some(0)];
        let index_from_head = vec![Some(0)];
        let head_from_base = vec![Some(0)];

        // Working line 0 is a modification of index line 0
        let mut index_working_mods = HashMap::new();
        index_working_mods.insert(0usize, (0usize, "old content"));

        let base_head_mods = HashMap::new();
        let head_index_mods = HashMap::new();

        let line = build_working_line_output(
            0,
            "new content",
            LineSource::Unstaged,
            1,
            "test.rs",
            &working_from_index,
            &index_from_head,
            &head_from_base,
            &index_working_mods,
            &base_head_mods,
            &head_index_mods,
            &["old content"],
            &["old content"],
            &|_| LineSource::Base,
            &|_| LineSource::Base,
        );

        // Modification should have Base source with old_content and change_source
        assert_eq!(line.source, LineSource::Base);
        assert_eq!(line.content, "new content");
        assert_eq!(line.prefix, ' ');
        assert_eq!(line.old_content, Some("old content".to_string()));
        assert_eq!(line.change_source, Some(LineSource::Unstaged));
    }

    #[test]
    fn test_build_output_committed_addition() {
        let working_from_index = vec![Some(0)];
        let index_from_head = vec![Some(0)];
        let head_from_base: Vec<Option<usize>> = vec![None]; // not from base

        let index_working_mods = HashMap::new();
        let base_head_mods = HashMap::new();
        let head_index_mods = HashMap::new();

        let line = build_working_line_output(
            0,
            "committed line",
            LineSource::Committed,
            1,
            "test.rs",
            &working_from_index,
            &index_from_head,
            &head_from_base,
            &index_working_mods,
            &base_head_mods,
            &head_index_mods,
            &["committed line"],
            &["committed line"],
            &|_| LineSource::Committed,
            &|_| LineSource::Committed,
        );

        assert_eq!(line.source, LineSource::Committed);
        assert_eq!(line.content, "committed line");
        assert_eq!(line.prefix, '+');
    }

    #[test]
    fn test_build_output_committed_modification() {
        let working_from_index = vec![Some(0)];
        let index_from_head = vec![Some(0)];
        let head_from_base = vec![Some(0)];

        let index_working_mods = HashMap::new();
        // Head line 0 is a modification of base line 0
        let mut base_head_mods = HashMap::new();
        base_head_mods.insert(0usize, (0usize, "base content"));
        let head_index_mods = HashMap::new();

        let line = build_working_line_output(
            0,
            "modified in commit",
            LineSource::Committed,
            1,
            "test.rs",
            &working_from_index,
            &index_from_head,
            &head_from_base,
            &index_working_mods,
            &base_head_mods,
            &head_index_mods,
            &["modified in commit"],
            &["modified in commit"],
            &|_| LineSource::Base,
            &|_| LineSource::Base,
        );

        assert_eq!(line.source, LineSource::Base);
        assert_eq!(line.old_content, Some("base content".to_string()));
        assert_eq!(line.change_source, Some(LineSource::Committed));
    }

    #[test]
    fn test_build_output_staged_addition() {
        let working_from_index = vec![Some(0)];
        let index_from_head: Vec<Option<usize>> = vec![None]; // not from head
        let head_from_base = vec![];

        let index_working_mods = HashMap::new();
        let base_head_mods = HashMap::new();
        let head_index_mods = HashMap::new();

        let line = build_working_line_output(
            0,
            "staged line",
            LineSource::Staged,
            1,
            "test.rs",
            &working_from_index,
            &index_from_head,
            &head_from_base,
            &index_working_mods,
            &base_head_mods,
            &head_index_mods,
            &["staged line"],
            &[],
            &|_| LineSource::Staged,
            &|_| LineSource::Base,
        );

        assert_eq!(line.source, LineSource::Staged);
        assert_eq!(line.prefix, '+');
    }

    #[test]
    fn test_build_output_staged_modification() {
        let working_from_index = vec![Some(0)];
        let index_from_head = vec![Some(0)];
        let head_from_base = vec![Some(0)];

        let index_working_mods = HashMap::new();
        let base_head_mods = HashMap::new();
        // Index line 0 is a modification of head line 0
        let mut head_index_mods = HashMap::new();
        head_index_mods.insert(0usize, (0usize, "head content"));

        let line = build_working_line_output(
            0,
            "modified in staging",
            LineSource::Staged,
            1,
            "test.rs",
            &working_from_index,
            &index_from_head,
            &head_from_base,
            &index_working_mods,
            &base_head_mods,
            &head_index_mods,
            &["modified in staging"],
            &["head content"],
            &|_| LineSource::Committed,
            &|idx| if idx == 0 { LineSource::Base } else { LineSource::Committed },
        );

        assert_eq!(line.source, LineSource::Base);
        assert_eq!(line.old_content, Some("head content".to_string()));
        assert_eq!(line.change_source, Some(LineSource::Staged));
    }
}
