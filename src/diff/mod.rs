//! Diff computation module for branchdiff
//!
//! This module computes 4-way diffs showing changes across:
//! - base (merge-base with main/master)
//! - head (committed on branch)
//! - index (staged)
//! - working (working tree)

mod inline;
mod output;
mod provenance;

// Re-export public types
pub use inline::{compute_inline_diff_merged, InlineDiffResult, InlineSpan};

// Internal imports for the algorithm
use output::{build_working_line_output, determine_deletion_source};
use provenance::{build_modification_map, build_provenance_map};

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
    /// Elided lines indicator (used in context-only view)
    Elided,
}

/// A single line in the diff output
#[derive(Debug, Clone)]
pub struct DiffLine {
    pub source: LineSource,
    pub content: String,
    pub prefix: char,
    /// Line number in the current file (if applicable)
    pub line_number: Option<usize>,
    /// The file this line belongs to
    pub file_path: Option<String>,
    /// Inline spans for within-line diff highlighting (if any)
    /// When empty, the entire content should be shown with the line's source style
    /// When populated, each span indicates whether it's emphasized (changed) or not
    pub inline_spans: Vec<InlineSpan>,
}

impl DiffLine {
    pub fn new(source: LineSource, content: String, prefix: char, line_number: Option<usize>) -> Self {
        Self {
            source,
            content,
            prefix,
            line_number,
            file_path: None,
            inline_spans: Vec::new(),
        }
    }

    /// Create a DiffLine with inline highlighting spans
    pub fn with_inline_spans(mut self, spans: Vec<InlineSpan>) -> Self {
        self.inline_spans = spans;
        self
    }

    pub fn with_file_path(mut self, path: &str) -> Self {
        self.file_path = Some(path.to_string());
        self
    }

    pub fn file_header(path: &str) -> Self {
        Self {
            source: LineSource::FileHeader,
            content: path.to_string(),
            prefix: ' ',
            line_number: None,
            file_path: Some(path.to_string()),
            inline_spans: Vec::new(),
        }
    }

    /// Create an elided lines marker showing how many lines were skipped
    pub fn elided(count: usize) -> Self {
        Self {
            source: LineSource::Elided,
            content: format!("{} lines", count),
            prefix: ' ',
            line_number: None,
            file_path: None,
            inline_spans: Vec::new(),
        }
    }
}

/// Result of diffing a single file across all 4 states
#[derive(Debug)]
pub struct FileDiff {
    pub lines: Vec<DiffLine>,
}

