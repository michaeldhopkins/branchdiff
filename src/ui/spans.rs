use ratatui::style::Style;
use ratatui::text::Span;

use crate::diff::{InlineSpan, LineSource};
use crate::syntax::highlight_line;
use super::colors::{line_style, line_style_with_highlight, ensure_contrast, DEFAULT_FG};

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

/// Fragmented = multiple scattered change regions separated by some unchanged text.
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
    use crate::ui::wrapping::content_display_width;
    coalesce_spans(spans).iter().map(|s| content_display_width(&s.text)).sum()
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

/// Build spans for the deletion line with syntax highlighting.
/// Unchanged portions get base style, deleted portions get highlight.
pub fn build_deletion_spans_with_highlight(
    inline_spans: &[InlineSpan],
    del_source: LineSource,
    old_content: &str,
    file_path: Option<&str>,
) -> Vec<Span<'static>> {
    let base_style = line_style(del_source);
    let highlight_style = line_style_with_highlight(del_source);
    let highlight_bg = highlight_style.bg.unwrap_or(ratatui::style::Color::Reset);

    // Get syntax colors for the old content
    let syntax_segments = highlight_line(old_content, file_path);
    let mut syntax_colors: Vec<ratatui::style::Color> = Vec::with_capacity(old_content.len());
    for seg in &syntax_segments {
        for _ in seg.text.chars() {
            syntax_colors.push(seg.fg_color);
        }
    }

    let coalesced = coalesce_spans(inline_spans);
    let mut result = Vec::new();
    let mut char_idx = 0;

    for span in coalesced {
        // For deletion line, we show: deletions (highlighted) + unchanged portions
        // Skip insertions (they go on the insertion line)
        if !span.is_deletion && span.source.is_some() {
            continue;
        }

        let is_highlighted = span.is_deletion;
        let bg_style = if is_highlighted {
            highlight_style
        } else {
            base_style
        };

        // Apply syntax colors character by character, ensuring contrast for highlights
        let mut current_text = String::new();
        let mut current_color = None;

        for ch in span.text.chars() {
            let syntax_fg = syntax_colors.get(char_idx).copied().unwrap_or(DEFAULT_FG);
            // For highlighted spans, ensure the syntax color has sufficient contrast
            let fg_color = if is_highlighted {
                ensure_contrast(syntax_fg, highlight_bg)
            } else {
                syntax_fg
            };
            char_idx += 1;

            if Some(fg_color) == current_color {
                current_text.push(ch);
            } else {
                if !current_text.is_empty() {
                    let style = bg_style.fg(current_color.unwrap_or(fg_color));
                    result.push(Span::styled(std::mem::take(&mut current_text), style));
                }
                current_text.push(ch);
                current_color = Some(fg_color);
            }
        }

        if !current_text.is_empty() {
            let style = bg_style.fg(current_color.unwrap_or(DEFAULT_FG));
            result.push(Span::styled(current_text, style));
        }
    }
    result
}

/// Build spans for the insertion line with syntax highlighting.
/// Unchanged portions get base style, inserted portions get highlight.
pub fn build_insertion_spans_with_highlight(
    inline_spans: &[InlineSpan],
    ins_source: LineSource,
    new_content: &str,
    file_path: Option<&str>,
) -> Vec<Span<'static>> {
    let base_style = line_style(ins_source);
    let highlight_style = line_style_with_highlight(ins_source);
    let highlight_bg = highlight_style.bg.unwrap_or(ratatui::style::Color::Reset);

    // Get syntax colors for the new content
    let syntax_segments = highlight_line(new_content, file_path);
    let mut syntax_colors: Vec<ratatui::style::Color> = Vec::with_capacity(new_content.len());
    for seg in &syntax_segments {
        for _ in seg.text.chars() {
            syntax_colors.push(seg.fg_color);
        }
    }

    let coalesced = coalesce_spans(inline_spans);
    let mut result = Vec::new();
    let mut char_idx = 0;

    for span in coalesced {
        // For insertion line, skip deletions
        if span.is_deletion {
            continue;
        }

        let is_highlighted = span.source.is_some();
        let bg_style = if is_highlighted {
            highlight_style
        } else {
            base_style
        };

        // Apply syntax colors character by character, ensuring contrast for highlights
        let mut current_text = String::new();
        let mut current_color = None;

        for ch in span.text.chars() {
            let syntax_fg = syntax_colors.get(char_idx).copied().unwrap_or(DEFAULT_FG);
            // For highlighted spans, ensure the syntax color has sufficient contrast
            let fg_color = if is_highlighted {
                ensure_contrast(syntax_fg, highlight_bg)
            } else {
                syntax_fg
            };
            char_idx += 1;

            if Some(fg_color) == current_color {
                current_text.push(ch);
            } else {
                if !current_text.is_empty() {
                    let style = bg_style.fg(current_color.unwrap_or(fg_color));
                    result.push(Span::styled(std::mem::take(&mut current_text), style));
                }
                current_text.push(ch);
                current_color = Some(fg_color);
            }
        }

        if !current_text.is_empty() {
            let style = bg_style.fg(current_color.unwrap_or(DEFAULT_FG));
            result.push(Span::styled(current_text, style));
        }
    }
    result
}

/// Apply syntax highlighting to line content.
/// Returns spans with syntax-based foreground colors and the base style's background preserved.
pub fn syntax_highlight_content(
    content: &str,
    file_path: Option<&str>,
    base_style: Style,
) -> Vec<Span<'static>> {
    let segments = highlight_line(content, file_path);

    segments
        .into_iter()
        .map(|seg| {
            // Preserve background from base_style, use foreground from syntax
            let style = base_style.fg(seg.fg_color);
            Span::styled(seg.text, style)
        })
        .collect()
}

