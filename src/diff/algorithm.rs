//! 4-way diff algorithm: base→head→index→working.
//!
//! Computes a unified diff showing changes across all four file versions,
//! using provenance maps to track where each line originated.

use super::cancellation::{
    collect_canceled_committed, collect_canceled_simple, collect_canceled_staged,
    insert_canceled_lines,
};
use super::output::{build_working_line_output, determine_deletion_source};
use super::provenance::{build_modification_map, build_provenance_map};
use super::{DiffLine, FileDiff, LineSource};

/// Input for computing a 4-way diff.
///
/// Named fields make call sites readable:
/// ```ignore
/// compute_four_way_diff(DiffInput {
///     path: "file.rs",
///     base: Some(base_content),
///     head: Some(head_content),
///     index: Some(index_content),
///     working: Some(working_content),
///     old_path: None,
/// })
/// ```
#[derive(Debug, Default)]
pub struct DiffInput<'a> {
    /// Path to the file being diffed
    pub path: &'a str,
    /// Content at merge-base (common ancestor with main/master)
    pub base: Option<&'a str>,
    /// Content at HEAD (committed on branch)
    pub head: Option<&'a str>,
    /// Content in index (staged)
    pub index: Option<&'a str>,
    /// Content in working tree
    pub working: Option<&'a str>,
    /// Original path if file was renamed
    pub old_path: Option<&'a str>,
}

fn build_deletion_diff(path: &str, content: &str, source: LineSource) -> FileDiff {
    let mut lines = vec![DiffLine::deleted_file_header(path)];
    for (i, line) in content.lines().enumerate() {
        lines.push(
            DiffLine::new(source, line.to_string(), '-', Some(i + 1)).with_file_path(path),
        );
    }
    FileDiff::new(lines)
}

fn check_file_deletion(input: &DiffInput<'_>) -> Option<FileDiff> {
    // Unstaged deletion: file exists in index but not working tree
    if input.working.is_none()
        && let Some(content) = input.index
    {
        return Some(build_deletion_diff(input.path, content, LineSource::DeletedStaged));
    }

    // Staged deletion: file exists in HEAD but not in index or working
    if input.index.is_none()
        && input.working.is_none()
        && let Some(content) = input.head
    {
        return Some(build_deletion_diff(input.path, content, LineSource::DeletedCommitted));
    }

    // Committed deletion: file exists in base but not in HEAD/index/working
    if input.head.is_none()
        && input.index.is_none()
        && input.working.is_none()
        && let Some(content) = input.base
    {
        return Some(build_deletion_diff(input.path, content, LineSource::DeletedBase));
    }

    None
}

