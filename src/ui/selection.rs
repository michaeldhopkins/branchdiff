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

/// Apply selection highlighting to a span, splitting it if partially selected
pub fn apply_selection_to_span(
    span: Span<'static>,
    char_offset: usize,
    sel_start: usize,
    sel_end: usize,
) -> Vec<Span<'static>> {
    let text = span.content.to_string();
    let text_len = text.len();
    let text_start = char_offset;
    let text_end = char_offset + text_len;

    // Check if there's any overlap with selection
    if text_end <= sel_start || text_start >= sel_end {
        // No overlap - return original span
        return vec![span];
    }

    let mut result = Vec::new();
    let base_style = span.style;
    let selected_style = base_style.bg(SELECTION_BG_COLOR);

    // Before selection
    if text_start < sel_start {
        let before_end = (sel_start - text_start).min(text_len);
        let before_text: String = text.chars().take(before_end).collect();
        if !before_text.is_empty() {
            result.push(Span::styled(before_text, base_style));
        }
    }

    // Selected portion
    let sel_in_text_start = sel_start.saturating_sub(text_start);
    let sel_in_text_end = (sel_end - text_start).min(text_len);
    if sel_in_text_start < sel_in_text_end {
        let selected_text: String = text.chars()
            .skip(sel_in_text_start)
            .take(sel_in_text_end - sel_in_text_start)
            .collect();
        if !selected_text.is_empty() {
            result.push(Span::styled(selected_text, selected_style));
        }
    }

    // After selection
    if text_end > sel_end {
        let after_start = sel_end.saturating_sub(text_start);
        let after_text: String = text.chars().skip(after_start).collect();
        if !after_text.is_empty() {
            result.push(Span::styled(after_text, base_style));
        }
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
}
