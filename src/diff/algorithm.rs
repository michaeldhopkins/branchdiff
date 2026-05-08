//! 4-way diff algorithm: base→head→index→working.
//!
//! Computes a unified diff showing changes across all four file versions,
//! using provenance maps to track where each line originated.

use std::sync::atomic::{AtomicBool, Ordering};

use super::cancellation::{
    collect_canceled_committed, collect_canceled_simple, collect_canceled_staged,
    insert_canceled_lines,
};
use super::output::{build_working_line_output, determine_deletion_source};
use super::provenance::{build_modification_map, build_provenance_map};
use super::{DiffLine, FileDiff, LineSource};

/// Check the cancel flag every this many iterations of the main output loop.
/// Power of two so the check is a cheap bitwise mask.
const CANCEL_CHECK_INTERVAL: usize = 1024;

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
    static NEVER: AtomicBool = AtomicBool::new(false);
    compute_four_way_diff_cancellable(input, &NEVER)
}

/// Like `compute_four_way_diff` but bails to a header-only stub when `cancel`
/// is observed set. Thin wrapper over `compute_four_way_diff_with_predicate`
/// for the production hot path; tests use the predicate form directly so they
/// can deterministically fire cancel after a known number of checkpoints.
pub fn compute_four_way_diff_cancellable(
    input: DiffInput<'_>,
    cancel: &AtomicBool,
) -> FileDiff {
    compute_four_way_diff_with_predicate(input, &|| cancel.load(Ordering::Relaxed))
}

/// Predicate-based cancellable diff. The predicate is polled at the boundaries
/// between the heavy phases (line collection, provenance + modification map
/// builds, the main output loop) and inside loops that are O(N) or worse, so
/// a stuck refresh on a 73 MB file can be aborted promptly. Returns a
/// header-only stub when the predicate trips.
pub fn compute_four_way_diff_with_predicate(
    input: DiffInput<'_>,
    is_cancelled: &dyn Fn() -> bool,
) -> FileDiff {
    if let Some(deletion_diff) = check_file_deletion(&input) {
        return deletion_diff;
    }

    let path = input.path;
    let header = match input.old_path {
        Some(old) => DiffLine::renamed_file_header(old, path),
        None => DiffLine::file_header(path),
    };
    let bail = || FileDiff::new(vec![header.clone()]);

    if is_cancelled() {
        return bail();
    }

    let mut lines = vec![header.clone()];

    let base = input.base.unwrap_or("");
    let head = input.head.unwrap_or(base);
    let index = input.index.unwrap_or(head);
    let working = input.working.unwrap_or(index);

    let base_lines: Vec<&str> = base.lines().collect();
    let head_lines: Vec<&str> = head.lines().collect();
    let index_lines: Vec<&str> = index.lines().collect();
    let working_lines: Vec<&str> = working.lines().collect();
    if is_cancelled() {
        return bail();
    }

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
    if is_cancelled() {
        return bail();
    }

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
        if working_idx & (CANCEL_CHECK_INTERVAL - 1) == 0 && is_cancelled() {
            return bail();
        }
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

    if append_canceled_sections(
        &mut lines,
        &mut output_head_positions,
        path,
        &head_lines,
        &index_lines,
        &head_from_base,
        &index_from_head,
        &working_from_index,
        &head_index_mods,
        &index_working_mods,
        is_cancelled,
    )
    .is_none()
    {
        return bail();
    }

    FileDiff::new(lines)
}

