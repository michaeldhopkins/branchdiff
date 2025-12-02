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
    // - There must be a contiguous unchanged segment of meaningful length
    // - This prevents false positives from scattered single-char matches
    //
    // We require: longest contiguous unchanged segment >= 5 chars
    // This accepts: "do_thing(data)" -> "do_thing(data, params)" (14 char segment)
    // This accepts: "hello world" -> "hello earth" (6 char segment "hello ")
    // This rejects: "body_line" -> "new body" (4 char segment "body")
    let is_meaningful = max_unchanged_segment >= 5;

    InlineDiffResult { spans, is_meaningful }
}
