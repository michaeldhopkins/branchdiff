use similar::{ChangeTag, TextDiff};

use super::LineSource;

/// Result of computing inline diff - indicates whether the diff is meaningful
/// for single-line merged display
#[derive(Debug)]
pub struct InlineDiffResult {
    /// The spans for rendering (only used if is_meaningful is true)
    pub spans: Vec<InlineSpan>,
    /// Whether this diff has meaningful inline changes (some unchanged portions)
    /// If false, the lines are too different and should use -/+ pair display
    pub is_meaningful: bool,
}

/// Represents a span within a line for inline diff highlighting
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineSpan {
    /// The text content of this span
    pub text: String,
    /// The source/style for this span (None = unchanged from base, Some = this specific source)
    pub source: Option<LineSource>,
    /// Whether this span represents deleted text (should be rendered but isn't part of actual line content)
    /// Deleted spans appear visually before their corresponding insertion at the same position
    pub is_deletion: bool,
}

/// Compute inline diff spans showing BOTH deleted and inserted text
///
/// The key insight: we show deleted text (in red/deletion color) immediately before
/// the corresponding inserted text (in the change_source color). Unchanged text
/// uses source=None (gray).
///
/// Example: "commercial_renewal.name" -> "bond.name"
/// Produces spans: [
///   { text: "commercial_renewal", source: DeletedBase, is_deletion: true },
///   { text: "bond", source: Committed, is_deletion: false },
///   { text: ".name", source: None, is_deletion: false },
/// ]
///
/// The deletion_source parameter specifies what color to use for deleted text
/// (e.g., DeletedBase for committed changes, DeletedCommitted for staged, etc.)
pub fn compute_inline_diff_merged(
    old_line: &str,
    new_line: &str,
    change_source: LineSource,
) -> InlineDiffResult {
    let deletion_source = match change_source {
        LineSource::Committed => LineSource::DeletedBase,
        LineSource::Staged => LineSource::DeletedCommitted,
        LineSource::Unstaged => LineSource::DeletedStaged,
        _ => LineSource::DeletedBase,
    };

    let diff = TextDiff::from_chars(old_line, new_line);
    let mut spans = Vec::new();
    let mut max_unchanged_segment = 0usize;
    let mut num_unchanged_segments = 0usize;
    // Track whether we've seen any non-whitespace content yet.
    // Leading whitespace shouldn't count toward "meaningful" unchanged segments
    // because indentation matching creates false positives (e.g., `}` vs `.map(...)`
    // both have 12 spaces of indentation but are completely different code).
    let mut seen_non_whitespace = false;

    let mut pending_unchanged = String::new();
    let mut pending_deleted = String::new();
    let mut pending_inserted = String::new();

    // Helper to flush pending unchanged content, updating metrics
    let flush_unchanged = |pending: &mut String,
                               spans: &mut Vec<InlineSpan>,
                               seen_non_whitespace: &mut bool,
                               max_unchanged_segment: &mut usize,
                               num_unchanged_segments: &mut usize| {
        if pending.is_empty() {
            return;
        }
        let has_non_ws = pending.chars().any(|c| !c.is_whitespace());
        if *seen_non_whitespace || has_non_ws {
            let segment_len = pending.chars().count();
            *max_unchanged_segment = (*max_unchanged_segment).max(segment_len);
            *num_unchanged_segments += 1;
        }
        if has_non_ws {
            *seen_non_whitespace = true;
        }
        spans.push(InlineSpan {
            text: std::mem::take(pending),
            source: None,
            is_deletion: false,
        });
    };

    // Helper to flush pending changed content (deleted or inserted)
    let flush_changed = |pending: &mut String,
                         spans: &mut Vec<InlineSpan>,
                         seen_non_whitespace: &mut bool,
                         source: LineSource,
                         is_deletion: bool| {
        if pending.is_empty() {
            return;
        }
        if pending.chars().any(|c| !c.is_whitespace()) {
            *seen_non_whitespace = true;
        }
        spans.push(InlineSpan {
            text: std::mem::take(pending),
            source: Some(source),
            is_deletion,
        });
    };

    for change in diff.iter_all_changes() {
        let text = change.value();
        match change.tag() {
            ChangeTag::Equal => {
                if !pending_deleted.is_empty() || !pending_inserted.is_empty() {
                    flush_unchanged(
                        &mut pending_unchanged,
                        &mut spans,
                        &mut seen_non_whitespace,
                        &mut max_unchanged_segment,
                        &mut num_unchanged_segments,
                    );
                    flush_changed(
                        &mut pending_deleted,
                        &mut spans,
                        &mut seen_non_whitespace,
                        deletion_source,
                        true,
                    );
                    flush_changed(
                        &mut pending_inserted,
                        &mut spans,
                        &mut seen_non_whitespace,
                        change_source,
                        false,
                    );
                }
                pending_unchanged.push_str(text);
            }
            ChangeTag::Delete => {
                pending_deleted.push_str(text);
            }
            ChangeTag::Insert => {
                pending_inserted.push_str(text);
            }
        }
    }

    // Flush any remaining content
    flush_unchanged(
        &mut pending_unchanged,
        &mut spans,
        &mut seen_non_whitespace,
        &mut max_unchanged_segment,
        &mut num_unchanged_segments,
    );
    flush_changed(
        &mut pending_deleted,
        &mut spans,
        &mut seen_non_whitespace,
        deletion_source,
        true,
    );
    flush_changed(
        &mut pending_inserted,
        &mut spans,
        &mut seen_non_whitespace,
        change_source,
        false,
    );

    // Determine if inline diff is meaningful:
    // We need:
    // 1. A contiguous unchanged segment of meaningful length (>= 5 chars)
    // 2. Not too many scattered unchanged segments (fragmentation)
    //
    // The fragmentation check catches cases like a for-loop becoming a comment,
    // where scattered single-char or word matches (like "the", "span", " ") create
    // many small unchanged segments but the lines are structurally completely different.
    //
    // A diff with 1-3 unchanged segments is likely a real modification (prefix, middle, suffix)
    // A diff with 5+ unchanged segments is likely coincidental character matches
    //
    // Examples:
    // - "do_thing(data)" -> "do_thing(data, params)": 2 segments (prefix + suffix) ✓
    // - "end" -> "end # comment": 1 segment (prefix) ✓
    // - "hello world" -> "hello earth": 2 segments ✓
    // - "for i in (x..y).rev() {" -> "// comment text": many scattered segments ✗
    let has_meaningful_segment = max_unchanged_segment >= 5;
    let not_too_fragmented = num_unchanged_segments <= 4;

    let is_meaningful = has_meaningful_segment && not_too_fragmented;

    InlineDiffResult { spans, is_meaningful }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === InlineSpan creation and properties ===

    #[test]
    fn test_inline_span_unchanged() {
        let span = InlineSpan {
            text: "unchanged".to_string(),
            source: None,
            is_deletion: false,
        };
        assert_eq!(span.text, "unchanged");
        assert!(span.source.is_none());
        assert!(!span.is_deletion);
    }

    #[test]
    fn test_inline_span_deletion() {
        let span = InlineSpan {
            text: "deleted".to_string(),
            source: Some(LineSource::DeletedBase),
            is_deletion: true,
        };
        assert_eq!(span.text, "deleted");
        assert_eq!(span.source, Some(LineSource::DeletedBase));
        assert!(span.is_deletion);
    }

    #[test]
    fn test_inline_span_insertion() {
        let span = InlineSpan {
            text: "inserted".to_string(),
            source: Some(LineSource::Committed),
            is_deletion: false,
        };
        assert_eq!(span.text, "inserted");
        assert_eq!(span.source, Some(LineSource::Committed));
        assert!(!span.is_deletion);
    }

    // === Deletion source mapping ===

    #[test]
    fn test_deletion_source_for_committed_change() {
        let result = compute_inline_diff_merged("old content", "new content", LineSource::Committed);

        // Deleted spans should have DeletedBase source
        let deletion_spans: Vec<_> = result.spans.iter()
            .filter(|s| s.is_deletion)
            .collect();
        for span in deletion_spans {
            assert_eq!(span.source, Some(LineSource::DeletedBase));
        }
    }

    #[test]
    fn test_deletion_source_for_staged_change() {
        let result = compute_inline_diff_merged("old content", "new content", LineSource::Staged);

        let deletion_spans: Vec<_> = result.spans.iter()
            .filter(|s| s.is_deletion)
            .collect();
        for span in deletion_spans {
            assert_eq!(span.source, Some(LineSource::DeletedCommitted));
        }
    }

    #[test]
    fn test_deletion_source_for_unstaged_change() {
        let result = compute_inline_diff_merged("old content", "new content", LineSource::Unstaged);

        let deletion_spans: Vec<_> = result.spans.iter()
            .filter(|s| s.is_deletion)
            .collect();
        for span in deletion_spans {
            assert_eq!(span.source, Some(LineSource::DeletedStaged));
        }
    }

    // === is_meaningful determination ===

    #[test]
    fn test_meaningful_when_long_unchanged_segment() {
        // "hello world" -> "hello earth" shares "hello " (6 chars) which is >= 5
        let result = compute_inline_diff_merged("hello world", "hello earth", LineSource::Committed);
        assert!(result.is_meaningful, "Should be meaningful with 6-char unchanged prefix");
    }

    #[test]
    fn test_meaningful_suffix_preservation() {
        // "do_thing(data)" -> "do_thing(data, params)" shares "do_thing(data" + ")"
        let result = compute_inline_diff_merged(
            "do_thing(data)",
            "do_thing(data, params)",
            LineSource::Committed,
        );
        assert!(result.is_meaningful, "Should be meaningful - prefix/suffix unchanged");
    }

    #[test]
    fn test_not_meaningful_too_short_unchanged() {
        // "abc" -> "xyz" - no meaningful unchanged segment
        let result = compute_inline_diff_merged("abc", "xyz", LineSource::Committed);
        assert!(!result.is_meaningful, "Should not be meaningful - no shared content");
    }

    #[test]
    fn test_not_meaningful_only_short_matches() {
        // Lines that share only small fragments shouldn't be meaningful
        let result = compute_inline_diff_merged("end", "let", LineSource::Committed);
        assert!(!result.is_meaningful, "Should not be meaningful - only 1 char match");
    }

    #[test]
    fn test_not_meaningful_too_fragmented() {
        // Many scattered unchanged segments indicate coincidental matches
        let result = compute_inline_diff_merged(
            "for i in (x..y).rev() {",
            "// the range span note",
            LineSource::Committed,
        );
        // This creates scattered matches like "r", " ", etc.
        assert!(!result.is_meaningful, "Should not be meaningful - too fragmented");
    }

    #[test]
    fn test_leading_whitespace_not_counted_for_meaningfulness() {
        // Two lines that only share leading whitespace shouldn't be meaningful
        let result = compute_inline_diff_merged(
            "            }",
            "            .map(|x| x)",
            LineSource::Committed,
        );
        // The 12 spaces of indentation shouldn't make this meaningful
        assert!(!result.is_meaningful, "Leading whitespace alone shouldn't make diff meaningful");
    }

    // === Span structure verification ===

    #[test]
    fn test_identical_lines_single_unchanged_span() {
        let result = compute_inline_diff_merged("same line", "same line", LineSource::Committed);

        assert_eq!(result.spans.len(), 1);
        assert_eq!(result.spans[0].text, "same line");
        assert!(result.spans[0].source.is_none());
        assert!(!result.spans[0].is_deletion);
    }

    #[test]
    fn test_empty_lines() {
        let result = compute_inline_diff_merged("", "", LineSource::Committed);

        // Empty lines should produce no spans
        assert!(result.spans.is_empty());
        assert!(!result.is_meaningful);
    }

    #[test]
    fn test_simple_replacement_structure() {
        // "commercial_renewal" -> "bond" with ".name" preserved
        let result = compute_inline_diff_merged(
            "commercial_renewal.name",
            "bond.name",
            LineSource::Committed,
        );

        // Should have deletion spans, insertion spans, and unchanged spans
        let has_deletions = result.spans.iter().any(|s| s.is_deletion);
        let has_insertions = result.spans.iter().any(|s| s.source == Some(LineSource::Committed));
        let has_unchanged = result.spans.iter().any(|s| s.source.is_none());

        assert!(has_deletions, "Should have deletion spans");
        assert!(has_insertions, "Should have insertion spans");
        assert!(has_unchanged, "Should have unchanged spans");

        // The unchanged portion should contain ".name"
        let unchanged: String = result.spans.iter()
            .filter(|s| s.source.is_none())
            .map(|s| s.text.as_str())
            .collect();
        assert!(unchanged.contains(".name"), "Should preserve '.name' as unchanged");

        // Deleted content should include material from "commercial_renewal"
        let deleted: String = result.spans.iter()
            .filter(|s| s.is_deletion)
            .map(|s| s.text.as_str())
            .collect();
        assert!(!deleted.is_empty(), "Should have deleted content");
    }

    #[test]
    fn test_prefix_change() {
        // Change only the prefix
        let result = compute_inline_diff_merged(
            "old_function_name()",
            "new_function_name()",
            LineSource::Committed,
        );

        // "old" -> "new" deletion/insertion, then "_function_name()" unchanged
        assert!(result.is_meaningful, "Should be meaningful - long unchanged suffix");

        let unchanged: String = result.spans.iter()
            .filter(|s| s.source.is_none())
            .map(|s| s.text.as_str())
            .collect();
        assert!(unchanged.contains("_function_name()"));
    }

    #[test]
    fn test_suffix_change() {
        // Change only the suffix
        let result = compute_inline_diff_merged(
            "function_name_old()",
            "function_name_new()",
            LineSource::Committed,
        );

        assert!(result.is_meaningful, "Should be meaningful - long unchanged prefix");

        let unchanged: String = result.spans.iter()
            .filter(|s| s.source.is_none())
            .map(|s| s.text.as_str())
            .collect();
        assert!(unchanged.contains("function_name_"));
    }

    #[test]
    fn test_middle_insertion() {
        // Insert in the middle
        let result = compute_inline_diff_merged(
            "hello world",
            "hello beautiful world",
            LineSource::Staged,
        );

        // Should have "hello ", insertion "beautiful ", "world"
        let unchanged_texts: Vec<_> = result.spans.iter()
            .filter(|s| s.source.is_none())
            .map(|s| s.text.as_str())
            .collect();

        let inserted_texts: Vec<_> = result.spans.iter()
            .filter(|s| s.source == Some(LineSource::Staged))
            .map(|s| s.text.as_str())
            .collect();

        assert!(unchanged_texts.contains(&"hello "));
        assert!(unchanged_texts.contains(&"world"));
        assert!(inserted_texts.iter().any(|t| t.contains("beautiful")));
    }

    #[test]
    fn test_complete_replacement_not_meaningful() {
        // Completely different lines
        let result = compute_inline_diff_merged(
            "func foo() { return 42; }",
            "struct Bar { x: i32, y: i32 }",
            LineSource::Committed,
        );

        assert!(!result.is_meaningful, "Completely different lines should not be meaningful");
    }

    // === Edge cases ===

    #[test]
    fn test_single_char_difference() {
        let result = compute_inline_diff_merged("test_a", "test_b", LineSource::Committed);

        // "test_" unchanged (5 chars - exactly at threshold)
        assert!(result.is_meaningful, "5 char unchanged segment should be meaningful");
    }

    #[test]
    fn test_whitespace_only_change() {
        let result = compute_inline_diff_merged(
            "let x = 1;",
            "let  x  =  1;",
            LineSource::Unstaged,
        );

        // Should show whitespace changes
        let has_insertion = result.spans.iter().any(|s| s.source.is_some() && !s.is_deletion);
        assert!(has_insertion, "Should detect whitespace insertions");
    }

    #[test]
    fn test_unicode_content() {
        let result = compute_inline_diff_merged(
            "hello こんにちは world",
            "hello 你好 world",
            LineSource::Committed,
        );

        // Should handle unicode properly
        let unchanged: String = result.spans.iter()
            .filter(|s| s.source.is_none())
            .map(|s| s.text.as_str())
            .collect();
        assert!(unchanged.contains("hello "));
        assert!(unchanged.contains(" world"));
    }

    #[test]
    fn test_deletion_before_insertion_ordering() {
        // Verify deleted content appears before inserted content in span order
        let result = compute_inline_diff_merged("old", "new", LineSource::Committed);

        // Find positions
        let deletion_pos = result.spans.iter().position(|s| s.is_deletion);
        let insertion_pos = result.spans.iter().position(|s| !s.is_deletion && s.source.is_some());

        if let (Some(del), Some(ins)) = (deletion_pos, insertion_pos) {
            assert!(del < ins, "Deletion should come before insertion");
        }
    }

    #[test]
    fn test_multiple_changes_in_line() {
        let result = compute_inline_diff_merged(
            "let x = foo(a, b);",
            "let y = bar(c, d);",
            LineSource::Committed,
        );

        // Multiple changes: x->y, foo->bar, a,b->c,d
        // But shared: "let ", " = ", "(", ", ", ");"
        let unchanged_count = result.spans.iter()
            .filter(|s| s.source.is_none())
            .count();
        assert!(unchanged_count >= 2, "Should have multiple unchanged segments");
    }
}