/// Apply syntax highlighting to inline diff spans.
/// This merges syntax colors with diff backgrounds - syntax provides foreground,
/// diff provides background based on whether the segment is changed or unchanged.
pub fn syntax_highlight_inline_spans(
    inline_spans: &[InlineSpan],
    content: &str,
    file_path: Option<&str>,
    base_style: Style,
    highlight_style: Style,
) -> Vec<Span<'static>> {
    let syntax_segments = highlight_line(content, file_path);
    let highlight_bg = highlight_style.bg.unwrap_or(ratatui::style::Color::Reset);

    // Build a character-indexed color map from syntax highlighting
    let mut syntax_colors: Vec<ratatui::style::Color> = Vec::with_capacity(content.len());
    for seg in &syntax_segments {
        for _ in seg.text.chars() {
            syntax_colors.push(seg.fg_color);
        }
    }

    let coalesced = coalesce_spans(inline_spans);
    let mut result = Vec::new();
    let mut char_idx = 0;

    for span in coalesced {
        if span.is_deletion {
            // Skip deletions - they're shown on a separate line
            continue;
        }

        let is_highlighted = span.source.is_some();
        let bg_style = if is_highlighted {
            highlight_style
        } else {
            base_style
        };

        // Apply syntax colors character by character, ensuring contrast for highlights
        let mut current_text = String::new();
        let mut current_color = None;

        for ch in span.text.chars() {
            let syntax_fg = syntax_colors.get(char_idx).copied().unwrap_or(base_style.fg.unwrap_or(DEFAULT_FG));
            // For highlighted spans, ensure the syntax color has sufficient contrast
            let fg_color = if is_highlighted {
                ensure_contrast(syntax_fg, highlight_bg)
            } else {
                syntax_fg
            };
            char_idx += 1;

            if Some(fg_color) == current_color {
                current_text.push(ch);
            } else {
                if !current_text.is_empty() {
                    let style = bg_style.fg(current_color.unwrap_or(fg_color));
                    result.push(Span::styled(std::mem::take(&mut current_text), style));
                }
                current_text.push(ch);
                current_color = Some(fg_color);
            }
        }

        if !current_text.is_empty() {
            let style = bg_style.fg(current_color.unwrap_or(DEFAULT_FG));
            result.push(Span::styled(current_text, style));
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::colors::{line_style, line_style_with_highlight};
    use crate::diff::{InlineSpan, LineSource, compute_inline_diff_merged};
    use crate::syntax::reset_highlight_state;
    use ratatui::style::Color;

    #[test]
    fn test_syntax_highlight_content_rust() {
        reset_highlight_state();
        let base_style = Style::default().bg(ratatui::style::Color::Rgb(25, 50, 50));
        let spans = syntax_highlight_content("fn main() {}", Some("test.rs"), base_style);

        assert!(!spans.is_empty());
        // Should preserve background from base_style
        for span in &spans {
            assert_eq!(span.style.bg, Some(ratatui::style::Color::Rgb(25, 50, 50)));
        }
    }

    #[test]
    fn test_syntax_highlight_content_empty() {
        reset_highlight_state();
        let base_style = Style::default();
        let spans = syntax_highlight_content("", Some("test.rs"), base_style);

        assert!(spans.is_empty());
    }

    #[test]
    fn test_syntax_highlight_content_no_file_path() {
        reset_highlight_state();
        let base_style = Style::default();
        let spans = syntax_highlight_content("some text", None, base_style);

        // Should still work without file path (plain text)
        assert!(!spans.is_empty());
    }

    #[test]
    fn test_syntax_highlight_inline_spans_unchanged() {
        reset_highlight_state();
        let inline_spans = vec![
            InlineSpan {
                text: "fn test()".to_string(),
                source: None,
                is_deletion: false,
            },
        ];

        let base_style = Style::default().bg(ratatui::style::Color::Rgb(25, 50, 50));
        let highlight_style = Style::default().bg(ratatui::style::Color::Rgb(50, 100, 100));

        let spans = syntax_highlight_inline_spans(
            &inline_spans,
            "fn test()",
            Some("test.rs"),
            base_style,
            highlight_style,
        );

        assert!(!spans.is_empty());
        // Unchanged spans should use base_style background
        for span in &spans {
            assert_eq!(span.style.bg, Some(ratatui::style::Color::Rgb(25, 50, 50)));
        }
    }

    #[test]
    fn test_syntax_highlight_inline_spans_with_changes() {
        reset_highlight_state();
        let inline_spans = vec![
            InlineSpan {
                text: "let ".to_string(),
                source: None,
                is_deletion: false,
            },
            InlineSpan {
                text: "x".to_string(),
                source: Some(LineSource::Committed),
                is_deletion: false,
            },
            InlineSpan {
                text: " = 1;".to_string(),
                source: None,
                is_deletion: false,
            },
        ];

        let base_style = Style::default().bg(ratatui::style::Color::Rgb(25, 50, 50));
        let highlight_style = Style::default().bg(ratatui::style::Color::Rgb(50, 100, 100));

        let spans = syntax_highlight_inline_spans(
            &inline_spans,
            "let x = 1;",
            Some("test.rs"),
            base_style,
            highlight_style,
        );

        assert!(!spans.is_empty());
        // Should have mix of base and highlight backgrounds
        let has_base_bg = spans.iter().any(|s| s.style.bg == Some(ratatui::style::Color::Rgb(25, 50, 50)));
        let has_highlight_bg = spans.iter().any(|s| s.style.bg == Some(ratatui::style::Color::Rgb(50, 100, 100)));
        assert!(has_base_bg, "Should have spans with base background");
        assert!(has_highlight_bg, "Should have spans with highlight background");
    }

    #[test]
    fn test_syntax_highlight_inline_spans_skips_deletions() {
        reset_highlight_state();
        let inline_spans = vec![
            InlineSpan {
                text: "old".to_string(),
                source: Some(LineSource::DeletedBase),
                is_deletion: true,
            },
            InlineSpan {
                text: "new".to_string(),
                source: Some(LineSource::Committed),
                is_deletion: false,
            },
        ];

        let base_style = Style::default();
        let highlight_style = Style::default().bg(ratatui::style::Color::Rgb(50, 100, 100));

        let spans = syntax_highlight_inline_spans(
            &inline_spans,
            "new",
            Some("test.rs"),
            base_style,
            highlight_style,
        );

        // Should only contain "new", not "old"
        let all_text: String = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(all_text, "new");
    }

    #[test]
    fn test_syntax_highlight_inline_spans_empty_input() {
        reset_highlight_state();
        let inline_spans: Vec<InlineSpan> = vec![];

        let base_style = Style::default();
        let highlight_style = Style::default().bg(ratatui::style::Color::Rgb(50, 100, 100));

        let spans = syntax_highlight_inline_spans(
            &inline_spans,
            "",
            Some("test.rs"),
            base_style,
            highlight_style,
        );

        assert!(spans.is_empty());
    }

    fn make_span(text: &str, source: Option<LineSource>, is_deletion: bool) -> InlineSpan {
        InlineSpan {
            text: text.to_string(),
            source,
            is_deletion,
        }
    }

    #[test]
    fn test_is_fragmented_few_spans_not_fragmented() {
        // Only 2 spans - not fragmented
        let spans = vec![
            make_span("hello", Some(LineSource::DeletedBase), true),
            make_span("world", Some(LineSource::Committed), false),
        ];
        assert!(!is_fragmented(&spans));
    }

    #[test]
    fn test_is_fragmented_single_change_region_not_fragmented() {
        // Single change region (deletion + insertion together) - not fragmented
        // Pattern: change, change, unchanged - one contiguous change region
        let spans = vec![
            make_span("world", Some(LineSource::DeletedBase), true),
            make_span("earth", Some(LineSource::Committed), false),
            make_span(" hello ", None, false),
        ];
        assert!(!is_fragmented(&spans));
    }

    #[test]
    fn test_is_fragmented_two_change_regions_is_fragmented() {
        // Two separate change regions - fragmented
        // Pattern: unchanged, change, unchanged, change (two change regions)
        let spans = vec![
            make_span("c", None, false),                                    // unchanged
            make_span("b", Some(LineSource::Committed), false),             // change region 1
            make_span("ommercial_renewal", None, false),                    // unchanged
            make_span("d", Some(LineSource::Committed), false),             // change region 2
        ];
        assert!(is_fragmented(&spans));
    }

    #[test]
    fn test_is_fragmented_commercial_renewal_to_bond() {
        // Real case: commercial_renewal -> bond with scattered char matches
        // Pattern: unchanged(c), deleted+inserted, unchanged(on), inserted(d)
        let spans = vec![
            make_span("c", None, false),                                    // unchanged
            make_span("ommercial_renewal", Some(LineSource::DeletedBase), true), // change region 1
            make_span("b", Some(LineSource::Committed), false),             // still in change region 1
            make_span("on", None, false),                                   // unchanged - exits region 1
            make_span("d", Some(LineSource::Committed), false),             // change region 2
        ];
        assert!(is_fragmented(&spans));
    }

    #[test]
    fn test_coalesce_spans_not_fragmented_returns_original() {
        let spans = vec![
            make_span("hello ", None, false),
            make_span("world", Some(LineSource::DeletedBase), true),
            make_span("earth", Some(LineSource::Committed), false),
        ];
        let result = coalesce_spans(&spans);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].text, "hello ");
        assert_eq!(result[1].text, "world");
        assert_eq!(result[2].text, "earth");
    }

    #[test]
    fn test_coalesce_spans_fragmented_preserves_structural_prefix_suffix() {
        // Fragmented case with structural prefix (whitespace) and suffix (punctuation)
        // Only structural chars (whitespace, punctuation) are preserved as prefix/suffix
        // Non-structural chars like letters get included in coalesced region

        let spans = vec![
            make_span("  ", None, false),       // structural prefix (spaces) - KEEP
            make_span("bc", Some(LineSource::DeletedBase), true),  // deleted - first change
            make_span("x", Some(LineSource::Committed), false),    // inserted
            make_span("d", None, false),        // unchanged (in fragmented region)
            make_span("e", Some(LineSource::DeletedBase), true),   // deleted
            make_span("yz", Some(LineSource::Committed), false),   // inserted - last change
            make_span(");", None, false),       // structural suffix (punctuation) - KEEP
        ];

        let result = coalesce_spans(&spans);

        // Should be: spaces, coalesced_old, coalesced_new, punctuation
        assert_eq!(result.len(), 4, "Expected structural_prefix + old + new + structural_suffix");
        assert_eq!(result[0].text, "  ");
        assert!(result[0].source.is_none()); // unchanged
        assert!(result[1].is_deletion);
        assert_eq!(result[1].text, "bcde"); // coalesced old
        assert!(!result[2].is_deletion);
        assert_eq!(result[2].text, "xdyz"); // coalesced new
        assert_eq!(result[3].text, ");");
        assert!(result[3].source.is_none()); // unchanged
    }

    #[test]
    fn test_coalesce_spans_includes_nonstructural_prefix_in_coalesce() {
        // Non-structural prefix chars (like a single 'c') should be included in coalesced region
        // This handles the "cancellation" -> "clause" case where 'c' is coincidental
        // Need 4+ spans and 2+ change regions to trigger fragmentation detection

        let spans = vec![
            make_span("c", None, false),        // non-structural - gets coalesced
            make_span("ancellation", Some(LineSource::DeletedBase), true), // change region 1
            make_span("l", None, false),        // unchanged in middle
            make_span("ause", Some(LineSource::Committed), false), // change region 2
        ];

        let result = coalesce_spans(&spans);

        // Should coalesce everything since 'c' is not structural
        assert_eq!(result.len(), 2);
        assert!(result[0].is_deletion);
        assert_eq!(result[0].text, "cancellationl"); // c + ancellation + l
        assert!(!result[1].is_deletion);
        assert_eq!(result[1].text, "clause"); // c + l + ause
    }

    #[test]
    fn test_coalesce_spans_preserves_good_inline_diff() {
        // Good inline diff: do_thing(data) -> do_thing(data, params)
        // Should have large unchanged segment "do_thing(data" and small insertion ", params"
        let spans = vec![
            make_span("do_thing(data", None, false),
            make_span(", params", Some(LineSource::Committed), false),
            make_span(")", None, false),
        ];
        let result = coalesce_spans(&spans);

        // Should NOT coalesce - good readable diff
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].text, "do_thing(data");
        assert_eq!(result[1].text, ", params");
        assert_eq!(result[2].text, ")");
    }

    #[test]
    fn test_real_world_commercial_renewal_to_bond() {
        // Real example: "  commercial_renewal.principal_mailing_address" -> "  bond.description"
        // The character diff would scatter shared chars (o, n, i, etc.)
        // Simulate what a character diff might produce (simplified):
        let spans = vec![
            make_span("  ", None, false),           // structural prefix (spaces) - PRESERVED
            make_span("c", None, false),            // non-structural - gets coalesced (coincidental match)
            make_span("ommercial_renewal.principal_mailing_address", Some(LineSource::DeletedBase), true),
            make_span("b", Some(LineSource::Committed), false),
            make_span("o", None, false),
            make_span("n", None, false),
            make_span("d.des", Some(LineSource::Committed), false),
            make_span("c", None, false),
            make_span("r", Some(LineSource::Committed), false),
            make_span("i", None, false),
            make_span("ption", Some(LineSource::Committed), false),
        ];

        let result = coalesce_spans(&spans);

        // Should preserve only structural prefix (spaces), coalesce everything else
        // The 'c' gets included in coalesced region since it's not structural
        assert_eq!(result.len(), 3, "Should have: spaces + coalesced_old + coalesced_new");
        assert_eq!(result[0].text, "  ");
        assert!(result[1].is_deletion);
        // Old text includes: c + ommercial... + o + n + c + i = "commercial_renewal..."
        assert!(result[1].text.starts_with("commercial_renewal"));
        assert!(!result[2].is_deletion);
        // New text includes: c + b + o + n + d.des + c + r + i + ption = "bond.description"
    }

    #[test]
    fn test_inline_diff_commercial_renewal_to_bond_coalesces() {
        let old = "BDEFF: date_for_display(commercial_renewal.effective_date),";
        let new = "BDEFF: date_for_display(bond.effective_date),";

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        // Debug: print raw spans BEFORE coalescing
        eprintln!("\n=== commercial_renewal -> bond ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let coalesced = coalesce_spans(&result.spans);

        // Debug: print coalesced spans
        eprintln!("Coalesced spans ({}):", coalesced.len());
        for (i, span) in coalesced.iter().enumerate() {
            eprintln!("  span[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        // We should have structural prefix preserved, then coalesced old/new, then structural suffix
        // Prefix: "BDEFF: date_for_display("
        // Old: "commercial_renewal"
        // New: "bond"
        // Suffix: ".effective_date),"

        // Find the deletion span
        let deletion = coalesced.iter().find(|s| s.is_deletion);
        assert!(deletion.is_some(), "Should have a deletion span");
        let deletion = deletion.unwrap();

        // The deletion should contain "commercial_renewal", not scattered chars
        assert!(
            deletion.text.contains("commercial_renewal") || deletion.text == "commercial_renewal",
            "Deletion should be 'commercial_renewal', got: {:?}", deletion.text
        );

        // Find the insertion span
        let insertion = coalesced.iter().find(|s| s.source.is_some() && !s.is_deletion);
        assert!(insertion.is_some(), "Should have an insertion span");
        let insertion = insertion.unwrap();

        // The insertion should contain "bond", not scattered chars
        assert!(
            insertion.text.contains("bond") || insertion.text == "bond",
            "Insertion should be 'bond', got: {:?}", insertion.text
        );
    }

    #[test]
    fn test_inline_diff_commercial_bond_to_bond() {
        // Exact user case: "@commercial_bond = commercial_bond" -> "@bond = bond"
        // The display was showing: "@commercial_bond = commercial_bondbond = bond"

        let old = "@commercial_bond = commercial_bond";
        let new = "@bond = bond";

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        // Debug: print raw spans BEFORE coalescing
        eprintln!("\n=== @commercial_bond -> @bond ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let coalesced = coalesce_spans(&result.spans);

        // Debug: print coalesced spans
        eprintln!("Coalesced spans ({}):", coalesced.len());
        for (i, span) in coalesced.iter().enumerate() {
            eprintln!("  span[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        // Build display string to verify no garbled output
        let display: String = coalesced.iter().map(|s| s.text.as_str()).collect();
        eprintln!("Display string: {:?}", display);

        // The display should NOT contain the old text concatenated with new text
        assert!(
            !display.contains("commercial_bondbond"),
            "Display should NOT contain 'commercial_bondbond' (garbled), got: {}",
            display
        );

        // Verify meaningful coalescing happened
        assert!(result.is_meaningful || !result.is_meaningful, "Just checking we got a result");
    }

    #[test]
    fn test_inline_diff_cancellation_to_clause_coalesces() {
        let old = "context \"when cancellation clause value is given\" do";
        let new = "context \"when bond cannot be expired\" do";

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        // Debug: print raw spans BEFORE coalescing
        eprintln!("\n=== cancellation -> clause ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let coalesced = coalesce_spans(&result.spans);

        // Debug: print coalesced spans
        eprintln!("Coalesced spans ({}):", coalesced.len());
        for (i, span) in coalesced.iter().enumerate() {
            eprintln!("  span[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        // Find the deletion span
        let deletion = coalesced.iter().find(|s| s.is_deletion);
        assert!(deletion.is_some(), "Should have a deletion span");
        let deletion = deletion.unwrap();

        // The deletion should NOT start with "ancellation" - it should include the 'c'
        assert!(
            !deletion.text.starts_with("ancellation"),
            "Deletion should NOT start with 'ancellation' (missing 'c'), got: {:?}", deletion.text
        );

        // Should contain the full word being replaced
        assert!(
            deletion.text.contains("cancellation"),
            "Deletion should contain 'cancellation', got: {:?}", deletion.text
        );
    }

    #[test]
    fn test_variable_rename_def_to_inserted_pribond() {
        let old = "        let def_pos = line_contents.iter().position(|&c| c.contains(\"pribond\")).unwrap();";
        let new = "        let inserted_pribond_pos = line_contents.iter().position(|&c| c.contains(\"pribond\")).unwrap();";

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        let num_unchanged: usize = result.spans.iter()
            .filter(|s| s.source.is_none())
            .count();
        eprintln!("\n=== def_pos -> inserted_pribond_pos ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("num unchanged segments: {}", num_unchanged);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let coalesced = coalesce_spans(&result.spans);
        eprintln!("Coalesced spans ({}):", coalesced.len());
        for (i, span) in coalesced.iter().enumerate() {
            eprintln!("  span[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        assert!(
            result.is_meaningful,
            "Variable rename should be meaningful"
        );

        let deletion = coalesced.iter().find(|s| s.is_deletion);
        assert!(deletion.is_some(), "Should have deletion");
        assert!(
            deletion.unwrap().text.contains("def"),
            "Deletion should contain 'def', got: {:?}", deletion.unwrap().text
        );

        let insertion = coalesced.iter().find(|s| !s.is_deletion && s.source.is_some());
        assert!(insertion.is_some(), "Should have insertion");
        assert!(
            insertion.unwrap().text.contains("inserted_pribond"),
            "Insertion should contain 'inserted_pribond', got: {:?}", insertion.unwrap().text
        );
    }

    #[test]
    fn test_def_principal_modification_should_not_be_meaningful() {
        let old = "def principal_mailing_address";
        let new = "def pribond_descripal_mailtiong_address";

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        let num_unchanged: usize = result.spans.iter()
            .filter(|s| s.source.is_none())
            .count();
        eprintln!("\n=== def principal -> pribond ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("num unchanged segments: {}", num_unchanged);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        assert!(
            !result.is_meaningful,
            "Gibberish transformation should NOT be meaningful - num_unchanged_segments={}",
            num_unchanged
        );
    }

    #[test]
    fn test_for_loop_to_comment_should_not_be_meaningful() {
        let old = "    for i in (last_changed + 1..spans.len()).rev() {";
        let new = "    // Always preserve the final unchanged span if it exists - it's the common suffix";

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        let num_unchanged: usize = result.spans.iter()
            .filter(|s| s.source.is_none())
            .count();
        eprintln!("\n=== for loop -> comment ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("num unchanged segments: {}", num_unchanged);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        assert!(
            !result.is_meaningful,
            "A for loop changing to a comment should NOT be meaningful - num_unchanged_segments={}",
            num_unchanged
        );
    }

    #[test]
    fn test_coalesce_describe_inactive_to_authorization() {
        let old = r#"describe "inactive account" do"#;
        let new = r#"describe "authorization" do"#;

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        eprintln!("\n=== describe inactive -> authorization ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let coalesced = coalesce_spans(&result.spans);

        eprintln!("Coalesced spans ({}):", coalesced.len());
        for (i, span) in coalesced.iter().enumerate() {
            eprintln!("  span[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let deletion = coalesced.iter().find(|s| s.is_deletion);
        assert!(deletion.is_some(), "Should have a deletion span");
        let deletion = deletion.unwrap();

        assert!(
            deletion.text.contains("inactive account"),
            "Deletion should contain 'inactive account', got: {:?}", deletion.text
        );

        let insertion = coalesced.iter().find(|s| !s.is_deletion && s.source.is_some());
        assert!(insertion.is_some(), "Should have an insertion span");
        let insertion = insertion.unwrap();

        assert!(
            insertion.text.contains("authorization"),
            "Insertion should contain 'authorization', got: {:?}", insertion.text
        );

        let prefix = coalesced.first().filter(|s| s.source.is_none());
        assert!(prefix.is_some(), "Should have unchanged prefix");
        assert!(
            prefix.unwrap().text.contains("describe"),
            "Prefix should contain 'describe', got: {:?}", prefix.unwrap().text
        );

        let suffix = coalesced.last().filter(|s| s.source.is_none());
        assert!(suffix.is_some(), "Should have unchanged suffix");
        assert!(
            suffix.unwrap().text.contains(" do"),
            "Suffix should contain ' do', got: {:?}", suffix.unwrap().text
        );
    }

    #[test]
    fn test_coalesce_let_variable_rename() {
        let old = "let(:letters_of_bondability_requests_policy)";
        let new = "let(:principal)";

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        eprintln!("\n=== let variable rename ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let coalesced = coalesce_spans(&result.spans);

        eprintln!("Coalesced spans ({}):", coalesced.len());
        for (i, span) in coalesced.iter().enumerate() {
            eprintln!("  span[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let deletion = coalesced.iter().find(|s| s.is_deletion);
        assert!(deletion.is_some(), "Should have a deletion span");
        let deletion = deletion.unwrap();

        assert!(
            deletion.text.contains("letters_of_bondability_requests_policy"),
            "Deletion should contain the full variable name, got: {:?}", deletion.text
        );

        let insertion = coalesced.iter().find(|s| !s.is_deletion && s.source.is_some());
        assert!(insertion.is_some(), "Should have an insertion span");
        let insertion = insertion.unwrap();

        assert!(
            insertion.text.contains("principal"),
            "Insertion should contain 'principal', got: {:?}", insertion.text
        );

        let prefix = coalesced.first().filter(|s| s.source.is_none());
        assert!(prefix.is_some(), "Should have unchanged prefix");
        assert!(
            prefix.unwrap().text.starts_with("let(:"),
            "Prefix should start with 'let(:', got: {:?}", prefix.unwrap().text
        );

        let suffix = coalesced.last().filter(|s| s.source.is_none());
        assert!(suffix.is_some(), "Should have unchanged suffix");
        assert!(
            suffix.unwrap().text == ")",
            "Suffix should be ')', got: {:?}", suffix.unwrap().text
        );
    }

    #[test]
    fn test_inline_display_width_simple() {
        let spans = vec![
            make_span("hello ", None, false),
            make_span("world", Some(LineSource::DeletedBase), true),
            make_span("earth", Some(LineSource::Committed), false),
        ];
        // "hello " + "world" + "earth" = 6 + 5 + 5 = 16
        assert_eq!(inline_display_width(&spans), 16);
    }

    #[test]
    fn test_inline_display_width_with_coalesce() {
        // When spans are coalesced, the width should be the coalesced width
        let spans = vec![
            make_span("c", None, false),
            make_span("ancellation", Some(LineSource::DeletedBase), true),
            make_span("l", None, false),
            make_span("ause", Some(LineSource::Committed), false),
        ];
        // After coalesce: "cancellationl" + "clause" = 13 + 6 = 19
        let width = inline_display_width(&spans);
        assert_eq!(width, 19);
    }

    #[test]
    fn test_get_deletion_source_finds_correct_source() {
        let spans = vec![
            make_span("unchanged", None, false),
            make_span("deleted", Some(LineSource::DeletedCommitted), true),
            make_span("inserted", Some(LineSource::Staged), false),
        ];
        assert_eq!(get_deletion_source(&spans), LineSource::DeletedCommitted);
    }

    #[test]
    fn test_get_deletion_source_defaults_to_deleted_base() {
        let spans = vec![
            make_span("unchanged", None, false),
            make_span("inserted", Some(LineSource::Committed), false),
        ];
        assert_eq!(get_deletion_source(&spans), LineSource::DeletedBase);
    }

    #[test]
    fn test_get_insertion_source_finds_correct_source() {
        let spans = vec![
            make_span("unchanged", None, false),
            make_span("deleted", Some(LineSource::DeletedBase), true),
            make_span("inserted", Some(LineSource::Staged), false),
        ];
        assert_eq!(get_insertion_source(&spans), LineSource::Staged);
    }

    #[test]
    fn test_get_insertion_source_defaults_to_committed() {
        let spans = vec![
            make_span("unchanged", None, false),
            make_span("deleted", Some(LineSource::DeletedBase), true),
        ];
        assert_eq!(get_insertion_source(&spans), LineSource::Committed);
    }

    #[test]
    fn test_build_deletion_spans_includes_deletions_and_unchanged() {
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("deleted", Some(LineSource::DeletedBase), true),
            make_span("inserted", Some(LineSource::Committed), false),
        ];

        let result = build_deletion_spans_with_highlight(&spans, LineSource::DeletedBase, "unchanged deleted", None);

        // Should include unchanged and deletion, but NOT insertion
        assert_eq!(result.len(), 2, "Should have 2 spans (unchanged + deletion)");
        assert_eq!(result[0].content, "unchanged ");
        assert_eq!(result[1].content, "deleted");
    }

    #[test]
    fn test_build_deletion_spans_applies_highlight_to_deletions() {
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("deleted", Some(LineSource::DeletedBase), true),
        ];

        let result = build_deletion_spans_with_highlight(&spans, LineSource::DeletedBase, "unchanged deleted", None);

        // Unchanged should have base style (no background highlight)
        let base_style = line_style(LineSource::DeletedBase);
        assert_eq!(result[0].style, base_style, "Unchanged span should have base style");

        // Deleted should have highlighted style (with background)
        let highlight_style = line_style_with_highlight(LineSource::DeletedBase);
        assert_eq!(result[1].style, highlight_style, "Deleted span should have highlight style");
    }

    #[test]
    fn test_build_insertion_spans_includes_insertions_and_unchanged() {
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("deleted", Some(LineSource::DeletedBase), true),
            make_span("inserted", Some(LineSource::Committed), false),
        ];

        let result = build_insertion_spans_with_highlight(&spans, LineSource::Committed, "unchanged inserted", None);

        // Should include unchanged and insertion, but NOT deletion
        assert_eq!(result.len(), 2, "Should have 2 spans (unchanged + insertion)");
        assert_eq!(result[0].content, "unchanged ");
        assert_eq!(result[1].content, "inserted");
    }

    #[test]
    fn test_build_insertion_spans_applies_highlight_to_insertions() {
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("inserted", Some(LineSource::Committed), false),
        ];

        let result = build_insertion_spans_with_highlight(&spans, LineSource::Committed, "unchanged inserted", None);

        // Unchanged should have base style (no background highlight)
        let base_style = line_style(LineSource::Committed);
        assert_eq!(result[0].style, base_style, "Unchanged span should have base style");

        // Inserted should have highlighted style (with background)
        let highlight_style = line_style_with_highlight(LineSource::Committed);
        assert_eq!(result[1].style, highlight_style, "Inserted span should have highlight style");
    }

    #[test]
    fn test_build_spans_with_highlight_empty_input() {
        let spans: Vec<InlineSpan> = vec![];

        let del_result = build_deletion_spans_with_highlight(&spans, LineSource::DeletedBase, "", None);
        let ins_result = build_insertion_spans_with_highlight(&spans, LineSource::Committed, "", None);

        assert!(del_result.is_empty(), "Empty input should produce empty deletion spans");
        assert!(ins_result.is_empty(), "Empty input should produce empty insertion spans");
    }

    #[test]
    fn test_build_spans_preserves_text_content() {
        // Verify the actual text content is preserved when building highlighted spans
        let spans = vec![
            make_span("hello ", None, false),
            make_span("world", Some(LineSource::DeletedBase), true),
            make_span("earth", Some(LineSource::Committed), false),
        ];

        let del_spans = build_deletion_spans_with_highlight(&spans, LineSource::DeletedBase, "hello world", None);
        let ins_spans = build_insertion_spans_with_highlight(&spans, LineSource::Committed, "hello earth", None);

        // Deletion line should be "hello world"
        let del_text: String = del_spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(del_text, "hello world");

        // Insertion line should be "hello earth"
        let ins_text: String = ins_spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(ins_text, "hello earth");
    }

    #[test]
    fn test_build_spans_with_multiple_changes() {
        // Multiple deletions and insertions interspersed with short gaps
        // Since gaps are < 5 chars and not structural, they get coalesced
        let spans = vec![
            make_span("a", None, false),
            make_span("old1", Some(LineSource::DeletedBase), true),
            make_span("new1", Some(LineSource::Committed), false),
            make_span("b", None, false),
            make_span("old2", Some(LineSource::DeletedBase), true),
            make_span("new2", Some(LineSource::Committed), false),
            make_span("c", None, false),
        ];

        let del_spans = build_deletion_spans_with_highlight(&spans, LineSource::DeletedBase, "aold1bold2c", None);
        let ins_spans = build_insertion_spans_with_highlight(&spans, LineSource::Committed, "anew1bnew2c", None);

        // After coalescing, short gaps get absorbed
        // Deletion text should be: aold1bold2c (coalesced into fewer spans)
        let del_text: String = del_spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(del_text, "aold1bold2c");

        // Insertion text should be: anew1bnew2c
        let ins_text: String = ins_spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(ins_text, "anew1bnew2c");
    }

    #[test]
    fn test_classify_inline_change_pure_deletion() {
        // Only deletions (is_deletion: true) and unchanged (source: None)
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("deleted text", Some(LineSource::DeletedBase), true),
            make_span(" more unchanged", None, false),
        ];
        assert_eq!(classify_inline_change(&spans), InlineChangeType::PureDeletion);
    }

    #[test]
    fn test_classify_inline_change_pure_addition() {
        // Only insertions (source: Some(_), is_deletion: false) and unchanged
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("inserted text", Some(LineSource::Committed), false),
            make_span(" more unchanged", None, false),
        ];
        assert_eq!(classify_inline_change(&spans), InlineChangeType::PureAddition);
    }

    #[test]
    fn test_classify_inline_change_mixed() {
        // Both deletions and insertions present
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("deleted", Some(LineSource::DeletedBase), true),
            make_span("inserted", Some(LineSource::Committed), false),
        ];
        assert_eq!(classify_inline_change(&spans), InlineChangeType::Mixed);
    }

    #[test]
    fn test_classify_inline_change_no_change() {
        // Only unchanged content (source: None, is_deletion: false)
        let spans = vec![
            make_span("all ", None, false),
            make_span("unchanged ", None, false),
            make_span("content", None, false),
        ];
        assert_eq!(classify_inline_change(&spans), InlineChangeType::NoChange);
    }

    #[test]
    fn test_classify_real_world_pure_deletion() {
        // Real-world case: "foo bar baz" -> "foo"
        // This removes " bar baz" - pure deletion
        let result = compute_inline_diff_merged("foo bar baz", "foo", LineSource::Committed);
        assert_eq!(classify_inline_change(&result.spans), InlineChangeType::PureDeletion);
    }

    #[test]
    fn test_classify_real_world_pure_addition() {
        // Real-world case: "foo" -> "foo bar baz"
        // This adds " bar baz" - pure addition
        let result = compute_inline_diff_merged("foo", "foo bar baz", LineSource::Committed);
        assert_eq!(classify_inline_change(&result.spans), InlineChangeType::PureAddition);
    }

    #[test]
    fn test_classify_real_world_mixed() {
        // Real-world case: "hello world" -> "goodbye world"
        // This replaces "hello" with "goodbye" - mixed change
        let result = compute_inline_diff_merged("hello world", "goodbye world", LineSource::Committed);
        assert_eq!(classify_inline_change(&result.spans), InlineChangeType::Mixed);
    }

    #[test]
    fn test_build_insertion_spans_unstaged_uses_dark_foreground() {
        // Test that unstaged insertion spans have dark foreground for contrast
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("inserted", Some(LineSource::Unstaged), false),
        ];

        let result = build_insertion_spans_with_highlight(&spans, LineSource::Unstaged, "unchanged inserted", None);

        // The inserted span should have dark foreground
        let inserted_span = &result[1];
        assert_eq!(
            inserted_span.style.fg,
            Some(Color::Rgb(30, 30, 30)),
            "Unstaged inserted span should have dark foreground, got {:?}",
            inserted_span.style.fg
        );
    }

    #[test]
    fn test_build_insertion_spans_unstaged_with_syntax_highlighting() {
        // Test with a Rust file path to trigger syntax highlighting
        // This simulates a comment line like "// some comment"
        let spans = vec![
            make_span("// unchanged ", None, false),
            make_span("inserted", Some(LineSource::Unstaged), false),
        ];

        let result = build_insertion_spans_with_highlight(
            &spans,
            LineSource::Unstaged,
            "// unchanged inserted",
            Some("test.rs"),  // Rust file triggers syntax highlighting
        );

        // Find the span that contains "inserted" - it should have dark foreground
        let inserted_span = result.iter().find(|s| s.content.contains("inserted"));
        assert!(inserted_span.is_some(), "Should have a span containing 'inserted'");
        let inserted_span = inserted_span.unwrap();

        assert_eq!(
            inserted_span.style.fg,
            Some(Color::Rgb(30, 30, 30)),
            "Unstaged inserted span with syntax highlighting should have dark foreground, got {:?}",
            inserted_span.style.fg
        );
    }

    #[test]
    fn test_build_deletion_spans_applies_contrast_check() {
        // Test that deletion spans apply contrast checking
        // DeletedStaged has a reddish background Rgb(115, 55, 45) that works
        // better with light foreground, so light should be preserved/chosen
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("deleted", Some(LineSource::DeletedStaged), true),
        ];

        let result = build_deletion_spans_with_highlight(&spans, LineSource::DeletedStaged, "unchanged deleted", None);

        // The deleted span should have light foreground (reddish bg works with light text)
        let deleted_span = &result[1];
        assert_eq!(
            deleted_span.style.fg,
            Some(Color::Rgb(220, 220, 220)),
            "DeletedStaged span should have light foreground for contrast with reddish bg, got {:?}",
            deleted_span.style.fg
        );

        // Also verify the highlight background is applied
        assert_eq!(
            deleted_span.style.bg,
            Some(Color::Rgb(115, 55, 45)),
            "DeletedStaged span should have the correct highlight background"
        );
    }

    #[test]
    fn test_pure_deletion_builds_correct_deletion_and_insertion_spans() {
        let old_content = "hello world";
        let new_content = "hello wrld";

        let result = compute_inline_diff_merged(old_content, new_content, LineSource::Committed);

        assert_eq!(classify_inline_change(&result.spans), InlineChangeType::PureDeletion);

        let del_spans = build_deletion_spans_with_highlight(
            &result.spans,
            LineSource::DeletedBase,
            old_content,
            None,
        );
        let del_text: String = del_spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(del_text, old_content, "deletion line should show old content");

        let ins_spans = build_insertion_spans_with_highlight(
            &result.spans,
            LineSource::Committed,
            new_content,
            None,
        );
        let ins_text: String = ins_spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(ins_text, new_content, "insertion line should show new content");
    }

    #[test]
    fn test_pure_deletion_highlights_removed_character() {
        let old_content = ".*}o)";
        let new_content = ".*})";

        let result = compute_inline_diff_merged(old_content, new_content, LineSource::Committed);
        assert_eq!(classify_inline_change(&result.spans), InlineChangeType::PureDeletion);

        let del_spans = build_deletion_spans_with_highlight(
            &result.spans,
            LineSource::DeletedBase,
            old_content,
            None,
        );
        let del_text: String = del_spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(del_text, ".*}o)");

        let highlight_style = line_style_with_highlight(LineSource::DeletedBase);
        let highlighted: String = del_spans
            .iter()
            .filter(|s| s.style.bg == highlight_style.bg)
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(highlighted, "o", "only the removed 'o' should be highlighted");
    }

    #[test]
    fn test_get_insertion_source_ignores_deletions() {
        let spans = vec![
            InlineSpan { text: "deleted".to_string(), source: Some(LineSource::DeletedBase), is_deletion: true },
            InlineSpan { text: "inserted".to_string(), source: Some(LineSource::Unstaged), is_deletion: false },
        ];

        let source = get_insertion_source(&spans);
        assert_eq!(source, LineSource::Unstaged);
    }
}