/// Compute file diff showing inline changes with proper source attribution
///
/// This function computes a 4-way diff showing changes across:
/// - base (merge-base with main/master)
/// - head (committed on branch)
/// - index (staged)
/// - working (working tree)
///
/// ## Architecture: Pure Provenance-Driven Design
///
/// The key insight: **provenance is the ONLY source of truth**.
///
/// We do NOT use base→working diff to structure output. Instead:
/// 1. Build provenance maps: base→head→index→working
/// 2. Build modification maps: adjacent delete-insert pairs at each stage
/// 3. Build reverse provenance to find deleted lines
/// 4. Walk through working lines, outputting each with its provenance-determined source
/// 5. Interleave deleted base lines at appropriate positions
///
/// Inline diffs are ONLY created from explicit modification maps - never from
/// content similarity detected by the diff algorithm.
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

    let is_deleted = working_content.is_none() && index_content.is_none();

    // Handle deleted files
    if is_deleted {
        let to_delete = head_content.or(base_content).unwrap_or("");
        for (i, line) in to_delete.lines().enumerate() {
            lines.push(DiffLine::new(
                LineSource::DeletedBase,
                line.to_string(),
                '-',
                Some(i + 1),
            ).with_file_path(path));
        }
        return FileDiff { lines };
    }

    if base == working {
        return FileDiff { lines };
    }

    // Build line vectors
    let base_lines: Vec<&str> = base.lines().collect();
    let head_lines: Vec<&str> = head.lines().collect();
    let index_lines: Vec<&str> = index.lines().collect();
    let working_lines: Vec<&str> = working.lines().collect();

    // =========================================================================
    // STEP 1: Build provenance maps (forward direction)
    // =========================================================================
    // provenance[new_idx] = Some(old_idx) means new line came from old line
    // provenance[new_idx] = None means new line was inserted
    let head_from_base = build_provenance_map(&base_lines, &head_lines);
    let index_from_head = build_provenance_map(&head_lines, &index_lines);
    let working_from_index = build_provenance_map(&index_lines, &working_lines);

    // =========================================================================
    // STEP 2: Build modification maps (adjacent delete-insert pairs)
    // =========================================================================
    // modification_map[new_idx] = (old_idx, old_content)
    // Only created for ADJACENT delete-insert pairs with meaningful similarity
    let base_head_mods = build_modification_map(&base_lines, &head_lines, LineSource::Committed);
    let head_index_mods = build_modification_map(&head_lines, &index_lines, LineSource::Staged);
    let index_working_mods = build_modification_map(&index_lines, &working_lines, LineSource::Unstaged);

    // =========================================================================
    // STEP 3: Build reverse provenance (to find deleted lines)
    // =========================================================================
    // For each base line, track which working line (if any) it ended up as
    // base_to_working[base_idx] = Some(working_idx) if base line is still present
    // base_to_working[base_idx] = None if base line was deleted

    let mut base_to_working: Vec<Option<usize>> = vec![None; base_lines.len()];

    // Track base lines that are still present via provenance chain
    for working_idx in 0..working_lines.len() {
        if let Some(index_idx) = working_from_index.get(working_idx).copied().flatten() {
            if let Some(head_idx) = index_from_head.get(index_idx).copied().flatten() {
                if let Some(base_idx) = head_from_base.get(head_idx).copied().flatten() {
                    base_to_working[base_idx] = Some(working_idx);
                }
            }
        }
    }

    // Also track base lines that were modified at any stage (shown as inline diffs)
    // These should NOT show as deletions - they're merged into the modified working line
    //
    // For committed modifications (base -> head):
    // The head line replaces the base line, so if that head line is present in working,
    // we should mark the base line as "present" (merged into the working line)
    for (head_idx, (base_idx, _)) in &base_head_mods {
        // Find the working line that contains this modified head line
        for working_idx in 0..working_lines.len() {
            if let Some(index_idx) = working_from_index.get(working_idx).copied().flatten() {
                if let Some(h_idx) = index_from_head.get(index_idx).copied().flatten() {
                    if h_idx == *head_idx {
                        base_to_working[*base_idx] = Some(working_idx);
                        break;
                    }
                }
            }
        }
    }

    // For staged modifications (head -> index):
    // Similar logic - if index line is present in working, mark the head line's base as present
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

    // For unstaged modifications (index -> working):
    // When a working line is a meaningful modification of an index line,
    // trace back to find the base line and mark it as present
    for (working_idx, (index_idx, _)) in &index_working_mods {
        if let Some(head_idx) = index_from_head.get(*index_idx).copied().flatten() {
            if let Some(base_idx) = head_from_base.get(head_idx).copied().flatten() {
                base_to_working[base_idx] = Some(*working_idx);
            }
        }
    }

    // =========================================================================
    // STEP 4: Helper functions
    // =========================================================================

    // trace_source: given a working line index, determine its ultimate source
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

    // trace_index_source: given an index line, determine its source
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

    // trace_head_source: given a head line, determine its source
    let trace_head_source = |head_idx: usize| -> LineSource {
        if head_from_base.get(head_idx).copied().flatten().is_some() {
            LineSource::Base
        } else {
            LineSource::Committed
        }
    };

    // =========================================================================
    // STEP 5: Build output - working lines with interleaved deletions
    // =========================================================================
    // Walk through working lines in order, outputting each with its source.
    // Before each working line, output any deleted base lines that should appear here.
    //
    // Key insight: we use the ORIGINAL base line positions to determine where
    // deletions should appear. A deleted base line at position N should appear
    // before the first working line that came from a base line > N.

    // Helper: find the base position for a working line
    // This checks both provenance (for unchanged lines) and modification maps (for modified lines)
    let get_working_base_pos = |working_idx: usize| -> Option<usize> {
        // First try direct provenance chain
        if let Some(index_idx) = working_from_index.get(working_idx).copied().flatten() {
            if let Some(head_idx) = index_from_head.get(index_idx).copied().flatten() {
                if let Some(base_idx) = head_from_base.get(head_idx).copied().flatten() {
                    return Some(base_idx);
                }
            }
        }

        // If no direct provenance, check if this working line is a modification
        // that traces back to a base line via modification maps

        // Check index_working_mods: working line modified from index line
        if let Some((index_idx, _)) = index_working_mods.get(&working_idx) {
            if let Some(head_idx) = index_from_head.get(*index_idx).copied().flatten() {
                if let Some(base_idx) = head_from_base.get(head_idx).copied().flatten() {
                    return Some(base_idx);
                }
            }
        }

        None
    };

    let mut line_num = 1usize;
    let mut next_base_deletion = 0usize;  // Next base line to check for deletion

    for working_idx in 0..working_lines.len() {
        let working_content = working_lines[working_idx].trim_end();

        // Find what base position this working line corresponds to (if any)
        let working_base_pos = get_working_base_pos(working_idx);

        // Determine the deletion boundary: the base position up to which we should
        // output deletions BEFORE this working line.
        //
        // If this working line has a base position, use it.
        // If not (it's an insertion), look ahead to find the next working line
        // that DOES have a base position, and use that as the boundary.
        // This ensures deletions appear before insertions at the same position.
        let deletion_boundary = if let Some(pos) = working_base_pos {
            Some(pos)
        } else {
            // Look ahead for the next base position
            let mut next_base = None;
            for future_idx in (working_idx + 1)..working_lines.len() {
                if let Some(pos) = get_working_base_pos(future_idx) {
                    next_base = Some(pos);
                    break;
                }
            }
            next_base
        };

        // Output any deleted base lines that come BEFORE this deletion boundary
        if let Some(boundary) = deletion_boundary {
            while next_base_deletion < boundary {
                if base_to_working[next_base_deletion].is_none() {
                    // This base line was deleted - output it
                    let base_content = base_lines[next_base_deletion].trim_end();

                    // Determine where it was deleted
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
                }
                next_base_deletion += 1;
            }
        }

        // Now output this working line
        let source = trace_source(working_idx);

        // Check if this is a modification (from one of our modification maps)
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
        line_num += 1;

        // If this working line came from a base line, advance past it
        if let Some(base_pos) = working_base_pos {
            next_base_deletion = next_base_deletion.max(base_pos + 1);
        }
    }

    // Output any remaining deleted base lines at the end
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
        }
        next_base_deletion += 1;
    }

    FileDiff { lines }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to get non-header lines
    fn content_lines(diff: &FileDiff) -> Vec<&DiffLine> {
        diff.lines.iter().filter(|l| l.source != LineSource::FileHeader).collect()
    }

    #[test]
    fn test_no_changes() {
        let content = "line1\nline2\nline3";
        let diff = compute_file_diff_v2("test.txt", Some(content), Some(content), Some(content), Some(content));

        // Should only have file header, no changes
        assert_eq!(diff.lines.len(), 1);
        assert_eq!(diff.lines[0].source, LineSource::FileHeader);
    }

    #[test]
    fn test_committed_addition() {
        let base = "line1\nline2";
        let head = "line1\nline2\nline3";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(head), Some(head));

        // Find the committed line
        let committed_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::Committed)
            .collect();

        assert!(!committed_lines.is_empty());
        assert!(committed_lines.iter().any(|l| l.content == "line3" && l.prefix == '+'));
    }

    #[test]
    fn test_unstaged_addition() {
        let content = "line1\nline2";
        let working = "line1\nline2\nline3";

        let diff = compute_file_diff_v2("test.txt", Some(content), Some(content), Some(content), Some(working));

        let unstaged_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::Unstaged)
            .collect();

        assert!(!unstaged_lines.is_empty());
        assert!(unstaged_lines.iter().any(|l| l.content == "line3" && l.prefix == '+'));
    }

    #[test]
    fn test_staged_addition() {
        let base = "line1\nline2";
        let index = "line1\nline2\nline3";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(index), Some(index));

        let staged_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::Staged)
            .collect();

        assert!(!staged_lines.is_empty());
        assert!(staged_lines.iter().any(|l| l.content == "line3" && l.prefix == '+'));
    }

    #[test]
    fn test_new_file() {
        let working = "line1\nline2";

        let diff = compute_file_diff_v2("test.txt", None, None, None, Some(working));

        let unstaged_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::Unstaged)
            .collect();

        assert_eq!(unstaged_lines.len(), 2);
        assert!(unstaged_lines.iter().all(|l| l.prefix == '+'));
    }

    #[test]
    fn test_deleted_file() {
        let base = "line1\nline2";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), None, None);

        let deleted_lines: Vec<_> = diff.lines.iter()
            .filter(|l| matches!(l.source, LineSource::DeletedBase | LineSource::DeletedCommitted))
            .collect();

        assert!(!deleted_lines.is_empty());
        assert!(deleted_lines.iter().all(|l| l.prefix == '-'));
    }

    // NEW TESTS FOR INLINE DIFF BEHAVIOR (merged single-line view)

    #[test]
    fn test_modified_line_shows_merged_with_inline_spans() {
        let base = "line1\nold content\nline3";
        let working = "line1\nnew content\nline3";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        // Modifications show as merged single line with inline spans
        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();

        assert_eq!(with_spans.len(), 1, "Should have one merged line with inline spans");
        assert_eq!(with_spans[0].content, "new content", "Should show new content");
        assert!(with_spans[0].prefix == ' ', "Merged line should have space prefix");
    }

    #[test]
    fn test_modified_line_position_preserved() {
        // Use content with enough overlap to trigger merged display
        let base = "before\nprocess_data(input)\nafter";
        let working = "before\nprocess_data(input, options)\nafter";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        // Modified line should be in the middle position with inline spans
        let contents: Vec<_> = lines.iter().map(|l| l.content.as_str()).collect();
        assert_eq!(contents, vec!["before", "process_data(input, options)", "after"]);

        // The modified line should have inline spans
        let modified = lines.iter().find(|l| l.content == "process_data(input, options)").unwrap();
        assert!(!modified.inline_spans.is_empty(), "Modified line should have inline spans");
    }

    #[test]
    fn test_multiple_modifications_show_merged() {
        // Use content with enough shared text to trigger merged display
        let base = "line1\nprocess_item(data1)\nline3\nprocess_item(data2)\nline5";
        let working = "line1\nprocess_item(data1, options)\nline3\nprocess_item(data2, options)\nline5";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        // Modifications show as merged lines with inline spans
        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();

        assert_eq!(with_spans.len(), 2, "Should have two merged lines with inline spans");
        assert_eq!(with_spans[0].content, "process_item(data1, options)");
        assert_eq!(with_spans[1].content, "process_item(data2, options)");
    }

    #[test]
    fn test_committed_modification_shows_merged() {
        // Use content with enough shared text to trigger merged display
        let base = "line1\nfunction getData()\nline3";
        let head = "line1\nfunction getData(params)\nline3";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        // Should show as merged line with inline spans showing committed change
        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(with_spans.len(), 1, "Should have one merged line with inline spans");
        assert_eq!(with_spans[0].content, "function getData(params)");

        // The changed portions should have Committed source
        let changed: Vec<_> = with_spans[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Committed))
            .collect();
        assert!(!changed.is_empty(), "Should have Committed-colored spans");
    }

    #[test]
    fn test_staged_modification_shows_merged() {
        // Use content with enough shared text to trigger merged display
        let base = "line1\nfunction getData()\nline3";
        let index = "line1\nfunction getData(params)\nline3";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(index), Some(index));
        let lines = content_lines(&diff);

        // Should show as merged line with inline spans
        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(with_spans.len(), 1, "Should have one merged line with inline spans");
        assert_eq!(with_spans[0].content, "function getData(params)");

        // The changed portions should have Staged source
        let changed: Vec<_> = with_spans[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Staged))
            .collect();
        assert!(!changed.is_empty(), "Should have Staged-colored spans");
    }

    #[test]
    fn test_context_lines_preserved() {
        let base = "line1\nline2\nline3\nline4\nline5";
        let working = "line1\nline2\nmodified\nline4\nline5";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        // With merged inline view, we have 5 lines total: 4 unchanged + 1 modified (with inline spans)
        // The modified line has source=Base but has inline_spans
        let pure_context: Vec<_> = lines.iter()
            .filter(|l| l.source == LineSource::Base && l.inline_spans.is_empty())
            .collect();

        // Should have 4 pure context lines (line1, line2, line4, line5)
        assert_eq!(pure_context.len(), 4);
        assert!(pure_context.iter().all(|l| l.prefix == ' '));
    }

    #[test]
    fn test_line_numbers_correct_after_deletion() {
        let base = "line1\nto_delete\nline3";
        let working = "line1\nline3";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        // Deleted lines should have no line number
        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        assert!(deleted.iter().all(|l| l.line_number.is_none()));

        // Remaining lines should have correct line numbers
        let with_numbers: Vec<_> = lines.iter()
            .filter(|l| l.line_number.is_some())
            .collect();

        assert_eq!(with_numbers.len(), 2);
        assert_eq!(with_numbers[0].line_number, Some(1));
        assert_eq!(with_numbers[1].line_number, Some(2));
    }

    #[test]
    fn test_line_numbers_correct_after_addition() {
        let base = "line1\nline2";
        let working = "line1\nnew_line\nline2";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let with_numbers: Vec<_> = lines.iter()
            .filter(|l| l.line_number.is_some())
            .map(|l| (l.content.as_str(), l.line_number.unwrap()))
            .collect();

        assert_eq!(with_numbers, vec![
            ("line1", 1),
            ("new_line", 2),
            ("line2", 3),
        ]);
    }

    // Tests for modifying lines that were added in previous stages

    #[test]
    fn test_modify_committed_line_in_working_tree() {
        // Scenario: A line was added in a commit, then modified in working tree
        // base: "line1"
        // head: "line1\ncommitted line"  (added "committed line")
        // working: "line1\ncommitted line # with comment"  (modified the committed line)
        //
        // Shows as merged line with inline highlighting:
        // - "committed line" part in gray (unchanged from comparison)
        // - " # with comment" part in Unstaged color (yellow)

        let base = "line1\n";
        let head = "line1\ncommitted line\n";
        let working = "line1\ncommitted line # with comment\n";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(head), Some(working));
        let lines = content_lines(&diff);

        // Should show as merged line with inline spans (not plain +)
        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1, "Should have one merged line with inline spans");
        assert_eq!(merged[0].content, "committed line # with comment");

        // Check that original part is gray (None) and new part is Unstaged
        let unchanged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() && !s.is_deletion)
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        assert!(!unchanged_spans.is_empty(), "Should have unchanged spans (gray)");
        assert!(!unstaged_spans.is_empty(), "Should have Unstaged spans (yellow)");
    }

    #[test]
    fn test_modify_staged_line_in_working_tree() {
        // Scenario: A line was staged, then modified in working tree
        // Shows as merged line with inline highlighting:
        // - Original staged part in gray (unchanged from comparison)
        // - Modified part in Unstaged color (yellow)

        let base = "line1\n";
        let head = "line1\n";  // Nothing committed yet
        let index = "line1\nstaged line\n";  // Added in staging
        let working = "line1\nstaged line modified\n";  // Modified in working

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        // Should show as merged line with inline spans
        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1, "Should have one merged line with inline spans");
        assert_eq!(merged[0].content, "staged line modified");

        // Check that original part is gray (None) and new part is Unstaged
        let unchanged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() && !s.is_deletion)
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        assert!(!unchanged_spans.is_empty(), "Should have unchanged spans (gray)");
        assert!(!unstaged_spans.is_empty(), "Should have Unstaged spans (yellow)");
    }

    #[test]
    fn test_modify_base_line_in_commit() {
        // Scenario: A line from base was modified in a commit
        // When the modification is significant (keeping substantial content),
        // shows as merged line with inline spans.
        // When the content is completely different, shows as delete + insert.
        let base = "do_thing(data)\n";
        let head = "do_thing(data, params)\n";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        // Modification should show as merged line with inline spans
        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(with_spans.len(), 1, "Should have one merged line");
        assert_eq!(with_spans[0].content, "do_thing(data, params)");

        // Changed portions should have Committed source
        let changed: Vec<_> = with_spans[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Committed))
            .collect();
        assert!(!changed.is_empty(), "Should have Committed-colored spans");
    }

    #[test]
    fn test_chain_of_modifications() {
        // Scenario: Line modified at each stage
        // base: "original"
        // head: "committed version"
        // index: "staged version"
        // working: "working version"
        //
        // Net change from base to working shows as merged line with Unstaged highlighting

        let base = "original\n";
        let head = "committed version\n";
        let index = "staged version\n";
        let working = "working version\n";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        // Modification should show as merged line with inline spans
        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(with_spans.len(), 1, "Should have one merged line");
        assert_eq!(with_spans[0].content, "working version");

        // The source for changes should be Unstaged (final modification was in working tree)
        let changed: Vec<_> = with_spans[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();
        assert!(!changed.is_empty(), "Should have Unstaged-colored spans");
    }

    #[test]
    fn test_committed_line_unchanged_through_stages() {
        // Scenario: Line added in commit, unchanged in index and working
        let base = "line1\n";
        let head = "line1\ncommitted line\n";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        let added: Vec<_> = lines.iter().filter(|l| l.prefix == '+').collect();

        assert_eq!(added.len(), 1);
        assert_eq!(added[0].content, "committed line");
        assert_eq!(added[0].source, LineSource::Committed,
            "Unchanged committed line should remain Committed");
    }

    #[test]
    fn test_staged_line_unchanged_in_working() {
        // Scenario: Line added in staging, unchanged in working
        let base = "line1\n";
        let head = "line1\n";
        let index = "line1\nstaged line\n";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(index), Some(index));
        let lines = content_lines(&diff);

        let added: Vec<_> = lines.iter().filter(|l| l.prefix == '+').collect();

        assert_eq!(added.len(), 1);
        assert_eq!(added[0].content, "staged line");
        assert_eq!(added[0].source, LineSource::Staged,
            "Unchanged staged line should remain Staged");
    }

    // INLINE DIFF TESTS (merged single-line view)

    #[test]
    fn test_inline_diff_merged_simple_addition() {
        // Test: "do_thing(data)" -> "do_thing(data, parameters)"
        let result = compute_inline_diff_merged("do_thing(data)", "do_thing(data, parameters)", LineSource::Unstaged);

        assert!(result.is_meaningful, "Should be meaningful - has unchanged portion");
        assert!(!result.spans.is_empty());

        // Should have spans with source=None (unchanged) and source=Some (changed)
        let changed: Vec<_> = result.spans.iter().filter(|s| s.source.is_some()).collect();
        let unchanged: Vec<_> = result.spans.iter().filter(|s| s.source.is_none()).collect();

        assert!(!changed.is_empty(), "Should have changed spans");
        assert!(!unchanged.is_empty(), "Should have unchanged spans");

        // The changed text should contain ", parameters"
        let changed_text: String = changed.iter().map(|s| s.text.as_str()).collect();
        assert!(changed_text.contains(", parameters"),
            "Changed text should contain ', parameters', got: {}", changed_text);

        // Unchanged parts should include "do_thing(data" and ")"
        let unchanged_text: String = unchanged.iter().map(|s| s.text.as_str()).collect();
        assert!(unchanged_text.contains("do_thing(data"),
            "Unchanged should contain 'do_thing(data', got: {}", unchanged_text);
    }

    #[test]
    fn test_inline_diff_merged_modification() {
        // Test: "hello world" -> "hello earth"
        let result = compute_inline_diff_merged("hello world", "hello earth", LineSource::Unstaged);

        assert!(result.is_meaningful, "Should be meaningful - has unchanged portion");

        let changed: Vec<_> = result.spans.iter().filter(|s| s.source.is_some()).collect();
        let unchanged: Vec<_> = result.spans.iter().filter(|s| s.source.is_none()).collect();

        assert!(!changed.is_empty(), "Should have changed spans");
        assert!(!unchanged.is_empty(), "Should have unchanged spans");

        // Unchanged should include "hello "
        let unchanged_text: String = unchanged.iter().map(|s| s.text.as_str()).collect();
        assert!(unchanged_text.contains("hello "),
            "Unchanged should contain 'hello ', got: {}", unchanged_text);
    }

    #[test]
    fn test_inline_diff_merged_no_change() {
        // Same content should have no changed spans
        let result = compute_inline_diff_merged("unchanged line", "unchanged line", LineSource::Unstaged);

        // No changes means nothing is meaningful (the lines are identical)
        let changed: Vec<_> = result.spans.iter().filter(|s| s.source.is_some()).collect();
        assert!(changed.is_empty(), "Should have no changed spans for identical content");
    }

    #[test]
    fn test_inline_diff_merged_complete_replacement() {
        // Completely different lines - should NOT be meaningful (no overlap)
        let result = compute_inline_diff_merged("abc", "xyz", LineSource::Unstaged);

        // Everything is changed, so it's NOT meaningful for inline display
        assert!(!result.is_meaningful, "Complete replacement should NOT be meaningful");

        // Should have spans for BOTH deleted and inserted content
        let deleted: Vec<_> = result.spans.iter().filter(|s| s.is_deletion).collect();
        let inserted: Vec<_> = result.spans.iter().filter(|s| !s.is_deletion && s.source.is_some()).collect();
        let unchanged: Vec<_> = result.spans.iter().filter(|s| s.source.is_none()).collect();

        assert!(!deleted.is_empty(), "Should have deleted spans");
        assert!(!inserted.is_empty(), "Should have inserted spans");
        assert!(unchanged.is_empty(), "Should have no unchanged spans for completely different content");

        let deleted_text: String = deleted.iter().map(|s| s.text.as_str()).collect();
        let inserted_text: String = inserted.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(deleted_text, "abc");
        assert_eq!(inserted_text, "xyz");
    }

    #[test]
    fn test_inline_diff_not_meaningful_falls_back_to_pair() {
        // When lines are completely different, should show -/+ pair
        // Use lines with no common characters at all
        let base = "abcdefgh\n";
        let working = "xyz12345\n";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        // Should have both deleted and added lines (fallback to -/+ pair)
        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        let added: Vec<_> = lines.iter().filter(|l| l.prefix == '+').collect();

        assert_eq!(deleted.len(), 1, "Should have one deleted line");
        assert_eq!(added.len(), 1, "Should have one added line");
        assert_eq!(deleted[0].content, "abcdefgh");
        assert_eq!(added[0].content, "xyz12345");
    }

    #[test]
    fn test_block_of_changes_no_inline_merge() {
        // When multiple consecutive lines are deleted and replaced, don't try to merge them
        // This tests the scenario where we have:
        //   Delete: line1, Delete: line2, Delete: line3
        //   Insert: line1', Insert: line2', Insert: line3'
        // Each should show as separate -/+ lines, not merged
        // Use completely different content to ensure no accidental overlap
        let base = "context\nalpha: aaa,\nbeta: bbb,\ngamma: ccc,\nend";
        let working = "context\nxray: xxx,\nyankee: yyy,\nzulu: zzz,\nend";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        // Should have 3 deleted and 3 added lines, no merged lines
        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        let added: Vec<_> = lines.iter().filter(|l| l.prefix == '+').collect();
        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();

        assert_eq!(deleted.len(), 3, "Should have 3 deleted lines");
        assert_eq!(added.len(), 3, "Should have 3 added lines");
        assert_eq!(merged.len(), 0, "Should have no merged lines for block changes");

        // All added lines should be fully colored (no inline spans means whole line is one color)
        for line in &added {
            assert!(line.inline_spans.is_empty(),
                "Added line '{}' should not have inline spans", line.content);
        }
    }

    #[test]
    fn test_single_line_modification_with_context_shows_inline() {
        // REGRESSION TEST: A single line modification surrounded by context should show
        // as a merged inline diff, not as a plain + line
        // Scenario: appending a comment to an existing line
        let base = "before\ndescribed_class.new(bond).execute\nafter";
        let working = "before\ndescribed_class.new(bond).execute # and add some color commentary\nafter";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        // Should have NO deleted lines (merged into inline diff)
        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        assert_eq!(deleted.len(), 0, "Should have no deleted lines - should be merged");

        // Should have one line with inline spans showing the modification
        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1, "Should have one merged line with inline spans");

        // The merged line should contain the new content
        assert!(merged[0].content.contains("# and add some color commentary"),
            "Merged line should contain the appended comment");

        // Should have unchanged spans (the original code) and changed spans (the comment)
        let unchanged: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none())
            .collect();
        let changed: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_some())
            .collect();

        assert!(!unchanged.is_empty(), "Should have unchanged spans (original code in gray)");
        assert!(!changed.is_empty(), "Should have changed spans (appended comment in color)");

        // The unchanged part should include the original code
        let unchanged_text: String = unchanged.iter().map(|s| s.text.as_str()).collect();
        assert!(unchanged_text.contains("described_class.new(bond).execute"),
            "Unchanged text should contain original code, got: {}", unchanged_text);
    }

    #[test]
    fn test_single_line_committed_modification_shows_inline() {
        // REGRESSION TEST: A single line modification that was COMMITTED should show
        // as a merged inline diff with the changed portion in Committed color
        // Scenario: appending a comment to an existing line, then committing it
        let base = "before\ndescribed_class.new(bond).execute\nafter";
        let head = "before\ndescribed_class.new(bond).execute # and add some color commentary\nafter";
        // head = index = working (change is committed, no further modifications)

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        // Should have NO deleted lines (merged into inline diff)
        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        assert_eq!(deleted.len(), 0, "Should have no deleted lines - should be merged");

        // Should have one line with inline spans showing the modification
        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1, "Should have one merged line with inline spans");

        // The merged line should contain the new content
        assert!(merged[0].content.contains("# and add some color commentary"),
            "Merged line should contain the appended comment");

        // Should have unchanged spans (the original code) and changed spans (the comment)
        let unchanged: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none())
            .collect();
        let changed: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_some())
            .collect();

        assert!(!unchanged.is_empty(), "Should have unchanged spans (original code in gray)");
        assert!(!changed.is_empty(), "Should have changed spans (appended comment in color)");

        // The changed portions should have Committed source (cyan/blue color)
        let committed_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Committed))
            .collect();
        assert!(!committed_spans.is_empty(), "Changed spans should be marked as Committed");

        // The unchanged part should include the original code
        let unchanged_text: String = unchanged.iter().map(|s| s.text.as_str()).collect();
        assert!(unchanged_text.contains("described_class.new(bond).execute"),
            "Unchanged text should contain original code, got: {}", unchanged_text);
    }

    #[test]
    fn test_modification_with_adjacent_empty_line_inserts_shows_inline() {
        // REGRESSION TEST: When a line is modified AND empty lines are added around it,
        // the modification should still show as a merged inline diff
        // Scenario from user: line 339 shows as plain yellow + when it should have inline diff
        // because lines 338 and 340 are empty line insertions
        let base = "before\ndescribed_class.new(bond).execute\nafter";
        let head = "before\n\ndescribed_class.new(bond).execute # comment\n\nafter";
        // Added empty lines before and after, plus modified the middle line

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        // The modified line should have inline spans showing the change
        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1, "Should have one merged line with inline spans");

        // The merged line should contain the appended comment
        assert!(merged[0].content.contains("# comment"),
            "Merged line should contain the appended comment, got: {}", merged[0].content);

        // Should have unchanged spans (the original code) and changed spans (the comment)
        let unchanged: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none())
            .collect();
        let changed: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_some())
            .collect();

        assert!(!unchanged.is_empty(), "Should have unchanged spans (original code in gray)");
        assert!(!changed.is_empty(), "Should have changed spans (appended comment in color)");
    }

    #[test]
    fn test_unstaged_modification_of_committed_line_shows_inline() {
        // Scenario: Line was ADDED in a branch commit, then modified in working tree
        // - Line does NOT exist on master (base)
        // - Line was ADDED in a branch commit (head): "described_class.new(bond).execute"
        // - Line is modified in working tree: "described_class.new(bond).execute # comment"
        // - Should show as merged inline diff with:
        //   - Original committed code in GRAY (unchanged from comparison)
        //   - Appended comment in YELLOW (Unstaged source)
        let base = "before\nafter";  // Line does NOT exist on master
        let head = "before\ndescribed_class.new(bond).execute\nafter";  // Added in commit
        let index = "before\ndescribed_class.new(bond).execute\nafter";  // Same as head (not staged)
        let working = "before\ndescribed_class.new(bond).execute # and add some color commentary\nafter";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        // Should have one line with inline spans showing the modification
        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1, "Should have one merged line with inline spans");

        // The merged line should have the full new content
        assert_eq!(merged[0].content, "described_class.new(bond).execute # and add some color commentary");

        // Check the inline spans - should have unchanged (gray) and Unstaged (yellow) parts
        let unchanged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() && !s.is_deletion)
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        // The original code should be shown in gray (unchanged)
        assert!(!unchanged_spans.is_empty(),
            "Should have unchanged spans for the original code");
        let unchanged_text: String = unchanged_spans.iter().map(|s| s.text.as_str()).collect();
        assert!(unchanged_text.contains("described_class.new(bond).execute"),
            "Unchanged text should contain original code, got: '{}'", unchanged_text);

        // The appended comment should be Unstaged (yellow)
        assert!(!unstaged_spans.is_empty(),
            "Should have Unstaged spans for the appended comment");
        let unstaged_text: String = unstaged_spans.iter().map(|s| s.text.as_str()).collect();
        assert!(unstaged_text.contains("# and add some color commentary"),
            "Unstaged text should contain the comment, got: '{}'", unstaged_text);
    }

    #[test]
    fn test_unstaged_modification_of_base_line_shows_gray_and_yellow() {
        // Scenario: Line EXISTS on master, modified in working tree only
        // - Line exists on master (base): "original_code()"
        // - No changes in commits (head = base)
        // - No staged changes (index = base)
        // - Modified in working tree: "original_code() # added comment"
        // - Should show as merged inline diff with:
        //   - Original code in GRAY (Base source, shown as None in spans)
        //   - Appended comment in YELLOW (Unstaged source)
        let base = "before\noriginal_code()\nafter";
        let head = "before\noriginal_code()\nafter";  // Same as base
        let index = "before\noriginal_code()\nafter";  // Same as base
        let working = "before\noriginal_code() # added comment\nafter";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        // Should have one line with inline spans showing the modification
        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1, "Should have one merged line with inline spans");

        // Check the inline spans
        // Original code should be Base (gray) - represented as None in source
        let base_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() || s.source == Some(LineSource::Base))
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        // Should NOT have Committed spans - this line was on master!
        let committed_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Committed))
            .collect();
        assert!(committed_spans.is_empty(),
            "Should NOT have Committed spans - line was on master, not added in commit. Got: {:?}",
            committed_spans.iter().map(|s| &s.text).collect::<Vec<_>>());

        // The original code should be shown in Base/gray color
        assert!(!base_spans.is_empty(),
            "Should have Base/gray spans for the original code");

        // The appended comment should be Unstaged (yellow)
        assert!(!unstaged_spans.is_empty(),
            "Should have Unstaged spans for the appended comment");
        let unstaged_text: String = unstaged_spans.iter().map(|s| s.text.as_str()).collect();
        assert!(unstaged_text.contains("# added comment"),
            "Unstaged text should contain the comment, got: '{}'", unstaged_text);
    }

    #[test]
    fn test_duplicate_lines_correct_source_attribution() {
        // REGRESSION TEST: When the same line content appears multiple times in a file
        // (common in specs), we need to correctly attribute the source based on POSITION,
        // not just content.
        //
        // Scenario (like in rspec tests):
        // - Base has a common line "end" in two places
        // - Commit adds a NEW "end" line in a different place
        // - Working modifies the COMMITTED "end" line
        // - Should show unchanged (gray) + Unstaged (yellow)
        let base = "context 'first' do\n  it 'test' do\n  end\nend\n";
        let head = "context 'first' do\n  it 'test' do\n  end\n  it 'new test' do\n  end\nend\n";
        let index = head;  // Same as head
        // Modify the line "  end" that was ADDED in the commit (the second one)
        let working = "context 'first' do\n  it 'test' do\n  end\n  it 'new test' do\n  end # added comment\nend\n";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        // Should have one merged line with inline spans
        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1, "Should have one merged line: {:?}", lines);

        // Unchanged portion in gray, changed portion in yellow (Unstaged)
        let unchanged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() && !s.is_deletion)
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        assert!(!unchanged_spans.is_empty(), "Should have unchanged (gray) spans");
        assert!(!unstaged_spans.is_empty(), "Should have Unstaged (yellow) spans");
    }

    #[test]
    fn test_duplicate_lines_earlier_base_line_doesnt_affect_committed_line() {
        // REGRESSION TEST: More complex duplicate line scenario
        //
        // This tests the case where:
        // 1. Base (master) has a line like "described_class.new(bond).execute"
        // 2. Commit adds ANOTHER identical line "described_class.new(bond).execute" elsewhere
        // 3. Working modifies the COMMITTED line (not the base line)
        // 4. Should show as merged inline diff with unchanged (gray) and unstaged (yellow)
        let base = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end
";
        let head = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end

context 'new' do
  it 'new test' do
    described_class.new(bond).execute
  end
end
";
        let index = head;
        // Modify the SECOND "described_class.new(bond).execute" (the one added in commit)
        let working = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end

context 'new' do
  it 'new test' do
    described_class.new(bond).execute # added comment
  end
end
";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        // Should have exactly one merged line with inline spans
        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1, "Should have exactly one merged line with inline spans");

        // The merged line should contain our modified line
        assert!(merged[0].content.contains("described_class.new(bond).execute # added comment"),
            "Merged line should contain the modified content");

        // Check spans: unchanged portion in gray, changed portion in yellow
        let unchanged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() && !s.is_deletion)
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        assert!(!unchanged_spans.is_empty(),
            "Should have unchanged (gray) spans. Got spans: {:?}", merged[0].inline_spans);

        // The "# added comment" should be yellow (Unstaged)
        assert!(!unstaged_spans.is_empty(),
            "Changed portion should be Unstaged (yellow)");
        let unstaged_text: String = unstaged_spans.iter().map(|s| s.text.as_str()).collect();
        assert!(unstaged_text.contains("# added comment"),
            "Unstaged text should contain the comment");
    }

    #[test]
    fn test_last_test_in_committed_block_shows_committed_not_base() {
        // REGRESSION TEST: When we have multiple new tests committed to a branch,
        // and the last test contains a line that also exists in base (like
        // "described_class.new(bond).execute"), the diff algorithm might incorrectly
        // mark that line as "Equal" (matching the base version) instead of recognizing
        // it as a new line from the commit.
        //
        // Scenario:
        // - Base has an existing test with "described_class.new(bond).execute"
        // - Head adds THREE new tests, all containing "described_class.new(bond).execute"
        // - All three tests' lines should be blue (Committed), not gray (Base)
        //
        // The bug: the diff algorithm sees the last test's "described_class.new(bond).execute"
        // as "equal" to the base one, so it shows gray instead of blue.

        let base = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end
";
        // Three new tests added
        let head = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end

context 'first new' do
  it 'first new test' do
    described_class.new(bond).execute
  end
end

context 'second new' do
  it 'second new test' do
    described_class.new(bond).execute
  end
end

context 'third new' do
  it 'third new test' do
    described_class.new(bond).execute
  end
end
";
        let index = head;
        let working = head;  // No working changes

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        // Find all "described_class.new(bond).execute" lines
        let execute_lines: Vec<_> = lines.iter()
            .filter(|l| l.content == "    described_class.new(bond).execute")
            .collect();

        assert_eq!(execute_lines.len(), 4, "Should have 4 execute lines total");

        // The FIRST one (from existing test) should be Base (gray)
        assert_eq!(execute_lines[0].source, LineSource::Base,
            "First execute line (from existing test) should be Base");

        // The other THREE (from new tests) should be Committed (blue)
        for (i, line) in execute_lines.iter().skip(1).enumerate() {
            assert_eq!(line.source, LineSource::Committed,
                "Execute line {} (from new test {}) should be Committed, got {:?}",
                i + 2, i + 1, line.source);
        }
    }

    #[test]
    fn test_committed_block_with_shared_end_line() {
        // REGRESSION TEST: More specific scenario matching user's screenshot
        // The user sees lines 355-361 of a new committed test, but some lines
        // (like line 358 with "described_class...execute") show as gray instead of blue.
        //
        // Key insight: the THIRD test in a block of three is showing gray for a line
        // that matches a line from the existing test. The diff might be seeing this
        // as the "end" of the shared content.
        //
        // From user screenshot:
        // Lines 355-357: blue (committed) - "it 'uses the bond's address...' do"
        // Line 358: GRAY (should be blue!) - blank or "described_class.new(bond).execute"
        // Lines 359-361: blue (committed)
        //
        // The "end" keyword is likely being matched to existing "end" lines.

        let base = "context 'existing' do
  it 'test' do
    described_class.new(bond).execute
  end
end
";
        // Add a new test that has similar structure including "end" lines
        let head = "context 'existing' do
  it 'test' do
    described_class.new(bond).execute
  end
end

context 'new' do
  it 'uses bond data' do
    expected_address = bond.principal_mailing_address

    described_class.new(bond).execute

    notice = Commercial::PremiumDueNotice.new(bond)
    expect(notice.pdf_fields_hash[:address]).to eq(expected_address)
  end
end
";
        let index = head;
        let working = head;

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        // All lines in the "new" test should be Committed (blue), not Base (gray)
        // Even though some lines like "end" and "described_class.new(bond).execute"
        // exist in the base test

        // Check specifically for the new test's lines
        let new_test_lines: Vec<_> = lines.iter()
            .enumerate()
            .filter(|(_, l)| {
                l.content.contains("uses bond data") ||
                l.content.contains("expected_address") ||
                l.content.contains("notice = Commercial") ||
                l.content.contains("expect(notice")
            })
            .collect();

        // These lines are unique to the new test, should be Committed
        for (idx, line) in &new_test_lines {
            assert_eq!(line.source, LineSource::Committed,
                "Line at {} ('{}') in new test should be Committed, got {:?}",
                idx, line.content, line.source);
        }

        // Now check for the "described_class.new(bond).execute" lines
        let execute_lines: Vec<_> = lines.iter()
            .enumerate()
            .filter(|(_, l)| l.content.trim() == "described_class.new(bond).execute")
            .collect();

        // Should have 2: one from base (gray), one from new test (blue)
        assert_eq!(execute_lines.len(), 2, "Should have 2 execute lines, got {:?}",
            execute_lines.iter().map(|(i, l)| (i, &l.content)).collect::<Vec<_>>());

        // First should be Base (from existing test)
        assert_eq!(execute_lines[0].1.source, LineSource::Base,
            "First execute line should be Base");

        // Second should be Committed (from new test) - THIS IS THE BUG CHECK
        assert_eq!(execute_lines[1].1.source, LineSource::Committed,
            "Second execute line (in new test) should be Committed, got {:?}",
            execute_lines[1].1.source);
    }

    #[test]
    fn test_blank_line_in_committed_block_shows_committed() {
        // REGRESSION TEST: Empty/blank lines within a newly committed test block
        // should also be Committed (blue), not Base (gray).
        //
        // Looking at screenshot: line 357 and 359 are blank lines within the new test,
        // but if base also has blank lines in similar positions, they might show gray.

        let base = "context 'existing' do
  it 'test' do
    existing_code

    described_class.new(bond).execute
  end
end
";
        // New test with blank lines at similar relative positions
        let head = "context 'existing' do
  it 'test' do
    existing_code

    described_class.new(bond).execute
  end
end

context 'new' do
  it 'new test' do
    expected_address = bond.principal_mailing_address

    described_class.new(bond).execute

    notice = Commercial::PremiumDueNotice.new(bond)
  end
end
";
        let index = head;
        let working = head;

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        // Debug: print all lines with their source
        eprintln!("\n=== All lines ===");
        for (i, line) in lines.iter().enumerate() {
            eprintln!("{:3}: {:?} '{}' prefix='{}'", i, line.source, line.content, line.prefix);
        }

        // Find the blank lines in the new test (after "context 'new'" appears)
        let new_context_idx = lines.iter().position(|l| l.content.contains("context 'new'"));
        assert!(new_context_idx.is_some(), "Should find 'context 'new'' in output");

        let new_context_idx = new_context_idx.unwrap();

        // All lines after "context 'new'" should be Committed
        for (i, line) in lines.iter().enumerate().skip(new_context_idx) {
            if line.content.trim().is_empty() {
                // Blank line in new test should be Committed, not Base
                assert_eq!(line.source, LineSource::Committed,
                    "Blank line at {} in new test should be Committed, got {:?}",
                    i, line.source);
            }
        }
    }

    #[test]
    fn test_third_test_in_block_of_three_shows_committed() {
        // REGRESSION TEST: User reports that when THREE new tests are added,
        // the THIRD test's interior lines show as gray instead of blue.
        // This might be a diff algorithm artifact where it tries to match
        // lines at the "end" of insertions to the base content.

        let base = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end
";
        // THREE new tests added, all with similar structure
        let head = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end

context 'first new' do
  it 'first test' do
    expected = bond.first_attribute

    described_class.new(bond).execute

    expect(result).to eq(expected)
  end
end

context 'second new' do
  it 'second test' do
    expected = bond.second_attribute

    described_class.new(bond).execute

    expect(result).to eq(expected)
  end
end

context 'third new' do
  it 'third test' do
    expected = bond.third_attribute

    described_class.new(bond).execute

    expect(result).to eq(expected)
  end
end
";
        let index = head;
        let working = head;

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        // Debug output
        eprintln!("\n=== All lines (three tests) ===");
        for (i, line) in lines.iter().enumerate() {
            eprintln!("{:3}: {:?} '{}' prefix='{}'", i, line.source, line.content, line.prefix);
        }

        // Find the third test's lines
        let third_context_idx = lines.iter().position(|l| l.content.contains("context 'third new'"));
        assert!(third_context_idx.is_some(), "Should find third test");
        let third_context_idx = third_context_idx.unwrap();

        // All lines in the third test should be Committed
        for (i, line) in lines.iter().enumerate().skip(third_context_idx) {
            // Skip "end" checks since they're shared structure - focus on unique content
            if line.content.contains("third test") ||
               line.content.contains("third_attribute") ||
               line.content.contains("described_class") ||
               line.content.contains("expect(result)") {
                assert_eq!(line.source, LineSource::Committed,
                    "Line {} in third test should be Committed: '{}', got {:?}",
                    i, line.content, line.source);
            }
        }

        // Specifically check that described_class.new(bond).execute in third test is Committed
        let execute_lines: Vec<_> = lines.iter().enumerate()
            .filter(|(_, l)| l.content.trim() == "described_class.new(bond).execute")
            .collect();

        assert_eq!(execute_lines.len(), 4, "Should have 4 execute lines (1 base + 3 new)");

        // First should be Base, last three should be Committed
        assert_eq!(execute_lines[0].1.source, LineSource::Base, "First execute line should be Base");
        assert_eq!(execute_lines[1].1.source, LineSource::Committed, "Second execute line should be Committed");
        assert_eq!(execute_lines[2].1.source, LineSource::Committed, "Third execute line should be Committed");
        assert_eq!(execute_lines[3].1.source, LineSource::Committed,
            "Fourth execute line (in third test) should be Committed, got {:?}", execute_lines[3].1.source);
    }

    #[test]
    fn test_modified_line_shows_as_single_merged_line() {
        // Modify a line in working tree - should show as single line with inline highlighting
        let base = "do_thing(data)\n";
        let working = "do_thing(data, parameters)\n";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        // Should NOT have separate deleted and added lines - just one merged line
        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        let modified: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();

        assert_eq!(deleted.len(), 0, "Should have no deleted line for modified content");
        assert_eq!(modified.len(), 1, "Should have one merged line with inline spans");

        // The merged line should have the new content
        assert_eq!(modified[0].content, "do_thing(data, parameters)");

        // Should have changed spans with Unstaged source
        let changed: Vec<_> = modified[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();
        assert!(!changed.is_empty(), "Should have Unstaged-colored spans for the changes");

        let changed_text: String = changed.iter().map(|s| s.text.as_str()).collect();
        assert!(changed_text.contains(", parameters"),
            "Changed text should contain ', parameters', got: {}", changed_text);
    }

    #[test]
    fn test_new_line_addition_no_inline_spans() {
        // Adding a completely new line should not have inline spans
        // (since there's no corresponding old line to diff against)
        let base = "line1\n";
        let working = "line1\nnew line\n";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let added: Vec<_> = lines.iter().filter(|l| l.prefix == '+').collect();
        assert_eq!(added.len(), 1);

        // New line without corresponding deletion should not have inline spans
        assert!(added[0].inline_spans.is_empty(),
            "Pure addition should not have inline spans (no line to diff against)");
    }

    #[test]
    fn test_pure_deletion_still_shows_minus() {
        // Deleting a line without replacement should still show - prefix
        let base = "line1\nto_delete\nline3\n";
        let working = "line1\nline3\n";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        assert_eq!(deleted.len(), 1, "Should have one deleted line");
        assert_eq!(deleted[0].content, "to_delete");
    }

    #[test]
    fn test_two_adjacent_committed_modifications() {
        // REGRESSION TEST: When two adjacent lines are both modified in a commit,
        // both should show correctly:
        // - effective_date: "2022-08-30" -> "2023-08-30" (modification)
        // - expiration_date: "2024-08-30" -> "2025-08-30" (modification)
        //
        // The issue: modification map only pairs first Delete with first Insert,
        // leaving the second modification unhandled (missing deletion line)

        let base = r#"            principal_zip: "00000",
            effective_date: "2022-08-30",
            expiration_date: "2024-08-30",
"#;
        let head = r#"            principal_zip: "00000",
            effective_date: "2023-08-30",
            expiration_date: "2025-08-30",
"#;

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        // Both lines were modified, should show as merged with inline spans
        let effective_lines: Vec<_> = lines.iter()
            .filter(|l| l.content.contains("effective_date"))
            .collect();
        let expiration_lines: Vec<_> = lines.iter()
            .filter(|l| l.content.contains("expiration_date"))
            .collect();

        // Both modified lines should be present with new values
        assert_eq!(effective_lines.len(), 1, "Should have one effective_date line");
        assert!(effective_lines[0].content.contains("2023"),
            "effective_date should show new value 2023");
        assert!(!effective_lines[0].inline_spans.is_empty(),
            "effective_date should have inline spans showing modification");

        assert_eq!(expiration_lines.len(), 1, "Should have one expiration_date line");
        assert!(expiration_lines[0].content.contains("2025"),
            "expiration_date should show new value 2025");
        assert!(!expiration_lines[0].inline_spans.is_empty(),
            "expiration_date should have inline spans showing modification");

        // The inline spans should have some Committed-colored portions (the changed chars)
        let effective_has_committed = effective_lines[0].inline_spans.iter()
            .any(|s| s.source == Some(LineSource::Committed));
        let expiration_has_committed = expiration_lines[0].inline_spans.iter()
            .any(|s| s.source == Some(LineSource::Committed));

        assert!(effective_has_committed, "effective_date inline spans should have Committed portions");
        assert!(expiration_has_committed, "expiration_date inline spans should have Committed portions");
    }

    #[test]
    fn test_deletion_positioned_correctly_with_insertions_before() {
        // REGRESSION TEST: Deleted lines should appear at their correct position,
        // not earlier in the file just because there are inserted lines before them.
        //
        // Scenario:
        // - Base has: line1, line2, line3, to_delete, line5
        // - Working: line1, line2, NEW_LINE, line3, line5  (inserted NEW_LINE, deleted to_delete)
        //
        // The deletion of "to_delete" (which was at position 4 in base) should appear
        // AFTER line3 (position 3 in base), not before NEW_LINE.
        //
        // Bug: When we hit NEW_LINE (which has no base position), the old code would
        // output all remaining deletions at that point, causing "to_delete" to appear
        // before NEW_LINE instead of after line3.

        let base = "line1\nline2\nline3\nto_delete\nline5";
        let working = "line1\nline2\nNEW_LINE\nline3\nline5";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        // Get the order of lines
        let line_contents: Vec<&str> = lines.iter().map(|l| l.content.as_str()).collect();

        // Find positions
        let new_line_pos = line_contents.iter().position(|&c| c == "NEW_LINE")
            .expect("NEW_LINE should be in output");
        let to_delete_pos = line_contents.iter().position(|&c| c == "to_delete")
            .expect("to_delete should be in output as deletion");
        let line3_pos = line_contents.iter().position(|&c| c == "line3")
            .expect("line3 should be in output");

        // The deletion "to_delete" should appear AFTER line3, not before NEW_LINE
        assert!(to_delete_pos > line3_pos,
            "Deletion 'to_delete' should appear after 'line3', but found at {} vs {}",
            to_delete_pos, line3_pos);
        assert!(to_delete_pos > new_line_pos,
            "Deletion 'to_delete' should appear after 'NEW_LINE', but found at {} vs {}",
            to_delete_pos, new_line_pos);

        // Verify the deleted line has the correct marker
        let deleted_line = &lines[to_delete_pos];
        assert_eq!(deleted_line.prefix, '-', "to_delete should have '-' prefix");
    }

    #[test]
    fn test_deletion_before_insertion_at_same_position() {
        // REGRESSION TEST: When a line is deleted and replaced with a new line,
        // the deletion should appear BEFORE the insertion (minus before plus).
        //
        // Scenario:
        // - Base has: def principal_mailing_address / commercial_renewal.principal_mailing_address / end
        // - Working: def principal_mailing_address / "new content" / end
        //
        // Expected output order:
        //   def principal_mailing_address
        // - commercial_renewal.principal_mailing_address  (deletion first)
        // + "new content"                                  (insertion second)
        //   end

        let base = "def principal_mailing_address\n  commercial_renewal.principal_mailing_address\nend";
        let working = "def principal_mailing_address\n  \"new content\"\nend";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        // Get the order of lines
        let line_contents: Vec<&str> = lines.iter().map(|l| l.content.as_str()).collect();
        let prefixes: Vec<char> = lines.iter().map(|l| l.prefix).collect();

        // Find positions
        let deleted_pos = line_contents.iter().position(|&c| c.contains("commercial_renewal"))
            .expect("deleted line should be in output");
        let inserted_pos = line_contents.iter().position(|&c| c.contains("new content"))
            .expect("inserted line should be in output");

        // Deletion should come BEFORE insertion
        assert!(deleted_pos < inserted_pos,
            "Deletion should appear before insertion: deleted at {}, inserted at {}.\nOrder: {:?}",
            deleted_pos, inserted_pos, line_contents);

        // Verify prefixes
        assert_eq!(prefixes[deleted_pos], '-', "Deleted line should have '-' prefix");
        assert_eq!(prefixes[inserted_pos], '+', "Inserted line should have '+' prefix");
    }

    #[test]
    fn test_inline_diff_thresholds() {
        // Test various pairs and their meaningful status
        // Meaningful = has a contiguous unchanged segment of at least 5 chars
        let test_cases = [
            // Unrelated lines: max segment is 4 ("body"), not meaningful
            ("  body_line", "  \"new body\"", false, "unrelated lines"),
            // Real modification: max segment is 13 ("do_thing(data"), meaningful
            ("do_thing(data)", "do_thing(data, parameters)", true, "real modification"),
            // Simple word change: max segment is 6 ("hello "), meaningful
            ("hello world", "hello earth", true, "simple word change"),
        ];

        for (old, new, expected, desc) in test_cases {
            let result = compute_inline_diff_merged(old, new, LineSource::Unstaged);
            assert_eq!(result.is_meaningful, expected,
                "Case '{}': old='{}', new='{}', expected is_meaningful={}",
                desc, old, new, expected);
        }
    }

    #[test]
    fn test_deletion_appears_after_preceding_context_line() {
        // REGRESSION TEST: A deleted line should appear AFTER the context line
        // that preceded it in the base file, and BEFORE the line that follows.
        //
        // Scenario:
        // - Base has: def foo / body_line / end
        // - Working: def foo / "new body" / end
        //
        // The deletion of "body_line" (base position 1) should appear:
        // - AFTER "def foo" (base position 0)
        // - BEFORE "new body" (insertion that replaced it)
        // - BEFORE "end" (base position 2)
        //
        // Expected output order:
        //   def foo           (context, base pos 0)
        // - body_line         (deletion of base pos 1)
        // + "new body"        (insertion)
        //   end               (context, base pos 2)

        let base = "def foo\n  body_line\nend";
        let working = "def foo\n  \"new body\"\nend";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        // Get the order of lines
        let line_contents: Vec<&str> = lines.iter().map(|l| l.content.as_str()).collect();

        // Find positions
        let def_pos = line_contents.iter().position(|&c| c.contains("def foo"))
            .expect("def foo should be in output");
        let deleted_pos = line_contents.iter().position(|&c| c.contains("body_line"))
            .expect("body_line should be in output as deletion");
        let inserted_pos = line_contents.iter().position(|&c| c.contains("new body"))
            .expect("new body should be in output");
        let end_pos = line_contents.iter().position(|&c| c == "end")
            .expect("end should be in output");

        // Deletion should appear AFTER def foo, BEFORE insertion, BEFORE end
        assert!(deleted_pos > def_pos,
            "Deletion should appear after 'def foo': deleted at {}, def at {}.\nOrder: {:?}",
            deleted_pos, def_pos, line_contents);
        assert!(deleted_pos < inserted_pos,
            "Deletion should appear before insertion: deleted at {}, inserted at {}.\nOrder: {:?}",
            deleted_pos, inserted_pos, line_contents);
        assert!(deleted_pos < end_pos,
            "Deletion should appear before 'end': deleted at {}, end at {}.\nOrder: {:?}",
            deleted_pos, end_pos, line_contents);

        // The output order should be exactly: def foo, body_line (deleted), new body, end
        assert_eq!(def_pos, 0, "def foo should be first");
        assert_eq!(deleted_pos, 1, "deletion should be second");
        assert_eq!(inserted_pos, 2, "insertion should be third");
        assert_eq!(end_pos, 3, "end should be fourth");
    }

    #[test]
    fn test_deletion_after_modified_line() {
        // REGRESSION TEST: When a method definition is modified (inline diff) and
        // the body line is deleted+replaced, the deletion should appear AFTER the
        // modified method definition, not before it.
        //
        // Scenario (matching the real bug):
        // - Base: def principal_mailing_address / commercial_renewal.principal_mailing_address / end
        // - Working: def pribond_descripal_mailtiong_address / "new content" / end
        //
        // The method def line is modified (shown with inline diff).
        // The body line is completely replaced (delete + insert).
        //
        // Expected output:
        //   def pribond_descripal_mailtiong_address  (modified line with inline spans)
        // - commercial_renewal.principal_mailing_address  (deletion)
        // + "new content"  (insertion)
        //   end

        let base = "def principal_mailing_address\n  commercial_renewal.principal_mailing_address\nend";
        let working = "def pribond_descripal_mailtiong_address\n  \"new content\"\nend";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        // Get the order of lines
        let line_contents: Vec<&str> = lines.iter().map(|l| l.content.as_str()).collect();
        let prefixes: Vec<char> = lines.iter().map(|l| l.prefix).collect();

        // Find positions
        let def_pos = line_contents.iter().position(|&c| c.contains("pribond"))
            .expect("modified def should be in output");
        let deleted_pos = line_contents.iter().position(|&c| c.contains("commercial_renewal"))
            .expect("commercial_renewal should be in output as deletion");
        let inserted_pos = line_contents.iter().position(|&c| c.contains("new content"))
            .expect("new content should be in output");
        let end_pos = line_contents.iter().position(|&c| c == "end")
            .expect("end should be in output");

        // Verify the deletion has '-' prefix
        assert_eq!(prefixes[deleted_pos], '-', "commercial_renewal should have '-' prefix");

        // Critical assertion: deletion must appear AFTER the def line
        assert!(deleted_pos > def_pos,
            "Deletion should appear AFTER the def line: deleted at {}, def at {}.\nOrder: {:?}",
            deleted_pos, def_pos, line_contents);

        // Expected order: def (0), deletion (1), insertion (2), end (3)
        assert_eq!(def_pos, 0, "def should be first");
        assert_eq!(deleted_pos, 1, "deletion should be second (after def)");
        assert_eq!(inserted_pos, 2, "insertion should be third");
        assert_eq!(end_pos, 3, "end should be fourth");
    }

    #[test]
    fn test_trailing_context_after_addition() {
        // REGRESSION TEST: When a line is added near the end, trailing context
        // lines should still appear.
        //
        // Scenario:
        // - Base: def foo / end / end
        // - Working: def foo / new_line / end / end
        //
        // Expected output:
        //   def foo
        // + new_line
        //   end
        //   end

        let base = "def foo\nend\nend";
        let working = "def foo\nnew_line\nend\nend";

        let diff = compute_file_diff_v2("test.rb", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        // Debug output
        eprintln!("Lines ({}):", lines.len());
        for (i, line) in lines.iter().enumerate() {
            eprintln!("  [{}] {} {:?} '{}'", i, line.prefix, line.source, line.content);
        }

        // Should have 4 lines: def foo, new_line (added), end, end
        assert_eq!(lines.len(), 4, "Should have 4 content lines");

        assert_eq!(lines[0].content, "def foo");
        assert_eq!(lines[0].source, LineSource::Base);

        assert_eq!(lines[1].content, "new_line");
        assert_eq!(lines[1].prefix, '+');

        assert_eq!(lines[2].content, "end");
        assert_eq!(lines[2].source, LineSource::Base);

        assert_eq!(lines[3].content, "end");
        assert_eq!(lines[3].source, LineSource::Base);
    }

    #[test]
    fn test_trailing_context_after_committed_addition() {
        // Same as above but the addition was in a commit (head), not working tree
        //
        // Scenario:
        // - Base: def foo / end / end
        // - Head/Index/Working: def foo / new_line / end / end
        //
        // Expected output - new_line should show as Committed (cyan):
        //   def foo
        // + new_line (Committed)
        //   end
        //   end

        let base = "def foo\nend\nend";
        let head = "def foo\nnew_line\nend\nend";

        let diff = compute_file_diff_v2("test.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        // Debug output
        eprintln!("Lines ({}):", lines.len());
        for (i, line) in lines.iter().enumerate() {
            eprintln!("  [{}] {} {:?} '{}'", i, line.prefix, line.source, line.content);
        }

        // Should have 4 lines
        assert_eq!(lines.len(), 4, "Should have 4 content lines including trailing context");

        assert_eq!(lines[0].content, "def foo");
        assert_eq!(lines[0].source, LineSource::Base);

        assert_eq!(lines[1].content, "new_line");
        assert_eq!(lines[1].source, LineSource::Committed);
        assert_eq!(lines[1].prefix, '+');

        assert_eq!(lines[2].content, "end");
        assert_eq!(lines[2].source, LineSource::Base);

        assert_eq!(lines[3].content, "end");
        assert_eq!(lines[3].source, LineSource::Base);
    }

    #[test]
    fn test_addition_at_end_of_file_with_trailing_context() {
        // REGRESSION TEST: Adding a line at the END followed by existing base lines
        //
        // Scenario - this matches the reported bug:
        // - Base: some_code / end / end
        // - Commit adds a line BEFORE the final two "end" lines
        // - Head/Index/Working: some_code / new_end / end / end
        //
        // All lines should appear, including the trailing "end" "end" from base

        let base = "class Foo\n  def bar\n  end\nend";
        let head = "class Foo\n  def bar\n    new_line\n  end\nend";

        let diff = compute_file_diff_v2("test.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        // Debug output
        eprintln!("\n=== Addition at end of file ===");
        eprintln!("Lines ({}):", lines.len());
        for (i, line) in lines.iter().enumerate() {
            eprintln!("  [{}] {} {:?} '{}'", i, line.prefix, line.source, line.content);
        }

        // Should have 5 lines total:
        // class Foo (base)
        //   def bar (base)
        //     new_line (committed/added)
        //   end (base)
        // end (base)

        assert_eq!(lines.len(), 5, "Should have 5 content lines");

        assert_eq!(lines[0].content, "class Foo");
        assert_eq!(lines[1].content, "  def bar");
        assert_eq!(lines[2].content, "    new_line");
        assert_eq!(lines[2].source, LineSource::Committed);
        assert_eq!(lines[3].content, "  end");
        assert_eq!(lines[3].source, LineSource::Base);
        assert_eq!(lines[4].content, "end");
        assert_eq!(lines[4].source, LineSource::Base);
    }

    #[test]
    fn test_addition_before_two_trailing_ends() {
        // EXACT SCENARIO from user bug report:
        // - Base: "do\n  body\nend\nend" (two 'end' lines at end)
        // - Head adds "+ end" BEFORE the existing two 'end' lines
        // Expected:
        //   do (base)
        //   body (base)
        //   + end (committed - blue)  <-- this and the following are "missing"
        //   end (base)                <-- trailing context
        //   end (base)                <-- trailing context

        let base = "do\n  body\nend\nend";
        let head = "do\n  body\n  new_end\nend\nend";  // Added "  new_end" between body and first end

        let diff = compute_file_diff_v2("test.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        eprintln!("\n=== Addition before two trailing ends ===");
        eprintln!("Lines ({}):", lines.len());
        for (i, line) in lines.iter().enumerate() {
            eprintln!("  [{}] {} {:?} '{}'", i, line.prefix, line.source, line.content);
        }

        assert_eq!(lines.len(), 5, "Should have 5 content lines");

        assert_eq!(lines[0].content, "do");
        assert_eq!(lines[0].source, LineSource::Base);

        assert_eq!(lines[1].content, "  body");
        assert_eq!(lines[1].source, LineSource::Base);

        assert_eq!(lines[2].content, "  new_end");
        assert_eq!(lines[2].source, LineSource::Committed);
        assert_eq!(lines[2].prefix, '+');

        // These are the "missing" trailing ends
        assert_eq!(lines[3].content, "end");
        assert_eq!(lines[3].source, LineSource::Base);

        assert_eq!(lines[4].content, "end");
        assert_eq!(lines[4].source, LineSource::Base);
    }

    #[test]
    fn test_final_file_ends_with_addition() {
        // Edge case: the file ends with an added line - no trailing base lines
        // Make sure the addition itself still shows

        let base = "do\n  body\nend";
        let head = "do\n  body\nend\n  extra";  // Added line at very end

        let diff = compute_file_diff_v2("test.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        eprintln!("\n=== File ends with addition ===");
        eprintln!("Lines ({}):", lines.len());
        for (i, line) in lines.iter().enumerate() {
            eprintln!("  [{}] {} {:?} '{}'", i, line.prefix, line.source, line.content);
        }

        assert_eq!(lines.len(), 4, "Should have 4 content lines");

        assert_eq!(lines[0].content, "do");
        assert_eq!(lines[0].source, LineSource::Base);

        assert_eq!(lines[1].content, "  body");
        assert_eq!(lines[1].source, LineSource::Base);

        assert_eq!(lines[2].content, "end");
        assert_eq!(lines[2].source, LineSource::Base);

        // The trailing addition
        assert_eq!(lines[3].content, "  extra");
        assert_eq!(lines[3].source, LineSource::Committed);
        assert_eq!(lines[3].prefix, '+');
    }

    #[test]
    fn test_file_without_trailing_newline() {
        // REGRESSION TEST: Files that don't end with a newline
        // The .lines() iterator doesn't include empty trailing lines
        //
        // Scenario from bug: file content ends with "end\nend\nend" (no final newline)
        // When comparing base vs head, lines might be miscounted

        // Note: NO trailing newline in these strings
        let base = "line1\nline2\nend\nend\nend";
        let head = "line1\nline2\nnew_line\nend\nend\nend";

        eprintln!("\n=== No trailing newline test ===");
        eprintln!("Base lines: {:?}", base.lines().collect::<Vec<_>>());
        eprintln!("Head lines: {:?}", head.lines().collect::<Vec<_>>());

        let diff = compute_file_diff_v2("test.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        eprintln!("Diff lines ({}):", lines.len());
        for (i, line) in lines.iter().enumerate() {
            eprintln!("  [{}] {} {:?} '{}'", i, line.prefix, line.source, line.content);
        }

        // Should have 6 lines total:
        // line1 (base)
        // line2 (base)
        // new_line (committed)
        // end (base)
        // end (base)
        // end (base)

        assert_eq!(lines.len(), 6, "Should have 6 content lines");
        assert_eq!(lines[0].content, "line1");
        assert_eq!(lines[1].content, "line2");
        assert_eq!(lines[2].content, "new_line");
        assert_eq!(lines[2].source, LineSource::Committed);
        assert_eq!(lines[3].content, "end");
        assert_eq!(lines[3].source, LineSource::Base);
        assert_eq!(lines[4].content, "end");
        assert_eq!(lines[4].source, LineSource::Base);
        assert_eq!(lines[5].content, "end");
        assert_eq!(lines[5].source, LineSource::Base);
    }

    #[test]
    fn test_exact_scenario_from_bug_report() {
        // EXACT scenario from the bug report:
        // Base file ends with:
        //   ...some tests...
        //   end  <- end of describe block
        //   end  <- end of RSpec.describe
        //
        // Head adds a new test BEFORE the final two ends:
        //   ...some tests...
        //   (empty line)
        //   it "new test" do
        //     expect(...)
        //   end   <- end of new test (ADDED)
        //   end   <- end of describe block (BASE)
        //   end   <- end of RSpec.describe (BASE)

        let base = r##"  describe "#method" do
    it "test 1" do
      expect(true).to be(true)
    end
  end
end"##;

        let head = r##"  describe "#method" do
    it "test 1" do
      expect(true).to be(true)
    end

    it "new test" do
      expect(false).to be(false)
    end
  end
end"##;

        eprintln!("\n=== Exact bug scenario test ===");
        eprintln!("Base ({} lines):", base.lines().count());
        for (i, line) in base.lines().enumerate() {
            eprintln!("  base[{}]: '{}'", i, line);
        }
        eprintln!("Head ({} lines):", head.lines().count());
        for (i, line) in head.lines().enumerate() {
            eprintln!("  head[{}]: '{}'", i, line);
        }

        let diff = compute_file_diff_v2("spec.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        eprintln!("\nDiff output ({} lines):", lines.len());
        for (i, line) in lines.iter().enumerate() {
            eprintln!("  [{}] {} {:?} '{}'", i, line.prefix, line.source, line.content);
        }

        // Expected:
        // [0] describe "#method" do       (base)
        // [1]   it "test 1" do            (base)
        // [2]     expect(true)...         (base)
        // [3]   end                       (base)
        // [4] (empty line)                (committed +)
        // [5]   it "new test" do          (committed +)
        // [6]     expect(false)...        (committed +)
        // [7]   end                       (committed +)
        // [8] end                         (base) <-- trailing
        // [9] end                         (base) <-- trailing

        assert!(lines.len() >= 10, "Should have at least 10 lines, got {}", lines.len());

        // Check the trailing base lines exist
        let last_two: Vec<_> = lines.iter().rev().take(2).collect();
        assert_eq!(last_two[0].content, "end", "Last line should be 'end'");
        assert_eq!(last_two[0].source, LineSource::Base, "Last line should be Base");
        // Second to last is "  end" (with indent)
        assert_eq!(last_two[1].content, "  end", "Second to last should be '  end'");
        assert_eq!(last_two[1].source, LineSource::Base, "Second to last should be Base");

        // Check that the added "end" (end of new test) is Committed
        let added_end = lines.iter().find(|l| l.content == "    end" && l.source == LineSource::Committed);
        assert!(added_end.is_some(), "Should have a committed '    end' line");

        // CRITICAL: All 10 lines should be present
        assert_eq!(lines.len(), 10, "All 10 lines should be in the diff output");
    }

    #[test]
    fn test_staging_changes_line_source_from_unstaged_to_staged() {
        let base = "line1\nline2";
        let modified = "line1\nline2\nline3";

        // Before staging: change is only in working tree
        let diff_before = compute_file_diff_v2(
            "test.txt",
            Some(base),
            Some(base),
            Some(base),      // index same as base
            Some(modified),  // working has the change
        );

        let unstaged_lines: Vec<_> = diff_before.lines.iter()
            .filter(|l| l.source == LineSource::Unstaged && l.content == "line3")
            .collect();
        assert_eq!(unstaged_lines.len(), 1, "line3 should be Unstaged before staging");

        // After staging: change is in index and working tree
        let diff_after = compute_file_diff_v2(
            "test.txt",
            Some(base),
            Some(base),
            Some(modified),  // index now has the change
            Some(modified),  // working same as index
        );

        let staged_lines: Vec<_> = diff_after.lines.iter()
            .filter(|l| l.source == LineSource::Staged && l.content == "line3")
            .collect();
        assert_eq!(staged_lines.len(), 1, "line3 should be Staged after staging");

        // Verify line3 is NOT unstaged after staging
        let still_unstaged: Vec<_> = diff_after.lines.iter()
            .filter(|l| l.source == LineSource::Unstaged && l.content == "line3")
            .collect();
        assert_eq!(still_unstaged.len(), 0, "line3 should NOT be Unstaged after staging");
    }
}
