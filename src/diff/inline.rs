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

    let mut pending_unchanged = String::new();
    let mut pending_deleted = String::new();
    let mut pending_inserted = String::new();

    for change in diff.iter_all_changes() {
        let text = change.value();
        match change.tag() {
            ChangeTag::Equal => {
                if !pending_deleted.is_empty() || !pending_inserted.is_empty() {
                    if !pending_unchanged.is_empty() {
                        let segment_len = pending_unchanged.chars().count();
                        max_unchanged_segment = max_unchanged_segment.max(segment_len);
                        num_unchanged_segments += 1;
                        spans.push(InlineSpan {
                            text: pending_unchanged.clone(),
                            source: None,
                            is_deletion: false
                        });
                        pending_unchanged.clear();
                    }
                    if !pending_deleted.is_empty() {
                        spans.push(InlineSpan {
                            text: pending_deleted.clone(),
                            source: Some(deletion_source),
                            is_deletion: true
                        });
                        pending_deleted.clear();
                    }
                    if !pending_inserted.is_empty() {
                        spans.push(InlineSpan {
                            text: pending_inserted.clone(),
                            source: Some(change_source),
                            is_deletion: false
                        });
                        pending_inserted.clear();
                    }
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
    if !pending_unchanged.is_empty() {
        let segment_len = pending_unchanged.chars().count();
        max_unchanged_segment = max_unchanged_segment.max(segment_len);
        num_unchanged_segments += 1;
        spans.push(InlineSpan {
            text: pending_unchanged,
            source: None,
            is_deletion: false
        });
    }
    if !pending_deleted.is_empty() {
        spans.push(InlineSpan {
            text: pending_deleted,
            source: Some(deletion_source),
            is_deletion: true
        });
    }
    if !pending_inserted.is_empty() {
        spans.push(InlineSpan {
            text: pending_inserted,
            source: Some(change_source),
            is_deletion: false
        });
    }

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
