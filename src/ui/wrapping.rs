use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

use super::{ScreenRowInfo, ScreenRowKind};

/// Wrap content spans into multiple lines if needed, returning Lines and ScreenRowInfo entries
pub fn wrap_content(
    content_spans: Vec<Span<'static>>,
    content: &str,
    prefix_str: String,
    prefix_char: String,
    style: Style,
    content_width: usize,
    prefix_width: usize,
    logical_idx: usize,
    kind: ScreenRowKind,
) -> (Vec<Line<'static>>, Vec<ScreenRowInfo>) {
    let content_len: usize = content_spans.iter().map(|s| s.content.len()).sum();

    // If content fits, no wrapping needed
    if content_len <= content_width {
        let mut spans = Vec::new();
        spans.push(Span::styled(prefix_str, Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(prefix_char, style));
        spans.extend(content_spans);

        let row_info = ScreenRowInfo {
            logical_idx,
            kind,
            content: content.to_string(),
        };

        return (vec![Line::from(spans)], vec![row_info]);
    }

    // Need to wrap - split content into chunks
    let mut result_lines = Vec::new();
    let mut row_infos = Vec::new();
    let mut current_line_spans = Vec::new();
    let mut current_width = 0;
    let mut is_first_line = true;
    let mut current_content = String::new();

    // Continuation line indent (same width as "123 + ")
    let continuation_indent = " ".repeat(prefix_width);

    for span in content_spans {
        let span_text = span.content.to_string();
        let span_style = span.style;
        let mut remaining = span_text.as_str();

        while !remaining.is_empty() {
            let space_available = content_width.saturating_sub(current_width);

            if space_available == 0 {
                // Emit current line and start new one
                let mut line_spans = Vec::new();
                let row_kind = if is_first_line { kind } else { ScreenRowKind::WrappedContinuation };

                if is_first_line {
                    line_spans.push(Span::styled(prefix_str.clone(), Style::default().fg(Color::DarkGray)));
                    line_spans.push(Span::styled(prefix_char.clone(), style));
                    is_first_line = false;
                } else {
                    line_spans.push(Span::styled(continuation_indent.clone(), Style::default().fg(Color::DarkGray)));
                }
                line_spans.extend(current_line_spans.drain(..));
                result_lines.push(Line::from(line_spans));

                row_infos.push(ScreenRowInfo {
                    logical_idx,
                    kind: row_kind,
                    content: std::mem::take(&mut current_content),
                });

                current_width = 0;
                continue;
            }

            if remaining.len() <= space_available {
                // Entire remaining text fits
                current_line_spans.push(Span::styled(remaining.to_string(), span_style));
                current_content.push_str(remaining);
                current_width += remaining.len();
                remaining = "";
            } else {
                // Need to split
                let (chunk, rest) = remaining.split_at(space_available);
                current_line_spans.push(Span::styled(chunk.to_string(), span_style));
                current_content.push_str(chunk);
                remaining = rest;

                // Emit current line
                let mut line_spans = Vec::new();
                let row_kind = if is_first_line { kind } else { ScreenRowKind::WrappedContinuation };

                if is_first_line {
                    line_spans.push(Span::styled(prefix_str.clone(), Style::default().fg(Color::DarkGray)));
                    line_spans.push(Span::styled(prefix_char.clone(), style));
                    is_first_line = false;
                } else {
                    line_spans.push(Span::styled(continuation_indent.clone(), Style::default().fg(Color::DarkGray)));
                }
                line_spans.extend(current_line_spans.drain(..));
                result_lines.push(Line::from(line_spans));

                row_infos.push(ScreenRowInfo {
                    logical_idx,
                    kind: row_kind,
                    content: std::mem::take(&mut current_content),
                });

                current_width = 0;
            }
        }
    }

    // Emit any remaining content
    if !current_line_spans.is_empty() || is_first_line {
        let mut line_spans = Vec::new();
        let row_kind = if is_first_line { kind } else { ScreenRowKind::WrappedContinuation };

        if is_first_line {
            line_spans.push(Span::styled(prefix_str, Style::default().fg(Color::DarkGray)));
            line_spans.push(Span::styled(prefix_char, style));
        } else {
            line_spans.push(Span::styled(continuation_indent, Style::default().fg(Color::DarkGray)));
        }
        line_spans.extend(current_line_spans);
        result_lines.push(Line::from(line_spans));

        row_infos.push(ScreenRowInfo {
            logical_idx,
            kind: row_kind,
            content: current_content,
        });
    }

    (result_lines, row_infos)
}
