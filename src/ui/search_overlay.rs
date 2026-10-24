//! Search match overlay and search bar rendering for the TUI diff view.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};

use crate::app::SearchState;

use super::colors::{SEARCH_CURRENT_BG, SEARCH_MATCH_BG};

/// Translate a content-space match range `[content_start, content_end)` into
/// the corresponding render-space ranges, accounting for deletion text that
/// `syntax_highlight_inline_spans` interleaves into the rendered output.
///
/// Search matches are computed against `line.content` (the new text only —
/// see [`crate::app::search::compute_matches`]). When the renderer emits
/// deletion text inline, those characters take up render columns but do not
/// exist in `content`, so a single content match may map to multiple
/// disjoint render ranges.
pub(super) fn content_match_to_render_ranges(
    inline_spans: &[crate::diff::InlineSpan],
    content_start: usize,
    content_end: usize,
) -> Vec<(usize, usize)> {
    if content_start >= content_end {
        return Vec::new();
    }
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut content_pos = 0;
    let mut render_pos = 0;

    for span in inline_spans {
        let len = span.text.chars().count();
        if span.is_deletion {
            render_pos += len;
            continue;
        }
        let span_c_end = content_pos + len;
        let inter_start = content_start.max(content_pos);
        let inter_end = content_end.min(span_c_end);
        if inter_start < inter_end {
            let r_start = render_pos + (inter_start - content_pos);
            let r_end = render_pos + (inter_end - content_pos);
            match ranges.last_mut() {
                Some(last) if last.1 == r_start => last.1 = r_end,
                _ => ranges.push((r_start, r_end)),
            }
        }
        content_pos = span_c_end;
        render_pos += len;
        if content_pos >= content_end {
            break;
        }
    }
    ranges
}

/// Overlay search match highlights on content spans whose rendered text
/// matches `line.content` 1:1 (no interleaved deletion text).
///
/// SearchMatch stores char offsets (not byte offsets) so this works correctly
/// with multi-byte UTF-8 content.
pub(super) fn apply_search_to_content(
    content_spans: Vec<Span<'static>>,
    search: &Option<SearchState>,
    line_idx: usize,
) -> Vec<Span<'static>> {
    apply_search_with_translation(content_spans, search, line_idx, None)
}

/// Same as [`apply_search_to_content`] but for spans whose rendered text is
/// `inline_spans` concatenated (i.e., includes interleaved deletion text).
/// Match offsets are translated through `inline_spans` so highlights land on
/// the correct rendered characters.
pub(super) fn apply_search_to_inline_render(
    content_spans: Vec<Span<'static>>,
    inline_spans: &[crate::diff::InlineSpan],
    search: &Option<SearchState>,
    line_idx: usize,
) -> Vec<Span<'static>> {
    apply_search_with_translation(content_spans, search, line_idx, Some(inline_spans))
}

pub(super) fn apply_search_with_translation(
    content_spans: Vec<Span<'static>>,
    search: &Option<SearchState>,
    line_idx: usize,
    inline_spans: Option<&[crate::diff::InlineSpan]>,
) -> Vec<Span<'static>> {
    let Some(search) = search else {
        return content_spans;
    };
    if search.matches.is_empty() {
        return content_spans;
    }

    // Collect render-space highlight ranges for matches on this line.
    let mut highlights: Vec<(usize, usize, bool)> = Vec::new();
    for (m_idx, m) in search.matches.iter().enumerate() {
        if m.line_idx != line_idx {
            continue;
        }
        let is_current = m_idx == search.current;
        let c_start = m.char_start;
        let c_end = m.char_start + m.char_len;
        match inline_spans {
            Some(inline) => {
                for (rs, re) in content_match_to_render_ranges(inline, c_start, c_end) {
                    highlights.push((rs, re, is_current));
                }
            }
            None => highlights.push((c_start, c_end, is_current)),
        }
    }

    if highlights.is_empty() {
        return content_spans;
    }

    // Apply right-to-left so earlier indices stay stable as later splits insert spans.
    highlights.sort_by_key(|h| std::cmp::Reverse(h.0));
    let mut result = content_spans;
    for (h_start, h_end, is_current) in highlights {
        let bg = if is_current { SEARCH_CURRENT_BG } else { SEARCH_MATCH_BG };
        result = overlay_range(result, h_start, h_end, bg);
    }
    result
}

/// Split spans as needed to apply `bg` over render-space `[h_start, h_end)`.
pub(super) fn overlay_range(
    spans: Vec<Span<'static>>,
    h_start: usize,
    h_end: usize,
    bg: ratatui::style::Color,
) -> Vec<Span<'static>> {
    let mut new_result = Vec::with_capacity(spans.len() + 2);
    let mut char_offset = 0;
    for span in spans {
        let span_char_len = span.content.chars().count();
        let span_end = char_offset + span_char_len;

        if span_end <= h_start || char_offset >= h_end {
            new_result.push(span);
        } else {
            let base_style = span.style;
            let text: Vec<char> = span.content.chars().collect();
            let rel_start = h_start.saturating_sub(char_offset);
            let rel_end = (h_end - char_offset).min(span_char_len);

            if rel_start > 0 {
                let before: String = text[..rel_start].iter().collect();
                new_result.push(Span::styled(before, base_style));
            }
            let matched: String = text[rel_start..rel_end].iter().collect();
            new_result.push(Span::styled(matched, base_style.bg(bg)));
            if rel_end < span_char_len {
                let after: String = text[rel_end..].iter().collect();
                new_result.push(Span::styled(after, base_style));
            }
        }
        char_offset = span_end;
    }
    new_result
}

pub(super) fn render_search_bar(frame: &mut Frame, search: &SearchState, area: Rect) {
    let bar_bg = Color::Rgb(40, 42, 54);
    let bar_style = Style::default().fg(Color::White).bg(bar_bg);

    let counter = if search.matches.is_empty() && !search.query.is_empty() {
        "[no matches]".to_string()
    } else if !search.matches.is_empty() {
        let total = search.match_count();
        if search.visible_count < total {
            format!("[{}/{}]", search.current_display(), search.visible_count)
        } else {
            format!("[{}/{}]", search.current_display(), total)
        }
    } else {
        String::new()
    };

    let counter_width = counter.len();
    let available_for_query = (area.width as usize).saturating_sub(counter_width + 2);

    let query_char_count = search.query.chars().count();
    let display_query: String = if query_char_count > available_for_query {
        let skip = query_char_count - available_for_query + 1;
        format!("…{}", search.query.chars().skip(skip).collect::<String>())
    } else {
        search.query.clone()
    };

    let cursor_char = if search.input_active { "█" } else { "" };
    let left_text = format!("/{}{}", display_query, cursor_char);
    let padding = (area.width as usize).saturating_sub(left_text.chars().count() + counter_width);

    let line = Line::from(vec![
        Span::styled(left_text, bar_style),
        Span::styled(" ".repeat(padding), bar_style),
        Span::styled(counter, Style::default().fg(Color::DarkGray).bg(bar_bg)),
    ]);

    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(vec![line]), area);
}
