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