/// Compute 4-way diff: base→head→index→working.
/// Uses provenance maps (not content similarity) to determine line sources.
/// Inline diffs only created from explicit modification maps.
pub fn compute_four_way_diff(input: DiffInput<'_>) -> FileDiff {
    if let Some(deletion_diff) = check_file_deletion(&input) {
        return deletion_diff;
    }

    let path = input.path;
    let header = match input.old_path {
        Some(old) => DiffLine::renamed_file_header(old, path),
        None => DiffLine::file_header(path),
    };
    let mut lines = vec![header];

    let base = input.base.unwrap_or("");
    let head = input.head.unwrap_or(base);
    let index = input.index.unwrap_or(head);
    let working = input.working.unwrap_or(index);

    let base_lines: Vec<&str> = base.lines().collect();
    let head_lines: Vec<&str> = head.lines().collect();
    let index_lines: Vec<&str> = index.lines().collect();
    let working_lines: Vec<&str> = working.lines().collect();

    // If base == working, only show "canceled" lines (added then removed)
    if base == working {
        let head_from_base = build_provenance_map(&base_lines, &head_lines);
        let index_from_head = build_provenance_map(&head_lines, &index_lines);
        let working_from_index = build_provenance_map(&index_lines, &working_lines);

        lines.extend(collect_canceled_simple(
            &head_lines,
            &index_lines,
            &head_from_base,
            &index_from_head,
            &working_from_index,
            path,
        ));

        return FileDiff::new(lines);
    }

    // Build provenance maps: provenance[new_idx] = Some(old_idx) if line came from old
    let head_from_base = build_provenance_map(&base_lines, &head_lines);
    let index_from_head = build_provenance_map(&head_lines, &index_lines);
    let working_from_index = build_provenance_map(&index_lines, &working_lines);

    // Build modification maps for adjacent delete-insert pairs with meaningful similarity
    let base_head_mods = build_modification_map(&base_lines, &head_lines, LineSource::Committed);
    let head_index_mods = build_modification_map(&head_lines, &index_lines, LineSource::Staged);
    let index_working_mods = build_modification_map(&index_lines, &working_lines, LineSource::Unstaged);

    // Build reverse provenance: base_to_working[base_idx] = Some(working_idx) if still present
    let mut base_to_working: Vec<Option<usize>> = vec![None; base_lines.len()];

    for working_idx in 0..working_lines.len() {
        if let Some(index_idx) = working_from_index.get(working_idx).copied().flatten()
            && let Some(head_idx) = index_from_head.get(index_idx).copied().flatten()
            && let Some(base_idx) = head_from_base.get(head_idx).copied().flatten()
        {
            base_to_working[base_idx] = Some(working_idx);
        }
    }

    // Modified base lines should not show as deletions - they're merged into inline diffs
    for (head_idx, (base_idx, _)) in &base_head_mods {
        for working_idx in 0..working_lines.len() {
            if let Some(index_idx) = working_from_index.get(working_idx).copied().flatten()
                && let Some(h_idx) = index_from_head.get(index_idx).copied().flatten()
                && h_idx == *head_idx
            {
                base_to_working[*base_idx] = Some(working_idx);
                break;
            }
        }
    }

    for (index_idx, (head_idx, _)) in &head_index_mods {
        if let Some(base_idx) = head_from_base.get(*head_idx).copied().flatten() {
            for working_idx in 0..working_lines.len() {
                if working_from_index.get(working_idx).copied().flatten() == Some(*index_idx) {
                    base_to_working[base_idx] = Some(working_idx);
                    break;
                }
            }
        }
    }

    for (working_idx, (index_idx, _)) in &index_working_mods {
        if let Some(head_idx) = index_from_head.get(*index_idx).copied().flatten()
            && let Some(base_idx) = head_from_base.get(head_idx).copied().flatten()
        {
            base_to_working[base_idx] = Some(*working_idx);
        }
    }

    let trace_source = |working_idx: usize| -> LineSource {
        if let Some(index_idx) = working_from_index.get(working_idx).copied().flatten() {
            if let Some(head_idx) = index_from_head.get(index_idx).copied().flatten() {
                if head_from_base.get(head_idx).copied().flatten().is_some() {
                    LineSource::Base
                } else {
                    LineSource::Committed
                }
            } else {
                LineSource::Staged
            }
        } else {
            LineSource::Unstaged
        }
    };

    let trace_index_source = |index_idx: usize| -> LineSource {
        if let Some(head_idx) = index_from_head.get(index_idx).copied().flatten() {
            if head_from_base.get(head_idx).copied().flatten().is_some() {
                LineSource::Base
            } else {
                LineSource::Committed
            }
        } else {
            LineSource::Staged
        }
    };

    let trace_head_source = |head_idx: usize| -> LineSource {
        if head_from_base.get(head_idx).copied().flatten().is_some() {
            LineSource::Base
        } else {
            LineSource::Committed
        }
    };

    // Find base position for a working line (via provenance or modification maps)
    let get_working_base_pos = |working_idx: usize| -> Option<usize> {
        if let Some(index_idx) = working_from_index.get(working_idx).copied().flatten()
            && let Some(head_idx) = index_from_head.get(index_idx).copied().flatten()
            && let Some(base_idx) = head_from_base.get(head_idx).copied().flatten()
        {
            return Some(base_idx);
        }

        if let Some((index_idx, _)) = index_working_mods.get(&working_idx)
            && let Some(head_idx) = index_from_head.get(*index_idx).copied().flatten()
            && let Some(base_idx) = head_from_base.get(head_idx).copied().flatten()
        {
            return Some(base_idx);
        }

        None
    };

    // Find head position for a working line (via provenance or modification maps)
    let get_working_head_idx = |working_idx: usize| -> Option<usize> {
        if let Some(index_idx) = working_from_index.get(working_idx).copied().flatten()
            && let Some(head_idx) = index_from_head.get(index_idx).copied().flatten()
        {
            return Some(head_idx);
        }

        if let Some((index_idx, _)) = index_working_mods.get(&working_idx)
            && let Some(head_idx) = index_from_head.get(*index_idx).copied().flatten()
        {
            return Some(head_idx);
        }

        None
    };

    let mut next_base_deletion = 0usize;
    let mut output_head_positions: Vec<Option<usize>> = Vec::new();

    for (line_num, working_idx) in (1usize..).zip(0..working_lines.len()) {
        let working_content = working_lines[working_idx].trim_end();
        let working_base_pos = get_working_base_pos(working_idx);

        // Deletion boundary: output deletions before working lines from later base positions.
        // For insertions, look ahead to find the next base position.
        let deletion_boundary = if let Some(pos) = working_base_pos {
            Some(pos)
        } else {
            let mut next_base = None;
            for future_idx in (working_idx + 1)..working_lines.len() {
                if let Some(pos) = get_working_base_pos(future_idx) {
                    next_base = Some(pos);
                    break;
                }
            }
            next_base
        };

        if let Some(boundary) = deletion_boundary {
            while next_base_deletion < boundary {
                if base_to_working[next_base_deletion].is_none() {
                    let base_content = base_lines[next_base_deletion].trim_end();
                    let delete_source = determine_deletion_source(
                        next_base_deletion,
                        &base_lines,
                        &head_lines,
                        &index_lines,
                        &head_from_base,
                        &index_from_head,
                    );

                    lines.push(DiffLine::new(
                        delete_source,
                        base_content.to_string(),
                        '-',
                        None,
                    ).with_file_path(path));
                    let head_idx_for_deletion = head_from_base.iter()
                        .position(|&h| h == Some(next_base_deletion));
                    output_head_positions.push(head_idx_for_deletion);
                }
                next_base_deletion += 1;
            }
        }

        let source = trace_source(working_idx);
        let working_head_idx = get_working_head_idx(working_idx);
        output_head_positions.push(working_head_idx);
        let output_line = build_working_line_output(
            working_idx,
            working_content,
            source,
            line_num,
            path,
            &working_from_index,
            &index_from_head,
            &head_from_base,
            &index_working_mods,
            &base_head_mods,
            &head_index_mods,
            &index_lines,
            &head_lines,
            &trace_index_source,
            &trace_head_source,
        );

        lines.push(output_line);

        if let Some(base_pos) = working_base_pos {
            next_base_deletion = next_base_deletion.max(base_pos + 1);
        }
    }

    while next_base_deletion < base_lines.len() {
        if base_to_working[next_base_deletion].is_none() {
            let base_content = base_lines[next_base_deletion].trim_end();
            let delete_source = determine_deletion_source(
                next_base_deletion,
                &base_lines,
                &head_lines,
                &index_lines,
                &head_from_base,
                &index_from_head,
            );
            lines.push(DiffLine::new(
                delete_source,
                base_content.to_string(),
                '-',
                None,
            ).with_file_path(path));
            let head_idx_for_deletion = head_from_base.iter()
                .position(|&h| h == Some(next_base_deletion));
            output_head_positions.push(head_idx_for_deletion);
        }
        next_base_deletion += 1;
    }

    // Collect and insert canceled lines (added in commits/staging but removed in working)
    let canceled_committed = collect_canceled_committed(
        &head_lines,
        &head_from_base,
        &index_from_head,
        &working_from_index,
        &head_index_mods,
        &index_working_mods,
    );
    insert_canceled_lines(
        &mut lines,
        canceled_committed,
        LineSource::CanceledCommitted,
        path,
        &mut output_head_positions,
    );

    let canceled_staged = collect_canceled_staged(
        &index_lines,
        &index_from_head,
        &working_from_index,
        &index_working_mods,
    );
    let mut output_index_positions: Vec<Option<usize>> = lines
        .iter()
        .map(|line| index_lines.iter().position(|h| h.trim_end() == line.content))
        .collect();
    insert_canceled_lines(
        &mut lines,
        canceled_staged,
        LineSource::CanceledStaged,
        path,
        &mut output_index_positions,
    );

    FileDiff::new(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Tests for check_file_deletion ===

    #[test]
    fn test_check_file_deletion_unstaged() {
        // File exists in index but not in working tree = unstaged deletion
        let input = DiffInput {
            path: "deleted.rs",
            base: Some("base content"),
            head: Some("head content"),
            index: Some("index content\nline 2"),
            working: None, // Not in working tree
            old_path: None,
        };

        let result = check_file_deletion(&input);
        assert!(result.is_some(), "Should detect unstaged deletion");

        let diff = result.unwrap();
        // First line is header
        assert_eq!(diff.lines[0].source, LineSource::FileHeader);
        assert!(diff.lines[0].content.contains("deleted.rs"));
        assert!(diff.lines[0].content.contains("(deleted)"));

        // Content lines should have DeletedStaged source
        assert_eq!(diff.lines[1].source, LineSource::DeletedStaged);
        assert_eq!(diff.lines[1].content, "index content");
        assert_eq!(diff.lines[1].prefix, '-');
        assert_eq!(diff.lines[1].line_number, Some(1));

        assert_eq!(diff.lines[2].source, LineSource::DeletedStaged);
        assert_eq!(diff.lines[2].content, "line 2");
        assert_eq!(diff.lines[2].line_number, Some(2));
    }

    #[test]
    fn test_check_file_deletion_staged() {
        // File exists in HEAD but not in index or working = staged deletion
        let input = DiffInput {
            path: "staged_delete.rs",
            base: Some("base content"),
            head: Some("head content\nhead line 2"),
            index: None,    // Not in index
            working: None,  // Not in working
            old_path: None,
        };

        let result = check_file_deletion(&input);
        assert!(result.is_some(), "Should detect staged deletion");

        let diff = result.unwrap();
        // Content should come from HEAD with DeletedCommitted source
        assert_eq!(diff.lines[1].source, LineSource::DeletedCommitted);
        assert_eq!(diff.lines[1].content, "head content");
    }

    #[test]
    fn test_check_file_deletion_committed() {
        // File exists in base but not in HEAD/index/working = committed deletion
        let input = DiffInput {
            path: "committed_delete.rs",
            base: Some("base content\nbase line 2\nbase line 3"),
            head: None,
            index: None,
            working: None,
            old_path: None,
        };

        let result = check_file_deletion(&input);
        assert!(result.is_some(), "Should detect committed deletion");

        let diff = result.unwrap();
        // Content should come from base with DeletedBase source
        assert_eq!(diff.lines[1].source, LineSource::DeletedBase);
        assert_eq!(diff.lines[1].content, "base content");
        assert_eq!(diff.lines.len(), 4); // header + 3 content lines
    }

    #[test]
    fn test_check_file_deletion_no_deletion() {
        // File exists in working tree = not a deletion
        let input = DiffInput {
            path: "exists.rs",
            base: Some("base"),
            head: Some("head"),
            index: Some("index"),
            working: Some("working"),
            old_path: None,
        };

        let result = check_file_deletion(&input);
        assert!(result.is_none(), "Should not detect deletion when file exists");
    }

    #[test]
    fn test_check_file_deletion_new_file() {
        // New file - no base/head/index, only working
        let input = DiffInput {
            path: "new.rs",
            base: None,
            head: None,
            index: None,
            working: Some("new content"),
            old_path: None,
        };

        let result = check_file_deletion(&input);
        assert!(result.is_none(), "New file should not be detected as deletion");
    }

    // === Tests for build_deletion_diff ===

    #[test]
    fn test_build_deletion_diff_preserves_content() {
        let content = "line 1\nline 2\nline 3";
        let diff = build_deletion_diff("test.rs", content, LineSource::DeletedBase);

        // Should have header + 3 content lines
        assert_eq!(diff.lines.len(), 4);

        // Verify all content is preserved
        assert_eq!(diff.lines[1].content, "line 1");
        assert_eq!(diff.lines[2].content, "line 2");
        assert_eq!(diff.lines[3].content, "line 3");
    }

    #[test]
    fn test_build_deletion_diff_correct_source() {
        let content = "content";

        // Test each deletion source type
        let diff_base = build_deletion_diff("a.rs", content, LineSource::DeletedBase);
        assert_eq!(diff_base.lines[1].source, LineSource::DeletedBase);

        let diff_committed = build_deletion_diff("b.rs", content, LineSource::DeletedCommitted);
        assert_eq!(diff_committed.lines[1].source, LineSource::DeletedCommitted);

        let diff_staged = build_deletion_diff("c.rs", content, LineSource::DeletedStaged);
        assert_eq!(diff_staged.lines[1].source, LineSource::DeletedStaged);
    }

    #[test]
    fn test_build_deletion_diff_line_numbers() {
        let content = "a\nb\nc\nd\ne";
        let diff = build_deletion_diff("test.rs", content, LineSource::DeletedBase);

        // Line numbers should be 1-indexed
        for (i, line) in diff.lines.iter().skip(1).enumerate() {
            assert_eq!(line.line_number, Some(i + 1));
        }
    }

    #[test]
    fn test_build_deletion_diff_file_path() {
        let diff = build_deletion_diff("path/to/file.rs", "content", LineSource::DeletedBase);

        // All content lines should have file_path set
        for line in diff.lines.iter().skip(1) {
            assert_eq!(line.file_path, Some("path/to/file.rs".to_string()));
        }
    }

    #[test]
    fn test_build_deletion_diff_empty_file() {
        let diff = build_deletion_diff("empty.rs", "", LineSource::DeletedBase);

        // Should only have header, no content lines
        assert_eq!(diff.lines.len(), 1);
        assert_eq!(diff.lines[0].source, LineSource::FileHeader);
    }

    // === Tests for compute_four_way_diff ===

    #[test]
    fn test_four_way_diff_base_equals_working() {
        // When base == working, only canceled lines should appear
        let base = "line1\nline2";
        let head = "line1\ninserted\nline2"; // Added a line
        let index = "line1\nline2"; // Removed it again

        let diff = compute_four_way_diff(DiffInput {
            path: "test.rs",
            base: Some(base),
            head: Some(head),
            index: Some(index),
            working: Some(base), // Same as base
            old_path: None,
        });

        // Should have header + canceled line
        let canceled: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::CanceledCommitted)
            .collect();
        assert_eq!(canceled.len(), 1);
        assert_eq!(canceled[0].content, "inserted");
    }

    #[test]
    fn test_four_way_diff_simple_addition() {
        let base = "line1\nline2";
        let working = "line1\nline2\nline3";

        let diff = compute_four_way_diff(DiffInput {
            path: "test.rs",
            base: Some(base),
            head: Some(base),
            index: Some(base),
            working: Some(working),
            old_path: None,
        });

        // line3 should be marked as Unstaged addition
        let additions: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::Unstaged)
            .collect();
        assert_eq!(additions.len(), 1);
        assert_eq!(additions[0].content, "line3");
        assert_eq!(additions[0].prefix, '+');
    }

    #[test]
    fn test_four_way_diff_simple_deletion() {
        let base = "line1\nline2\nline3";
        let working = "line1\nline3";

        let diff = compute_four_way_diff(DiffInput {
            path: "test.rs",
            base: Some(base),
            head: Some(base),
            index: Some(base),
            working: Some(working),
            old_path: None,
        });

        // line2 should be marked as deleted
        let deletions: Vec<_> = diff.lines.iter()
            .filter(|l| l.source.is_deletion())
            .collect();
        assert_eq!(deletions.len(), 1);
        assert_eq!(deletions[0].content, "line2");
        assert_eq!(deletions[0].prefix, '-');
    }

    #[test]
    fn test_four_way_diff_committed_change() {
        // When a line is modified in a commit and the change is similar enough,
        // the algorithm merges it into an inline diff rather than showing
        // separate delete/add lines.
        //
        // Use strings with long shared suffix to ensure they're well above the
        // is_meaningful threshold (>= 5 unchanged chars) regardless of tuning.
        let base = "old_function_name()";
        let head = "new_function_name()"; // Changed in commit - shares "_function_name()" (16 chars)

        let diff = compute_four_way_diff(DiffInput {
            path: "test.rs",
            base: Some(base),
            head: Some(head),
            index: Some(head),
            working: Some(head),
            old_path: None,
        });

        // The result should show the current content with old_content attached
        // for inline diff rendering
        let modified_line = diff.lines.iter()
            .find(|l| l.content == "new_function_name()")
            .expect("Should have the current content");

        assert_eq!(modified_line.old_content, Some("old_function_name()".to_string()),
            "Should have old content for inline diff");
        assert_eq!(modified_line.change_source, Some(LineSource::Committed),
            "Should indicate change came from commit");
        assert_eq!(modified_line.source, LineSource::Base,
            "Source should be Base since line traces back to base");
        assert_eq!(modified_line.prefix, ' ',
            "Prefix should be space (not deletion marker)");
    }

    #[test]
    fn test_four_way_diff_committed_complete_replacement() {
        // When lines are completely different, they appear as separate
        // delete/add rather than inline diff
        let base = "func foo() { return 42; }";
        let head = "struct Bar { x: i32, y: i32 }"; // Completely different

        let diff = compute_four_way_diff(DiffInput {
            path: "test.rs",
            base: Some(base),
            head: Some(head),
            index: Some(head),
            working: Some(head),
            old_path: None,
        });

        // Should show deletion of old content
        let has_deletion = diff.lines.iter()
            .any(|l| l.source.is_deletion() && l.content == "func foo() { return 42; }");
        assert!(has_deletion, "Should show deletion when lines are too different");

        // Should show the new content
        let has_new = diff.lines.iter()
            .any(|l| l.content == "struct Bar { x: i32, y: i32 }");
        assert!(has_new, "Should show new content");
    }

    #[test]
    fn test_four_way_diff_staged_change() {
        let base = "base";
        let index = "staged"; // Changed in staging

        let diff = compute_four_way_diff(DiffInput {
            path: "test.rs",
            base: Some(base),
            head: Some(base),
            index: Some(index),
            working: Some(index),
            old_path: None,
        });

        // Should have staged addition
        let staged: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::Staged)
            .collect();
        assert!(!staged.is_empty(), "Should have staged content");
    }

    #[test]
    fn test_four_way_diff_empty_files() {
        let diff = compute_four_way_diff(DiffInput {
            path: "empty.rs",
            base: Some(""),
            head: Some(""),
            index: Some(""),
            working: Some(""),
            old_path: None,
        });

        // Should only have header
        assert_eq!(diff.lines.len(), 1);
        assert_eq!(diff.lines[0].source, LineSource::FileHeader);
    }

    #[test]
    fn test_four_way_diff_identical_nonempty_content() {
        let content = "line1\nline2\nline3";
        let diff = compute_four_way_diff(DiffInput {
            path: "unchanged.rs",
            base: Some(content),
            head: Some(content),
            index: Some(content),
            working: Some(content),
            old_path: None,
        });

        assert_eq!(diff.lines.len(), 1);
        assert_eq!(diff.lines[0].source, LineSource::FileHeader);
    }

    #[test]
    fn test_four_way_diff_new_file() {
        let diff = compute_four_way_diff(DiffInput {
            path: "new.rs",
            base: None,
            head: None,
            index: None,
            working: Some("new content"),
            old_path: None,
        });

        // All content should be Unstaged additions
        let content_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source != LineSource::FileHeader)
            .collect();
        assert_eq!(content_lines.len(), 1);
        assert_eq!(content_lines[0].source, LineSource::Unstaged);
        assert_eq!(content_lines[0].content, "new content");
    }

    #[test]
    fn test_four_way_diff_renamed_file() {
        let diff = compute_four_way_diff(DiffInput {
            path: "new_name.rs",
            base: Some("content"),
            head: Some("content"),
            index: Some("content"),
            working: Some("content"),
            old_path: Some("old_name.rs"),
        });

        // Header should indicate rename
        assert_eq!(diff.lines[0].source, LineSource::FileHeader);
        assert!(diff.lines[0].content.contains("old_name.rs"));
        assert!(diff.lines[0].content.contains("new_name.rs"));
    }

    #[test]
    fn test_four_way_diff_multiple_changes() {
        let base = "a\nb\nc\nd\ne";
        let working = "a\nB\nc\nD\ne\nf";

        let diff = compute_four_way_diff(DiffInput {
            path: "test.rs",
            base: Some(base),
            head: Some(base),
            index: Some(base),
            working: Some(working),
            old_path: None,
        });

        // Should have: header + 5 original lines (some modified) + 1 addition + 2 deletions
        // Total content depends on algorithm specifics, but verify key aspects
        let additions: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::Unstaged && l.prefix == '+')
            .collect();
        assert!(!additions.is_empty(), "Should have additions");

        // 'f' should be added
        let has_f = diff.lines.iter().any(|l| l.content == "f");
        assert!(has_f, "Should have 'f' as addition");
    }

    #[test]
    fn test_four_way_diff_preserves_line_numbers() {
        let working = "line1\nline2\nline3";

        let diff = compute_four_way_diff(DiffInput {
            path: "test.rs",
            base: Some(""),
            head: Some(""),
            index: Some(""),
            working: Some(working),
            old_path: None,
        });

        // Line numbers should be sequential starting from 1
        let content_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.line_number.is_some())
            .collect();

        for (i, line) in content_lines.iter().enumerate() {
            assert_eq!(line.line_number, Some(i + 1));
        }
    }

    #[test]
    fn test_four_way_diff_file_path_propagation() {
        let diff = compute_four_way_diff(DiffInput {
            path: "path/to/file.rs",
            base: Some("content"),
            head: Some("content"),
            index: Some("content"),
            working: Some("content"),
            old_path: None,
        });

        // All lines should have file_path set
        for line in &diff.lines {
            assert_eq!(line.file_path, Some("path/to/file.rs".to_string()));
        }
    }

    // === Tests for DiffInput defaults ===

    #[test]
    fn test_diff_input_default() {
        let input = DiffInput::default();
        assert_eq!(input.path, "");
        assert!(input.base.is_none());
        assert!(input.head.is_none());
        assert!(input.index.is_none());
        assert!(input.working.is_none());
        assert!(input.old_path.is_none());
    }

    /// Helper: build a 4-way diff where base→working is the only change
    /// (head and index match working). Returns the FileDiff.
    fn diff_base_to_working(base: &str, working: &str) -> FileDiff {
        compute_four_way_diff(DiffInput {
            path: "test.rs",
            base: Some(base),
            head: Some(working),
            index: Some(working),
            working: Some(working),
            old_path: None,
        })
    }

    /// Helper: extract (prefix, content) pairs, skipping the file header.
    fn line_pairs(diff: &FileDiff) -> Vec<(char, &str)> {
        diff.lines.iter()
            .filter(|l| l.source != LineSource::FileHeader)
            .map(|l| (l.prefix, l.content.as_str()))
            .collect()
    }

    #[test]
    fn test_deleted_function_has_clean_boundary() {
        // Delete fn four between fn three and fn five.
        // The deletion should be exactly: fn four() { ... }
        // NOT: } \n \n fn four() { ... (stealing fn three's closing brace)
        let base = "\
fn three() {\n    println!(\"three\");\n}\n\n\
fn four() {\n    println!(\"four\");\n}\n\n\
fn five() {\n    println!(\"five\");\n}";

        let working = "\
fn three() {\n    println!(\"three\");\n}\n\n\
fn five() {\n    println!(\"five\");\n}";

        let diff = diff_base_to_working(base, working);
        let deletions: Vec<&str> = diff.lines.iter()
            .filter(|l| l.prefix == '-')
            .map(|l| l.content.as_str())
            .collect();

        assert_eq!(deletions[0], "fn four() {",
            "first deleted line should be 'fn four() {{', got: {deletions:?}");
        assert!(deletions.contains(&"}"),
            "deletion should include the closing '}}', got: {deletions:?}");
    }

    #[test]
    fn test_added_function_has_clean_boundary() {
        // Add fn new between fn six and fn seven.
        // The addition should be exactly: fn new() { ... }
        // NOT: } \n \n fn new() { ... (stealing fn six's closing brace)
        let base = "\
fn six() {\n    println!(\"six\");\n}\n\n\
fn seven() {\n    println!(\"seven\");\n}";

        let working = "\
fn six() {\n    println!(\"six\");\n}\n\n\
fn new_func() {\n    println!(\"new\");\n}\n\n\
fn seven() {\n    println!(\"seven\");\n}";

        let diff = diff_base_to_working(base, working);
        let additions: Vec<&str> = diff.lines.iter()
            .filter(|l| l.prefix == '+')
            .map(|l| l.content.as_str())
            .collect();

        assert_eq!(additions[0], "fn new_func() {",
            "first added line should be 'fn new_func() {{', got: {additions:?}");
        assert!(additions.contains(&"}"),
            "addition should include the closing '}}', got: {additions:?}");
    }

    #[test]
    fn test_multiple_deleted_functions_each_have_clean_boundaries() {
        // Delete fn two AND fn four from a file with five functions.
        // Each deletion should be self-contained — no leaking braces.
        let base = "\
fn one() {\n    println!(\"one\");\n}\n\n\
fn two() {\n    println!(\"two\");\n}\n\n\
fn three() {\n    println!(\"three\");\n}\n\n\
fn four() {\n    println!(\"four\");\n    println!(\"more\");\n}\n\n\
fn five() {\n    println!(\"five\");\n}";

        let working = "\
fn one() {\n    println!(\"one\");\n}\n\n\
fn three() {\n    println!(\"three\");\n}\n\n\
fn five() {\n    println!(\"five\");\n}";

        let diff = diff_base_to_working(base, working);
        let pairs = line_pairs(&diff);

        // Find all deletion runs (contiguous '-' lines)
        let mut deletion_runs: Vec<Vec<&str>> = Vec::new();
        let mut current_run: Vec<&str> = Vec::new();
        for (prefix, content) in &pairs {
            if *prefix == '-' {
                current_run.push(content);
            } else if !current_run.is_empty() {
                deletion_runs.push(current_run.clone());
                current_run.clear();
            }
        }
        if !current_run.is_empty() {
            deletion_runs.push(current_run);
        }

        assert_eq!(deletion_runs.len(), 2,
            "should have 2 deletion runs, got {}: {deletion_runs:?}", deletion_runs.len());

        // First deletion run should start with fn two
        assert_eq!(deletion_runs[0][0], "fn two() {",
            "first deletion should start with 'fn two() {{', got: {:?}", deletion_runs[0]);

        // Second deletion run should start with fn four
        assert_eq!(deletion_runs[1][0], "fn four() {",
            "second deletion should start with 'fn four() {{', got: {:?}", deletion_runs[1]);
    }

    #[test]
    fn test_deletion_with_adjacent_addition_has_clean_boundary() {
        // Delete fn two, add fn new in a different spot.
        // The deletion of fn two should still have a clean boundary.
        let base = "\
fn one() {\n    println!(\"one\");\n}\n\n\
fn two() {\n    println!(\"two\");\n}\n\n\
fn three() {\n    println!(\"three\");\n}";

        let working = "\
fn one() {\n    println!(\"one\");\n}\n\n\
fn three() {\n    println!(\"three\");\n}\n\n\
fn brand_new() {\n    println!(\"new\");\n}";

        let diff = diff_base_to_working(base, working);
        let deletions: Vec<&str> = diff.lines.iter()
            .filter(|l| l.prefix == '-')
            .map(|l| l.content.as_str())
            .collect();
        let additions: Vec<&str> = diff.lines.iter()
            .filter(|l| l.prefix == '+')
            .map(|l| l.content.as_str())
            .collect();

        assert_eq!(deletions[0], "fn two() {",
            "deletion should start with 'fn two() {{', got: {deletions:?}");
        // The blank line before fn brand_new is genuinely new content,
        // so it's acceptable as the first addition line.
        let first_nonblank_add = additions.iter()
            .find(|l| !l.trim().is_empty())
            .expect("should have non-blank additions");
        assert_eq!(*first_nonblank_add, "fn brand_new() {",
            "first non-blank addition should be 'fn brand_new() {{', got: {additions:?}");
    }
}
