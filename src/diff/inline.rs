use similar::{ChangeTag, DiffOp, TextDiff};

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

    // ASCII lines are one byte per char. Diffing the bytes skips the `&str`
    // token `TextDiff::from_chars` builds for every character, and uses the
    // same Myers-plus-compact script, so the highlights stay put.
    if old_line.is_ascii() && new_line.is_ascii() {
        let ops = similar::capture_diff_slices(
            similar::Algorithm::Myers,
            old_line.as_bytes(),
            new_line.as_bytes(),
        );
        return assemble(edits_from_ops(old_line, new_line, &ops), deletion_source, change_source);
    }
    let diff = TextDiff::from_chars(old_line, new_line);
    let edits: Vec<(ChangeTag, &str)> =
        diff.iter_all_changes().map(|change| (change.tag(), change.value())).collect();
    assemble(edits, deletion_source, change_source)
}

fn edits_from_ops<'a>(old: &'a str, new: &'a str, ops: &[DiffOp]) -> Vec<(ChangeTag, &'a str)> {
    let mut edits = Vec::with_capacity(ops.len());
    for op in ops {
        match *op {
            DiffOp::Equal { old_index, len, .. } => {
                edits.push((ChangeTag::Equal, &old[old_index..old_index + len]));
            }
            DiffOp::Delete { old_index, old_len, .. } => {
                edits.push((ChangeTag::Delete, &old[old_index..old_index + old_len]));
            }
            DiffOp::Insert { new_index, new_len, .. } => {
                edits.push((ChangeTag::Insert, &new[new_index..new_index + new_len]));
            }
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                edits.push((ChangeTag::Delete, &old[old_index..old_index + old_len]));
                edits.push((ChangeTag::Insert, &new[new_index..new_index + new_len]));
            }
        }
    }
    edits
}

