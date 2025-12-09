use ratatui::text::Span;

use crate::diff::{InlineSpan, LineSource};
use super::colors::{line_style, line_style_with_highlight};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineChangeType {
    Mixed,
    PureDeletion,
    PureAddition,
    NoChange,
}

pub fn classify_inline_change(spans: &[InlineSpan]) -> InlineChangeType {
    let has_deletions = spans.iter().any(|s| s.is_deletion);
    let has_insertions = spans.iter().any(|s| !s.is_deletion && s.source.is_some());

    match (has_deletions, has_insertions) {
        (true, true) => InlineChangeType::Mixed,
        (true, false) => InlineChangeType::PureDeletion,
        (false, true) => InlineChangeType::PureAddition,
        (false, false) => InlineChangeType::NoChange,
    }
}

/// Fragmented = multiple scattered change regions separated by unchanged text.
/// e.g., "c[b]ommercial_renewal[d]" has 2 change regions (fragmented)
/// vs "do_thing(data[, params])" has 1 change region (not fragmented)
pub fn is_fragmented(spans: &[InlineSpan]) -> bool {
    if spans.len() < 4 {
        return false;
    }

    let mut change_regions = 0;
    let mut in_change_region = false;

    for span in spans {
        let is_changed = span.source.is_some();
        if is_changed && !in_change_region {
            change_regions += 1;
            in_change_region = true;
        } else if !is_changed {
            in_change_region = false;
        }
    }

    change_regions >= 2
}

/// Preserve spans that are substantial (5+ chars) or purely structural (whitespace/punctuation).
/// Short non-structural spans like single letters are likely coincidental matches.
fn should_preserve_as_prefix(s: &str) -> bool {
    if s.len() >= 5 {
        return true;
    }
    s.chars().all(|c| c.is_whitespace() || "(){}[]<>:;,\"'`.".contains(c))
}

fn should_preserve_as_suffix(s: &str) -> bool {
    if s.len() >= 5 {
        return true;
    }
    s.chars().all(|c| c.is_whitespace() || "(){}[]<>:;,\"'`.".contains(c))
}

/// Coalesce fragmented inline spans into cleaner word-based representation.
/// Preserves structural prefix/suffix but merges scattered char-level matches.
pub fn coalesce_spans(spans: &[InlineSpan]) -> Vec<InlineSpan> {
    if !is_fragmented(spans) {
        return spans.to_vec();
    }

    let first_changed = spans.iter().position(|s| s.source.is_some());
    let last_changed = spans.iter().rposition(|s| s.source.is_some());

    let (first_changed, last_changed) = match (first_changed, last_changed) {
        (Some(f), Some(l)) => (f, l),
        _ => return spans.to_vec(),
    };

    let mut result = Vec::new();

    let mut prefix_end = 0;
    for (i, span) in spans[..first_changed].iter().enumerate() {
        if should_preserve_as_prefix(&span.text) {
            result.push(span.clone());
            prefix_end = i + 1;
        } else {
            break;
        }
    }

    let mut suffix_start = spans.len();

    // Always preserve the final unchanged span if it exists - it's the common suffix
    // For internal gaps, be more selective about what to preserve
    if last_changed + 1 < spans.len() {
        // There's at least one unchanged span after the last change
        // Always preserve the very last span (true line ending)
        let last_idx = spans.len() - 1;

        // Work backwards from second-to-last, requiring structural chars for internal gaps
        for i in (last_changed + 1..last_idx).rev() {
            if should_preserve_as_suffix(&spans[i].text) {
                suffix_start = i;
            } else {
                break;
            }
        }

        // Always include the final span as suffix
        if suffix_start > last_idx {
            suffix_start = last_idx;
        }
    }

    let coalesce_start = prefix_end;
    let coalesce_end = suffix_start;

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
            old_text.push_str(&span.text);
            new_text.push_str(&span.text);
        }
    }

    if !old_text.is_empty() && old_text != new_text {
        result.push(InlineSpan {
            text: old_text,
            source: deletion_source,
            is_deletion: true,
        });
    }

    if !new_text.is_empty() {
        // Infer insertion source from deletion source if needed
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

    for span in &spans[suffix_start..] {
        result.push(span.clone());
    }

    result
}

pub fn inline_display_width(spans: &[InlineSpan]) -> usize {
    coalesce_spans(spans).iter().map(|s| s.text.len()).sum()
}

pub fn get_deletion_source(spans: &[InlineSpan]) -> LineSource {
    spans.iter()
        .find(|s| s.is_deletion && s.source.is_some())
        .and_then(|s| s.source)
        .unwrap_or(LineSource::DeletedBase)
}

pub fn get_insertion_source(spans: &[InlineSpan]) -> LineSource {
    spans.iter()
        .find(|s| !s.is_deletion && s.source.is_some())
        .and_then(|s| s.source)
        .unwrap_or(LineSource::Committed)
}

/// Build spans for the deletion line: unchanged portions get base style, deleted portions get highlight.
pub fn build_deletion_spans_with_highlight(
    inline_spans: &[InlineSpan],
    del_source: LineSource,
) -> Vec<Span<'static>> {
    let base_style = line_style(del_source);
    let highlight_style = line_style_with_highlight(del_source);

    let coalesced = coalesce_spans(inline_spans);
    let mut result = Vec::new();
    for span in coalesced {
        if span.is_deletion {
            result.push(Span::styled(span.text.clone(), highlight_style));
        } else if span.source.is_none() {
            result.push(Span::styled(span.text.clone(), base_style));
        }
    }
    result
}

/// Build spans for the insertion line: unchanged portions get base style, inserted portions get highlight.
pub fn build_insertion_spans_with_highlight(
    inline_spans: &[InlineSpan],
    ins_source: LineSource,
) -> Vec<Span<'static>> {
    let base_style = line_style(ins_source);
    let highlight_style = line_style_with_highlight(ins_source);

    let coalesced = coalesce_spans(inline_spans);
    let mut result = Vec::new();
    for span in coalesced {
        if span.is_deletion {
            continue;
        } else if span.source.is_some() {
            result.push(Span::styled(span.text.clone(), highlight_style));
        } else {
            result.push(Span::styled(span.text.clone(), base_style));
        }
    }
    result
}
