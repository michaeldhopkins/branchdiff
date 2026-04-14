use ratatui::text::Span;

use crate::app::Selection;

pub const SELECTION_BG_COLOR: ratatui::style::Color = ratatui::style::Color::Rgb(60, 60, 100);

/// Get the selection range for a specific line (start_col, end_col)
/// Returns None if the line is not selected
pub fn get_line_selection_range(selection: &Option<Selection>, line_idx: usize) -> Option<(usize, usize)> {
    let sel = selection.as_ref()?;

    // Normalize selection (start should be before end)
    let (start, end) = if sel.start.row < sel.end.row
        || (sel.start.row == sel.end.row && sel.start.col <= sel.end.col)
    {
        (sel.start, sel.end)
    } else {
        (sel.end, sel.start)
    };

    // Check if this line is within selection
    if line_idx < start.row || line_idx > end.row {
        return None;
    }

    // Determine start and end columns for this line
    let start_col = if line_idx == start.row { start.col } else { 0 };
    let end_col = if line_idx == end.row { end.col } else { usize::MAX };

    Some((start_col, end_col))
}

/// Apply selection highlighting to a span, splitting it if partially selected.
///
/// All offsets (`display_offset`, `sel_start`, `sel_end`) are in **display
/// columns** (i.e. `UnicodeWidthStr::width`), matching the screen coordinates
/// used by the selection model.
pub fn apply_selection_to_span(
    span: Span<'static>,
    display_offset: usize,
    sel_start: usize,
    sel_end: usize,
) -> Vec<Span<'static>> {
    use unicode_width::UnicodeWidthChar;

    let text = span.content.to_string();
    let text_width: usize = text.chars().map(|c| UnicodeWidthChar::width(c).unwrap_or(0)).sum();
    let text_start = display_offset;
    let text_end = display_offset + text_width;

    if text_end <= sel_start || text_start >= sel_end {
        return vec![span];
    }

    let mut result = Vec::new();
    let base_style = span.style;
    let selected_style = base_style.bg(SELECTION_BG_COLOR);

    // Split text at a display-width boundary, returning (before, from) byte indices.
    let byte_index_at_width = |target_width: usize| -> usize {
        let mut w = 0;
        for (i, ch) in text.char_indices() {
            if w >= target_width {
                return i;
            }
            w += UnicodeWidthChar::width(ch).unwrap_or(0);
        }
        text.len()
    };

    let local_sel_start = sel_start.saturating_sub(text_start).min(text_width);
    let local_sel_end = sel_end.saturating_sub(text_start).min(text_width);

    let byte_sel_start = byte_index_at_width(local_sel_start);
    let byte_sel_end = byte_index_at_width(local_sel_end);

    // Before selection
    if byte_sel_start > 0 {
        result.push(Span::styled(text[..byte_sel_start].to_string(), base_style));
    }

    // Selected portion
    if byte_sel_start < byte_sel_end {
        result.push(Span::styled(text[byte_sel_start..byte_sel_end].to_string(), selected_style));
    }

    // After selection
    if byte_sel_end < text.len() {
        result.push(Span::styled(text[byte_sel_end..].to_string(), base_style));
    }

    if result.is_empty() {
        vec![span]
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Position, Selection};
    use ratatui::style::Style;

    fn selection(start_row: usize, start_col: usize, end_row: usize, end_col: usize) -> Selection {
        Selection {
            start: Position { row: start_row, col: start_col },
            end: Position { row: end_row, col: end_col },
            active: true,
        }
    }

    // ===== get_line_selection_range tests =====

    #[test]
    fn line_selection_returns_none_when_no_selection() {
        assert_eq!(get_line_selection_range(&None, 5), None);
    }

    #[test]
    fn line_selection_returns_none_when_line_before_selection() {
        let sel = selection(5, 0, 10, 0);
        assert_eq!(get_line_selection_range(&Some(sel), 3), None);
    }

    #[test]
    fn line_selection_returns_none_when_line_after_selection() {
        let sel = selection(5, 0, 10, 0);
        assert_eq!(get_line_selection_range(&Some(sel), 15), None);
    }

    #[test]
    fn line_selection_single_line_returns_exact_columns() {
        let sel = selection(5, 3, 5, 10);
        assert_eq!(get_line_selection_range(&Some(sel), 5), Some((3, 10)));
    }

    #[test]
    fn line_selection_normalizes_backwards_selection() {
        // Selection dragged backwards: end is before start
        let sel = selection(5, 10, 5, 3);
        assert_eq!(get_line_selection_range(&Some(sel), 5), Some((3, 10)));
    }

    #[test]
    fn line_selection_first_line_of_multiline() {
        let sel = selection(5, 8, 10, 4);
        // First line: from start.col to end of line
        assert_eq!(get_line_selection_range(&Some(sel), 5), Some((8, usize::MAX)));
    }

    #[test]
    fn line_selection_last_line_of_multiline() {
        let sel = selection(5, 8, 10, 4);
        // Last line: from beginning to end.col
        assert_eq!(get_line_selection_range(&Some(sel), 10), Some((0, 4)));
    }

    #[test]
    fn line_selection_middle_line_of_multiline() {
        let sel = selection(5, 8, 10, 4);
        // Middle lines: entire line selected
        assert_eq!(get_line_selection_range(&Some(sel), 7), Some((0, usize::MAX)));
    }

    // ===== apply_selection_to_span tests =====

    #[test]
    fn span_no_overlap_before_selection() {
        let span = Span::styled("hello", Style::default());
        // Span at offset 0-5, selection at 10-15
        let result = apply_selection_to_span(span.clone(), 0, 10, 15);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "hello");
    }

    #[test]
    fn span_no_overlap_after_selection() {
        let span = Span::styled("hello", Style::default());
        // Span at offset 20-25, selection at 10-15
        let result = apply_selection_to_span(span.clone(), 20, 10, 15);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "hello");
    }

    #[test]
    fn span_fully_inside_selection() {
        let span = Span::styled("hello", Style::default());
        // Span at offset 10-15, selection at 5-20 (fully contains span)
        let result = apply_selection_to_span(span, 10, 5, 20);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "hello");
        assert_eq!(result[0].style.bg, Some(SELECTION_BG_COLOR));
    }

    #[test]
    fn span_selection_at_start() {
        let span = Span::styled("hello", Style::default());
        // Span at offset 0, selection covers first 2 chars
        let result = apply_selection_to_span(span, 0, 0, 2);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "he");
        assert_eq!(result[0].style.bg, Some(SELECTION_BG_COLOR));
        assert_eq!(result[1].content, "llo");
        assert_eq!(result[1].style.bg, None);
    }

    #[test]
    fn span_selection_at_end() {
        let span = Span::styled("hello", Style::default());
        // Span at offset 0, selection covers last 2 chars
        let result = apply_selection_to_span(span, 0, 3, 10);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "hel");
        assert_eq!(result[0].style.bg, None);
        assert_eq!(result[1].content, "lo");
        assert_eq!(result[1].style.bg, Some(SELECTION_BG_COLOR));
    }

    #[test]
    fn span_selection_in_middle() {
        let span = Span::styled("hello", Style::default());
        // Span at offset 0, selection covers middle chars
        let result = apply_selection_to_span(span, 0, 1, 4);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].content, "h");
        assert_eq!(result[0].style.bg, None);
        assert_eq!(result[1].content, "ell");
        assert_eq!(result[1].style.bg, Some(SELECTION_BG_COLOR));
        assert_eq!(result[2].content, "o");
        assert_eq!(result[2].style.bg, None);
    }

    #[test]
    fn span_with_char_offset() {
        let span = Span::styled("world", Style::default());
        // Span starts at offset 10, selection is 12-14 (chars 2-4 of span)
        let result = apply_selection_to_span(span, 10, 12, 14);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].content, "wo");
        assert_eq!(result[1].content, "rl");
        assert_eq!(result[1].style.bg, Some(SELECTION_BG_COLOR));
        assert_eq!(result[2].content, "d");
    }

    // ===== Additional edge case tests =====

    #[test]
    fn span_empty_string() {
        let span = Span::styled("", Style::default());
        let result = apply_selection_to_span(span, 0, 0, 10);
        // Empty span should return original
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "");
    }

    #[test]
    fn span_unicode_content() {
        // Test that unicode content is handled correctly.
        // apply_selection_to_span uses DISPLAY WIDTH positions, not byte or char positions.
        // "héllo wörld" = 11 chars, 13 bytes, 11 display columns (all 1-col wide).
        let span = Span::styled("héllo wörld", Style::default());

        // Select the entire string (all 11 display columns)
        let result = apply_selection_to_span(span.clone(), 0, 0, 11);
        assert_eq!(result.len(), 1, "Entire string should be one selected span");
        assert_eq!(result[0].content, "héllo wörld");
        assert_eq!(result[0].style.bg, Some(SELECTION_BG_COLOR));

        // Select first 5 characters: "héllo"
        let span2 = Span::styled("héllo wörld", Style::default());
        let result2 = apply_selection_to_span(span2, 0, 0, 5);
        assert_eq!(result2.len(), 2, "Should split into selected and unselected");
        assert_eq!(result2[0].content, "héllo");
        assert_eq!(result2[0].style.bg, Some(SELECTION_BG_COLOR));
        assert_eq!(result2[1].content, " wörld");

        // Verify multi-byte characters work in the middle too
        let span3 = Span::styled("héllo wörld", Style::default());
        let result3 = apply_selection_to_span(span3, 0, 6, 9); // "wör"
        assert_eq!(result3.len(), 3);
        assert_eq!(result3[0].content, "héllo ");
        assert_eq!(result3[1].content, "wör");
        assert_eq!(result3[1].style.bg, Some(SELECTION_BG_COLOR));
        assert_eq!(result3[2].content, "ld");
    }

    #[test]
    fn span_selection_exact_boundaries() {
        let span = Span::styled("hello", Style::default());
        // Selection exactly matches span boundaries
        let result = apply_selection_to_span(span, 5, 5, 10);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "hello");
        assert_eq!(result[0].style.bg, Some(SELECTION_BG_COLOR));
    }

    #[test]
    fn line_selection_normalizes_multiline_backwards() {
        // Selection dragged backwards across multiple lines
        let sel = selection(10, 4, 5, 8);
        // Line 7 should still be fully selected (middle line)
        assert_eq!(get_line_selection_range(&Some(sel.clone()), 7), Some((0, usize::MAX)));
        // Start line (after normalization, this is row 5)
        assert_eq!(get_line_selection_range(&Some(sel.clone()), 5), Some((8, usize::MAX)));
        // End line (after normalization, this is row 10)
        assert_eq!(get_line_selection_range(&Some(sel), 10), Some((0, 4)));
    }

    #[test]
    fn line_selection_at_boundary() {
        // Selection starts and ends at exact line boundaries
        let sel = selection(5, 0, 5, 0);
        // Zero-width selection at start of line
        assert_eq!(get_line_selection_range(&Some(sel), 5), Some((0, 0)));
    }

    #[test]
    fn span_preserves_original_style_fg() {
        use ratatui::style::Color;
        let base_style = Style::default().fg(Color::Red);
        let span = Span::styled("hello", base_style);
        let result = apply_selection_to_span(span, 0, 0, 5);
        // Selected span should keep fg color but add selection bg
        assert_eq!(result[0].style.fg, Some(Color::Red));
        assert_eq!(result[0].style.bg, Some(SELECTION_BG_COLOR));
    }

    #[test]
    fn span_single_char_selection() {
        let span = Span::styled("hello", Style::default());
        let result = apply_selection_to_span(span, 0, 2, 3);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].content, "he");
        assert_eq!(result[1].content, "l");
        assert_eq!(result[1].style.bg, Some(SELECTION_BG_COLOR));
        assert_eq!(result[2].content, "lo");
    }

    #[test]
    fn span_cjk_selection_uses_display_width() {
        // CJK chars: each is 1 char, 3 bytes, 2 display columns.
        // "你好世界" = 4 chars, 12 bytes, 8 display columns.
        // Selection coordinates come from screen positions (display width).
        let span = Span::styled("你好世界", Style::default());

        // Select first 2 display columns = first CJK char "你"
        let result = apply_selection_to_span(span.clone(), 0, 0, 2);
        assert_eq!(result.len(), 2, "should split into selected + unselected");
        assert_eq!(result[0].content, "你");
        assert_eq!(result[0].style.bg, Some(SELECTION_BG_COLOR));
        assert_eq!(result[1].content, "好世界");

        // Select display columns 2-6 = "好世" (columns 2,3 and 4,5)
        let result2 = apply_selection_to_span(span.clone(), 0, 2, 6);
        assert_eq!(result2.len(), 3);
        assert_eq!(result2[0].content, "你");
        assert_eq!(result2[1].content, "好世");
        assert_eq!(result2[1].style.bg, Some(SELECTION_BG_COLOR));
        assert_eq!(result2[2].content, "界");
    }

    #[test]
    fn span_cjk_with_display_width_offset() {
        // Two spans: "ab" (2 display cols) then "你好" (4 display cols).
        // Selecting display columns 3-5 should select "你" from the second span.
        // The second span starts at display offset 2.
        let span = Span::styled("你好", Style::default());
        let result = apply_selection_to_span(span, 2, 3, 5);
        // display offset 2: "你" occupies cols 2-3, "好" occupies cols 4-5
        // selection 3-5: starts mid-"你" (col 3), includes "好" start (col 4)
        // Since we can't split a CJK char, the selection boundary snaps to char edges.
        // "你" starts at col 2, "好" starts at col 4, ends at col 6.
        // sel_start=3 is inside "你", so "你" should be before-selection.
        // sel_end=5 is inside "好", so "好" should be selected.
        // Actually: sel_start=3, in display-width terms relative to span start (offset 2),
        // that's local col 1, which is in the middle of "你" (cols 0-1).
        // The split should include "你" as before and "好" as selected.
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "你");
        assert_eq!(result[0].style.bg, None);
        assert_eq!(result[1].content, "好");
        assert_eq!(result[1].style.bg, Some(SELECTION_BG_COLOR));
    }

    #[test]
    fn span_mixed_ascii_cjk_selection() {
        // "hi你好" = 2 ASCII + 2 CJK = 2+4 = 6 display columns
        let span = Span::styled("hi你好", Style::default());

        // Select display columns 0-4 = "hi你" (2 cols + 2 cols = 4)
        let result = apply_selection_to_span(span.clone(), 0, 0, 4);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "hi你");
        assert_eq!(result[0].style.bg, Some(SELECTION_BG_COLOR));
        assert_eq!(result[1].content, "好");
    }
}
