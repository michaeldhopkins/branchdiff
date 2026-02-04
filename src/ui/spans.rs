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
    use crate::syntax::reset_highlight_state;

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
}