fn assemble(
    edits: Vec<(ChangeTag, &str)>,
    deletion_source: LineSource,
    change_source: LineSource,
) -> InlineDiffResult {
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
        spans.push(InlineSpan { text: std::mem::take(pending), source: None, is_deletion: false });
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
        spans.push(InlineSpan { text: std::mem::take(pending), source: Some(source), is_deletion });
    };

    for (tag, text) in edits {
        match tag {
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
                        &mut pending_deleted, &mut spans, &mut seen_non_whitespace,
                        deletion_source, true,
                    );
                    flush_changed(
                        &mut pending_inserted, &mut spans, &mut seen_non_whitespace, change_source,
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
        &mut pending_deleted, &mut spans, &mut seen_non_whitespace, deletion_source, true,
    );
    flush_changed(
        &mut pending_inserted, &mut spans, &mut seen_non_whitespace, change_source, false,
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
    use proptest::prelude::*;

    use super::*;

    // === InlineSpan creation and properties ===

    #[test]
    fn test_inline_span_unchanged() {
        let span = InlineSpan { text: "unchanged".to_string(), source: None, is_deletion: false };
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
        let result =
            compute_inline_diff_merged("old content", "new content", LineSource::Committed);

        // Deleted spans should have DeletedBase source
        let deletion_spans: Vec<_> = result.spans.iter().filter(|s| s.is_deletion).collect();
        for span in deletion_spans {
            assert_eq!(span.source, Some(LineSource::DeletedBase));
        }
    }

    #[test]
    fn test_deletion_source_for_staged_change() {
        let result = compute_inline_diff_merged("old content", "new content", LineSource::Staged);

        let deletion_spans: Vec<_> = result.spans.iter().filter(|s| s.is_deletion).collect();
        for span in deletion_spans {
            assert_eq!(span.source, Some(LineSource::DeletedCommitted));
        }
    }

    #[test]
    fn test_deletion_source_for_unstaged_change() {
        let result = compute_inline_diff_merged("old content", "new content", LineSource::Unstaged);

        let deletion_spans: Vec<_> = result.spans.iter().filter(|s| s.is_deletion).collect();
        for span in deletion_spans {
            assert_eq!(span.source, Some(LineSource::DeletedStaged));
        }
    }

    // === is_meaningful determination ===

    #[test]
    fn test_meaningful_when_long_unchanged_segment() {
        // "hello world" -> "hello earth" shares "hello " (6 chars) which is >= 5
        let result =
            compute_inline_diff_merged("hello world", "hello earth", LineSource::Committed);
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
        let unchanged: String =
            result.spans.iter().filter(|s| s.source.is_none()).map(|s| s.text.as_str()).collect();
        assert!(unchanged.contains(".name"), "Should preserve '.name' as unchanged");

        // Deleted content should include material from "commercial_renewal"
        let deleted: String =
            result.spans.iter().filter(|s| s.is_deletion).map(|s| s.text.as_str()).collect();
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

        let unchanged: String =
            result.spans.iter().filter(|s| s.source.is_none()).map(|s| s.text.as_str()).collect();
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

        let unchanged: String =
            result.spans.iter().filter(|s| s.source.is_none()).map(|s| s.text.as_str()).collect();
        assert!(unchanged.contains("function_name_"));
    }

    #[test]
    fn test_middle_insertion() {
        // Insert in the middle
        let result =
            compute_inline_diff_merged("hello world", "hello beautiful world", LineSource::Staged);

        // Should have "hello ", insertion "beautiful ", "world"
        let unchanged_texts: Vec<_> =
            result.spans.iter().filter(|s| s.source.is_none()).map(|s| s.text.as_str()).collect();

        let inserted_texts: Vec<_> = result
            .spans
            .iter()
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
        let result =
            compute_inline_diff_merged("let x = 1;", "let  x  =  1;", LineSource::Unstaged);

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
        let unchanged: String =
            result.spans.iter().filter(|s| s.source.is_none()).map(|s| s.text.as_str()).collect();
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
        let unchanged_count = result.spans.iter().filter(|s| s.source.is_none()).count();
        assert!(unchanged_count >= 2, "Should have multiple unchanged segments");
    }

    /// The ASCII path must keep the character-diff highlights. A different edit
    /// script changes which characters are marked and whether the line stays inline.
    #[test]
    fn ascii_path_matches_char_diff_spans() {
        let pairs = [
            ("", ""),
            ("same line", "same line"),
            ("abc", "xyz"),
            ("end", "let"),
            ("old", "new"),
            ("hello world", "hello earth"),
            ("old content", "new content"),
            ("do_thing(data)", "do_thing(data, params)"),
            ("hello world", "hello beautiful world"),
            ("test_a", "test_b"),
            ("let x = 1;", "let  x  =  1;"),
            ("commercial_renewal.name", "bond.name"),
            ("let x = foo(a, b);", "let y = bar(c, d);"),
            ("for i in (x..y).rev() {", "// the range span note"),
            ("            }", "            .map(|x| x)"),
            (
                "    let old_var_0 = different_content_0;",
                "    let variable_0 = some_content_here_0;",
            ),
            (
                "abcdefghijklmnopqrstuvwxyz0123456789 abcdefghijklmnopqrstuvwxyz0123456789 abcdefghijklmnopqrstuvwxyz0123456789",
                "abcdefghijklmnopqrstuvwxyz0123456789 abcdefghijklmnopqrstuvwxyz0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
            ),
        ];
        for (old, new) in pairs {
            let live = compute_inline_diff_merged(old, new, LineSource::Committed);
            let expect = char_diff_spans(old, new, LineSource::Committed);
            assert_eq!(live.is_meaningful, expect.is_meaningful, "{old:?} -> {new:?}");
            assert_eq!(live.spans, expect.spans, "{old:?} -> {new:?}");
        }
    }

    /// The pre-ASCII assembler, still on `TextDiff::from_chars`, used as the oracle.
    fn char_diff_spans(
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
        let edits: Vec<(ChangeTag, &str)> =
            diff.iter_all_changes().map(|change| (change.tag(), change.value())).collect();
        assemble(edits, deletion_source, change_source)
    }

    /// A multi-byte character diffed as bytes would slice inside the character
    /// and panic, so a line pair that is not ASCII proves it took the char path.
    #[test]
    fn non_ascii_lines_take_the_char_path() {
        for (old, new) in [
            ("é", "è"),
            ("caf\u{e9} au lait", "caf\u{e8} au lait"),
            ("日本語のテキスト", "日本語のテクスト"),
            ("ascii only", "ascii only 😀"),
            ("a\u{301}b", "ab"),
            ("\u{feff}let x = 1;", "let x = 1;"),
        ] {
            for source in [LineSource::Committed, LineSource::Staged, LineSource::Unstaged] {
                let live = compute_inline_diff_merged(old, new, source);
                let expect = char_diff_spans(old, new, source);
                assert_eq!(live.is_meaningful, expect.is_meaningful, "{old:?} -> {new:?}");
                assert_eq!(live.spans, expect.spans, "{old:?} -> {new:?}");
            }
        }
    }

    /// Lines drawn from a small alphabet so repeats are common, with tabs and
    /// runs of spaces, up to well past the 100-token cutoff where
    /// `TextDiff::from_chars` switches to hashed tokens.
    fn ascii_line() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                4 => Just("a"),
                4 => Just("b"),
                2 => Just("c"),
                2 => Just(" "),
                1 => Just("\t"),
                1 => Just("  "),
                1 => Just("("),
                1 => Just(")"),
                1 => Just(","),
                1 => Just("_"),
                1 => Just("let "),
                1 => Just("x = "),
                1 => Just(";"),
                1 => Just("0123456789"),
            ],
            0..=40,
        )
        .prop_map(|pieces| pieces.concat())
    }

    fn ascii_pair() -> impl Strategy<Value = (String, String)> {
        prop_oneof![
            (ascii_line(), ascii_line()),
            // An edit of one line: the common case, and the one where Compact's
            // slides matter.
            ascii_line().prop_flat_map(|old| {
                let len = old.len();
                (
                    Just(old),
                    0..=len,
                    0..=len,
                    proptest::collection::vec(
                        prop_oneof![Just("a"), Just("b"), Just(" "), Just("\t"), Just("zz")],
                        0..=6,
                    ),
                )
                    .prop_map(|(old, i, j, insert)| {
                        let (start, end) = if i <= j { (i, j) } else { (j, i) };
                        let new = format!("{}{}{}", &old[..start], insert.concat(), &old[end..]);
                        (old, new)
                    })
            }),
        ]
    }

    struct HandSpan {
        text: &'static str,
        source: Option<LineSource>,
        deletion: bool,
    }

    struct HandCase {
        old: &'static str,
        new: &'static str,
        meaningful: bool,
        spans: Vec<HandSpan>,
    }

    fn hand(text: &'static str, source: Option<LineSource>, deletion: bool) -> HandSpan {
        HandSpan { text, source, deletion }
    }

    fn span(text: &str, source: Option<LineSource>, is_deletion: bool) -> InlineSpan {
        InlineSpan { text: text.to_string(), source, is_deletion }
    }

    fn ascii_hand_cases() -> Vec<HandCase> {
        let deleted = Some(LineSource::DeletedBase);
        let committed = Some(LineSource::Committed);
        vec![
            HandCase { old: "", new: "", meaningful: false, spans: vec![] },
            HandCase { old: "a", new: "a", meaningful: false, spans: vec![hand("a", None, false)] },
            HandCase {
                old: "a",
                new: "",
                meaningful: false,
                spans: vec![hand("a", deleted, true)],
            },
            HandCase {
                old: "",
                new: "b",
                meaningful: false,
                spans: vec![hand("b", committed, false)],
            },
            HandCase {
                old: "a",
                new: "b",
                meaningful: false,
                spans: vec![hand("a", deleted, true), hand("b", committed, false)],
            },
            HandCase {
                old: "ab",
                new: "ac",
                meaningful: false,
                spans: vec![
                    hand("a", None, false),
                    hand("b", deleted, true),
                    hand("c", committed, false),
                ],
            },
            HandCase {
                old: "hello",
                new: "hello!",
                meaningful: true,
                spans: vec![hand("hello", None, false), hand("!", committed, false)],
            },
            HandCase {
                old: "hello!",
                new: "hello",
                meaningful: true,
                spans: vec![hand("hello", None, false), hand("!", deleted, true)],
            },
            HandCase {
                old: "helloX",
                new: "helloY",
                meaningful: true,
                spans: vec![
                    hand("hello", None, false),
                    hand("X", deleted, true),
                    hand("Y", committed, false),
                ],
            },
            HandCase {
                old: "hello-a-b-c-world",
                new: "hello-X-Y-Z-world",
                meaningful: true,
                spans: vec![
                    hand("hello-", None, false),
                    hand("a", deleted, true),
                    hand("X", committed, false),
                    hand("-", None, false),
                    hand("b", deleted, true),
                    hand("Y", committed, false),
                    hand("-", None, false),
                    hand("c", deleted, true),
                    hand("Z", committed, false),
                    hand("-world", None, false),
                ],
            },
        ]
    }

    /// Every arm of `edits_from_ops`, the empty and single-byte inputs, and the
    /// two bounds in `assemble` (`>= 5` unchanged characters, `<= 4` segments).
    /// The expected spans are the edit script, not the char-path oracle. The
    /// oracle is checked as well, so a byte path that drifts from it fails too.
    #[test]
    fn byte_path_spans_match_hand_computed_edits() {
        let cases = ascii_hand_cases();
        for case in &cases {
            let live = compute_inline_diff_merged(case.old, case.new, LineSource::Committed);
            let spans: Vec<InlineSpan> =
                case.spans.iter().map(|s| span(s.text, s.source, s.deletion)).collect();
            assert_eq!(live.is_meaningful, case.meaningful, "{:?} -> {:?}", case.old, case.new);
            assert_eq!(live.spans, spans, "{:?} -> {:?}", case.old, case.new);
            let oracle = char_diff_spans(case.old, case.new, LineSource::Committed);
            assert_eq!(
                live.spans, oracle.spans,
                "byte path vs chars {:?} -> {:?}",
                case.old, case.new
            );
            assert_eq!(
                live.is_meaningful, oracle.is_meaningful,
                "{:?} -> {:?}",
                case.old, case.new
            );
        }

        let staged = compute_inline_diff_merged("hello", "hello!", LineSource::Staged);
        assert!(staged.is_meaningful);
        assert_eq!(
            staged.spans,
            vec![span("hello", None, false), span("!", Some(LineSource::Staged), false)]
        );
        let unstaged = compute_inline_diff_merged("a", "", LineSource::Unstaged);
        assert_eq!(unstaged.spans, vec![span("a", Some(LineSource::DeletedStaged), true)]);
    }

    /// Non-ASCII stays on the char path. `é` and `è` share the UTF-8 lead byte
    /// `0xC3`, so the byte path would slice inside the character.
    #[test]
    fn non_ascii_spans_match_hand_computed_characters() {
        let deleted = Some(LineSource::DeletedBase);
        let committed = Some(LineSource::Committed);
        let live = compute_inline_diff_merged("é", "è", LineSource::Committed);
        assert!(!live.is_meaningful);
        assert_eq!(live.spans, vec![span("é", deleted, true), span("è", committed, false)]);
        assert_eq!(live.spans, char_diff_spans("é", "è", LineSource::Committed).spans);

        let cafe = compute_inline_diff_merged("café", "cafe", LineSource::Committed);
        assert!(!cafe.is_meaningful);
        assert_eq!(
            cafe.spans,
            vec![span("caf", None, false), span("é", deleted, true), span("e", committed, false),]
        );

        assert_eq!(
            compute_inline_diff_merged("", "é", LineSource::Staged).spans,
            vec![span("é", Some(LineSource::Staged), false)]
        );
        assert_eq!(
            compute_inline_diff_merged("é", "", LineSource::Committed).spans,
            vec![span("é", deleted, true)]
        );
        assert_eq!(
            compute_inline_diff_merged("a", "é", LineSource::Unstaged).spans,
            vec![
                span("a", Some(LineSource::DeletedStaged), true),
                span("é", Some(LineSource::Unstaged), false),
            ]
        );
    }

    /// One side ASCII, so `&&` stays on the char path and `||` takes the byte
    /// path. Myers keeps the first `a` of `aaa` against `baé` as characters,
    /// and the last `a` as bytes.
    #[test]
    fn mixed_line_keeps_the_char_alignment() {
        let deleted = Some(LineSource::DeletedBase);
        let committed = Some(LineSource::Committed);
        let live = compute_inline_diff_merged("aaa", "baé", LineSource::Committed);
        assert!(!live.is_meaningful);
        assert_eq!(
            live.spans,
            vec![
                span("b", committed, false),
                span("a", None, false),
                span("aa", deleted, true),
                span("é", committed, false),
            ]
        );
        assert_eq!(live.spans, char_diff_spans("aaa", "baé", LineSource::Committed).spans);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        /// The byte path must produce the spans `TextDiff::from_chars` did,
        /// for every ASCII pair, including lines over 100 characters.
        #[test]
        fn ascii_byte_path_matches_char_diff_on_random_pairs((old, new) in ascii_pair()) {
            prop_assert!(old.is_ascii() && new.is_ascii());
            for source in [LineSource::Committed, LineSource::Staged, LineSource::Unstaged] {
                let live = compute_inline_diff_merged(&old, &new, source);
                let expect = char_diff_spans(&old, &new, source);
                prop_assert_eq!(live.is_meaningful, expect.is_meaningful, "{:?} -> {:?}", old, new);
                prop_assert_eq!(live.spans, expect.spans, "{:?} -> {:?}", old, new);
            }
        }
    }
}