/// Append CanceledCommitted + CanceledStaged sections to `lines`. Returns
/// `None` when the predicate fired during the O(output × index) lookup,
/// so the caller can produce a header-only stub.
#[allow(clippy::too_many_arguments)]
fn append_canceled_sections(
    lines: &mut Vec<DiffLine>,
    output_head_positions: &mut Vec<Option<usize>>,
    path: &str,
    head_lines: &[&str],
    index_lines: &[&str],
    head_from_base: &[Option<usize>],
    index_from_head: &[Option<usize>],
    working_from_index: &[Option<usize>],
    head_index_mods: &std::collections::HashMap<usize, (usize, &str)>,
    index_working_mods: &std::collections::HashMap<usize, (usize, &str)>,
    is_cancelled: &dyn Fn() -> bool,
) -> Option<()> {
    let canceled_committed = collect_canceled_committed(
        head_lines,
        head_from_base,
        index_from_head,
        working_from_index,
        head_index_mods,
        index_working_mods,
    );
    insert_canceled_lines(
        lines,
        canceled_committed,
        LineSource::CanceledCommitted,
        path,
        output_head_positions,
    );

    let canceled_staged = collect_canceled_staged(
        index_lines,
        index_from_head,
        working_from_index,
        index_working_mods,
    );
    // O(output × index_lines) scan; on a 5M-line working tree this can run for
    // tens of seconds. Walk it manually so we can poll the cancel flag.
    let mut output_index_positions: Vec<Option<usize>> = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        if i & (CANCEL_CHECK_INTERVAL - 1) == 0 && is_cancelled() {
            return None;
        }
        output_index_positions
            .push(index_lines.iter().position(|h| h.trim_end() == line.content));
    }
    insert_canceled_lines(
        lines,
        canceled_staged,
        LineSource::CanceledStaged,
        path,
        &mut output_index_positions,
    );
    Some(())
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

    // === Tests for compute_four_way_diff_cancellable ===

    fn make_large_input(lines: usize) -> (String, String) {
        let base = (0..lines).map(|i| format!("base line {i}")).collect::<Vec<_>>().join("\n");
        let working = (0..lines).map(|i| format!("working line {i}")).collect::<Vec<_>>().join("\n");
        (base, working)
    }

    #[test]
    fn test_cancellable_bails_when_cancel_pre_set() {
        // With cancel pre-set, a 50k-line diff that would normally take seconds
        // must return a header-only stub effectively immediately. This is the
        // contract the watchdog relies on.
        let (base, working) = make_large_input(50_000);
        let cancel = AtomicBool::new(true);

        let start = std::time::Instant::now();
        let diff = compute_four_way_diff_cancellable(
            DiffInput {
                path: "huge.log",
                base: Some(&base),
                head: Some(&base),
                index: Some(&base),
                working: Some(&working),
                old_path: None,
            },
            &cancel,
        );
        let elapsed = start.elapsed();

        assert_eq!(diff.lines.len(), 1, "cancelled diff must be header-only");
        assert_eq!(diff.lines[0].source, LineSource::FileHeader);
        assert!(diff.lines[0].content.contains("huge.log"));
        // Loose bound: a real diff of 50k lines takes hundreds of ms+; a
        // header-only stub returns in microseconds. Use 100ms to absorb CI noise.
        assert!(elapsed < std::time::Duration::from_millis(100),
            "cancelled diff took too long ({elapsed:?}), suggesting a heavy phase missed its cancel check");
    }

    #[test]
    fn test_cancellable_finishes_normally_when_not_cancelled() {
        // The cancellable form must produce identical output to the regular
        // form when cancel stays false — otherwise we'd accidentally degrade
        // every diff path that uses the cancellable variant.
        let base = "line1\nline2\nline3";
        let working = "line1\nMODIFIED\nline2\nline3\nline4";
        let cancel = AtomicBool::new(false);

        let regular = compute_four_way_diff(DiffInput {
            path: "test.rs",
            base: Some(base),
            head: Some(base),
            index: Some(base),
            working: Some(working),
            old_path: None,
        });
        let cancellable = compute_four_way_diff_cancellable(
            DiffInput {
                path: "test.rs",
                base: Some(base),
                head: Some(base),
                index: Some(base),
                working: Some(working),
                old_path: None,
            },
            &cancel,
        );

        assert_eq!(regular.lines.len(), cancellable.lines.len());
        for (a, b) in regular.lines.iter().zip(cancellable.lines.iter()) {
            assert_eq!(a.content, b.content);
            assert_eq!(a.source, b.source);
            assert_eq!(a.prefix, b.prefix);
        }
    }

    #[test]
    fn test_with_predicate_main_loop_checkpoint_actually_fires() {
        // Distinguish the main-loop checkpoint from the trailing
        // append_canceled_sections checkpoint by sizing the input so the only
        // way poll #4 can land on the loop is if the in-loop check is wired up.
        //
        // Poll order with all checkpoints intact:
        //   1: entry, 2: post .lines() collect, 3: post maps, 4: main-loop iter 0,
        //   5: append_canceled_sections iter 0.
        //
        // With a small working set (< CANCEL_CHECK_INTERVAL), the main loop
        // polls only at iter 0. We use a counter-based predicate that returns
        // true on poll #5 — the loop poll #4 sees false, the loop completes,
        // and append_canceled poll #5 trips. Result: header-only.
        //
        // If someone removes the main-loop check, poll #4 lands on
        // append_canceled iter 0 (sees false), no more polls fire (< 1024
        // output lines), and the function returns the FULL diff. The
        // assertion below catches that.
        use std::sync::atomic::AtomicUsize;

        // Stay well under CANCEL_CHECK_INTERVAL (1024) so we get exactly one
        // poll per loop's iter 0.
        let line_count = 200;
        let working: String = (0..line_count)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let base = "different content";

        let polls = AtomicUsize::new(0);
        let trip_at = 5;
        let predicate = |poll_log: &AtomicUsize| {
            let n = poll_log.fetch_add(1, Ordering::Relaxed) + 1;
            n >= trip_at
        };
        let pred_fn = || predicate(&polls);

        let diff = compute_four_way_diff_with_predicate(
            DiffInput {
                path: "x.rs",
                base: Some(base),
                head: Some(base),
                index: Some(base),
                working: Some(&working),
                old_path: None,
            },
            &pred_fn,
        );

        let total_polls = polls.load(Ordering::Relaxed);
        assert_eq!(
            diff.lines.len(),
            1,
            "expected header-only stub: in-loop checkpoint must consume poll #4, \
             leaving poll #5 to fire in append_canceled_sections. \
             Got {} lines after {} polls — the main-loop checkpoint was skipped, \
             so poll #4 fell into append_canceled instead and the trip never happened.",
            diff.lines.len(),
            total_polls,
        );
        assert_eq!(
            total_polls, trip_at,
            "expected exactly {trip_at} polls (function returns immediately on trip); \
             got {total_polls}"
        );
    }

    #[test]
    fn test_with_predicate_runs_to_completion_when_never_cancelled() {
        // Sanity: the predicate-based form must produce identical output to the
        // regular form when the predicate stays false. Otherwise we'd be
        // shipping a divergent code path through the AtomicBool wrapper.
        let base = "line1\nline2\nline3";
        let working = "line1\nMODIFIED\nline2\nline3\nline4";

        let regular = compute_four_way_diff(DiffInput {
            path: "test.rs",
            base: Some(base),
            head: Some(base),
            index: Some(base),
            working: Some(working),
            old_path: None,
        });
        let with_pred = compute_four_way_diff_with_predicate(
            DiffInput {
                path: "test.rs",
                base: Some(base),
                head: Some(base),
                index: Some(base),
                working: Some(working),
                old_path: None,
            },
            &|| false,
        );

        assert_eq!(regular.lines.len(), with_pred.lines.len());
        for (a, b) in regular.lines.iter().zip(with_pred.lines.iter()) {
            assert_eq!(a.content, b.content);
            assert_eq!(a.source, b.source);
        }
    }

    #[test]
    fn test_cancellable_output_is_header_only_or_fully_completed_never_partial() {
        // Race a 5ms-delayed cancel flip against a 50k-line diff that
        // (uncancelled) generates 50k+ output lines. The function must end in
        // exactly one of two states: header-only stub (an in-loop checkpoint
        // observed cancel and returned `bail()`) OR fully-completed output
        // (the function finished before the flip).
        //
        // A *partial* line count (between 2 and ~50k) would mean the bail path
        // leaked already-pushed lines through to the caller — exactly the
        // regression class this guards against. Removing the in-loop cancel
        // checks but keeping the entry check would let the function run to
        // completion here (returning ~50k lines) — which still passes the
        // "never partial" check, so this is paired with the strict-time
        // `test_cancellable_bails_when_cancel_pre_set` above.
        let line_count = 50_000;
        let working: String = (0..line_count)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let base = "totally different content";

        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let cancel_clone = cancel.clone();
        let flip = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(5));
            cancel_clone.store(true, Ordering::Relaxed);
        });

        let diff = compute_four_way_diff_cancellable(
            DiffInput {
                path: "x.rs",
                base: Some(base),
                head: Some(base),
                index: Some(base),
                working: Some(&working),
                old_path: None,
            },
            &cancel,
        );
        flip.join().unwrap();

        let n = diff.lines.len();
        assert!(
            n == 1 || n >= line_count,
            "diff has {n} lines — must be header-only (1) or fully completed (>= {line_count}); \
             a partial count means the bail path leaked accumulated lines"
        );
    }
}
