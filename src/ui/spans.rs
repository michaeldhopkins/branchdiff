use crate::diff::{InlineSpan, LineSource};

/// Check if the inline spans are fragmented (multiple scattered change regions)
pub fn is_fragmented(spans: &[InlineSpan]) -> bool {
    if spans.len() < 4 {
        // Need at least 4 spans to have scattered changes
        // (e.g., unchanged, change, unchanged, change)
        return false;
    }

    // Count transitions between unchanged and changed regions
    // A clean diff has at most one "change region" (possibly with both deletion and insertion)
    // A fragmented diff has multiple separate change regions
    let mut change_regions = 0;
    let mut in_change_region = false;

    for span in spans {
        let is_changed = span.source.is_some();
        if is_changed && !in_change_region {
            // Entering a new change region
            change_regions += 1;
            in_change_region = true;
        } else if !is_changed {
            // Exiting change region (if we were in one)
            in_change_region = false;
        }
    }

    // Fragmented if we have multiple separate change regions
    // e.g., "c[b]ommercial_renewal[d]" has 2 change regions
    // vs "do_thing(data[, params])" has 1 change region
    change_regions >= 2
}

/// Check if a span should be preserved as a prefix (not coalesced).
/// We preserve it if it's substantial context (5+ chars) or ends with structural characters.
fn should_preserve_as_prefix(s: &str) -> bool {
    if s.len() >= 5 {
        // Long enough to be meaningful context
        return true;
    }
    // Short spans: only preserve if entirely structural (whitespace/punctuation)
    s.chars().all(|c| c.is_whitespace() || "(){}[]<>:;,\"'`.".contains(c))
}

/// Check if a span should be preserved as a suffix (not coalesced).
fn should_preserve_as_suffix(s: &str) -> bool {
    if s.len() >= 5 {
        // Long enough to be meaningful context
        return true;
    }
    // Short spans: only preserve if entirely structural (whitespace/punctuation)
    s.chars().all(|c| c.is_whitespace() || "(){}[]<>:;,\"'`.".contains(c))
}

/// Coalesce fragmented inline spans into cleaner word-based representation.
/// Only coalesces the fragmented middle portion, preserving unchanged prefix and suffix
/// if they look structural (whitespace, punctuation) rather than coincidental char matches.
pub fn coalesce_spans(spans: &[InlineSpan]) -> Vec<InlineSpan> {
    if !is_fragmented(spans) {
        return spans.to_vec();
    }

    // Find the first and last changed spans to identify the fragmented region
    let first_changed = spans.iter().position(|s| s.source.is_some());
    let last_changed = spans.iter().rposition(|s| s.source.is_some());

    let (first_changed, last_changed) = match (first_changed, last_changed) {
        (Some(f), Some(l)) => (f, l),
        _ => return spans.to_vec(), // No changes, return as-is
    };

    let mut result = Vec::new();

    // Add unchanged prefix spans (before first change) - but only if they're substantial
    // Single letters like "c" that happen to match are likely coincidental and should
    // be included in the coalesced region
    let mut prefix_end = 0;
    for (i, span) in spans[..first_changed].iter().enumerate() {
        if should_preserve_as_prefix(&span.text) {
            result.push(span.clone());
            prefix_end = i + 1;
        } else {
            // Small non-structural unchanged span - stop here, include rest in coalesced region
            break;
        }
    }

    // Find suffix spans (after last change) that should be preserved - working backwards
    let mut suffix_start = spans.len();
    for i in (last_changed + 1..spans.len()).rev() {
        if should_preserve_as_suffix(&spans[i].text) {
            suffix_start = i;
        } else {
            // Small non-structural - stop here, include this and everything before in coalesced region
            break;
        }
    }

    // Coalesce the middle (fragmented) portion, including non-structural prefix/suffix spans
    let coalesce_start = prefix_end;
    let coalesce_end = suffix_start;

    // Reconstruct the OLD text and NEW text for the coalesced portion
    let mut old_text = String::new();
    let mut new_text = String::new();
    let mut deletion_source: Option<LineSource> = None;
    let mut insertion_source: Option<LineSource> = None;

    for span in &spans[coalesce_start..coalesce_end] {
        if span.is_deletion {
            old_text.push_str(&span.text);
            if deletion_source.is_none() {
                deletion_source = span.source;
            }
        } else if span.source.is_some() {
            new_text.push_str(&span.text);
            if insertion_source.is_none() {
                insertion_source = span.source;
            }
        } else {
            // Unchanged in the middle - include in both
            old_text.push_str(&span.text);
            new_text.push_str(&span.text);
        }
    }

    // Add the coalesced deletion (if different from insertion)
    if !old_text.is_empty() && old_text != new_text {
        result.push(InlineSpan {
            text: old_text,
            source: deletion_source,
            is_deletion: true,
        });
    }

    // Add the coalesced insertion
    // If there were no explicit insertions but we have new_text (from unchanged spans),
    // we need to infer the insertion source from the deletion source
    if !new_text.is_empty() {
        // If no insertion source was found, infer it from the deletion source
        // DeletedBase -> Committed, DeletedCommitted -> Staged, DeletedStaged -> Unstaged
        let effective_insertion_source = insertion_source.or_else(|| {
            deletion_source.and_then(|ds| match ds {
                LineSource::DeletedBase => Some(LineSource::Committed),
                LineSource::DeletedCommitted => Some(LineSource::Staged),
                LineSource::DeletedStaged => Some(LineSource::Unstaged),
                _ => None,
            })
        });

        result.push(InlineSpan {
            text: new_text,
            source: effective_insertion_source,
            is_deletion: false,
        });
    }

    // Add structural suffix spans
    for span in &spans[suffix_start..] {
        result.push(span.clone());
    }

    result
}

/// Calculate display width of inline diff (using coalesced spans)
pub fn inline_display_width(spans: &[InlineSpan]) -> usize {
    coalesce_spans(spans).iter().map(|s| s.text.len()).sum()
}

/// Reconstruct old content from inline spans (unchanged + deletions)
pub fn reconstruct_old_content(spans: &[InlineSpan]) -> String {
    spans.iter()
        .filter(|s| s.is_deletion || s.source.is_none())
        .map(|s| s.text.as_str())
        .collect()
}

/// Get deletion source for coloring the - line
pub fn get_deletion_source(spans: &[InlineSpan]) -> LineSource {
    spans.iter()
        .find(|s| s.is_deletion && s.source.is_some())
        .and_then(|s| s.source)
        .unwrap_or(LineSource::DeletedBase)
}

/// Get insertion source for coloring the + line
pub fn get_insertion_source(spans: &[InlineSpan]) -> LineSource {
    spans.iter()
        .find(|s| !s.is_deletion && s.source.is_some())
        .and_then(|s| s.source)
        .unwrap_or(LineSource::Committed)
}
