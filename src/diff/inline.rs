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
