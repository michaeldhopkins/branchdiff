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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::search::{SearchMatch, SearchState};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Style};
    use ratatui::text::Span;

    fn state(query: &str, match_count: usize, visible: usize, active: bool) -> SearchState {
        SearchState {
            query: query.to_string(),
            input_active: active,
            matches: (0..match_count)
                .map(|line_idx| SearchMatch { line_idx, char_start: 0, char_len: 1 })
                .collect(),
            current: 0,
            visible_count: visible,
            visible_position: 0,
        }
    }

    /// The painted row, including trailing spaces, so a counter that is not
    /// flush with the right edge fails an exact comparison.
    fn paint(search: &SearchState, width: u16) -> String {
        let backend = TestBackend::new(width, 1);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| render_search_bar(frame, search, Rect::new(0, 0, width, 1)))
            .expect("draw");
        let buf = terminal.backend().buffer();
        (0..width).map(|x| buf[(x, 0)].symbol()).collect()
    }

    #[test]
    fn overlay_keeps_the_tail_when_the_match_ends_inside_the_span() {
        let spans = vec![Span::styled("abcdefghij", Style::default().fg(Color::Red))];
        let out = overlay_range(spans, 2, 5, Color::Yellow);
        let text: String = out.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(text, "abcdefghij");
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].content.as_ref(), "ab");
        assert_eq!(out[0].style.bg, None);
        assert_eq!(out[0].style.fg, Some(Color::Red));
        assert_eq!(out[1].content.as_ref(), "cde");
        assert_eq!(out[1].style.bg, Some(Color::Yellow));
        assert_eq!(out[2].content.as_ref(), "fghij");
        assert_eq!(out[2].style.bg, None);
        assert_eq!(out[2].style.fg, Some(Color::Red));
    }

    #[test]
    fn an_empty_search_shows_the_prompt_and_a_cursor() {
        let row = paint(&state("", 0, 0, true), 12);
        assert_eq!(row, "/█          ");
        assert!(!row.contains("no matches"));
    }

    #[test]
    fn a_query_with_no_matches_names_that_and_the_cursor_tracks_input() {
        let inactive = paint(&state("zz", 0, 0, false), 30);
        assert_eq!(inactive, "/zz               [no matches]");
        assert!(!inactive.contains('█'));

        let active = paint(&state("zz", 0, 0, true), 30);
        assert_eq!(active, "/zz█              [no matches]");
    }

    #[test]
    fn matches_with_an_empty_query_show_the_count() {
        let row = paint(&state("", 1, 1, false), 20);
        assert_eq!(row, "/              [1/1]");
        assert!(!row.contains("no matches"));
    }

    #[test]
    fn a_filtered_count_shows_visible_matches_not_the_total() {
        // 3 visible of 8. The counter is five columns and sits on the right edge.
        let row = paint(&state("ab", 8, 3, false), 24);
        assert_eq!(row, "/ab                [1/3]");
        assert!(!row.contains("[1/8]"));
    }

    #[test]
    fn an_unfiltered_count_shows_the_same_number_for_both_arms() {
        // visible_count == total, so either arm prints "[1/3]".
        let row = paint(&state("ab", 3, 3, false), 24);
        assert_eq!(row, "/ab                [1/3]");
    }

    #[test]
    fn a_query_that_exactly_fits_is_shown_whole() {
        // Width 20, counter "[1/1]" (5) leaves 13 columns for the query
        // (width - counter - 2). Thirteen characters must not gain an ellipsis.
        let row = paint(&state("abcdefghijklm", 1, 1, false), 20);
        assert_eq!(row, "/abcdefghijklm [1/1]");
        assert!(!row.contains('…'));
    }

    #[test]
    fn a_long_query_keeps_its_tail_and_the_counter_on_the_right() {
        // Available columns are 13. Skip is (20 - 13 + 1) = 8, so the bar
        // shows the ellipsis plus "ijklmnopqrst", then one space, then "[1/1]".
        let row = paint(&state("abcdefghijklmnopqrst", 1, 1, false), 20);
        assert_eq!(row, "/…ijklmnopqrst [1/1]");
    }
}
