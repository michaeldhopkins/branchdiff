use std::io::{self, Write};

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::diff::content_display_width;
use crate::html::html_escape;
use crate::ui::ScreenRowInfo;
use crate::ui::wrapping::wrap_content;

use super::AnchoredComments;

const COMMENT_HEADER_COLOR: Color = Color::Rgb(110, 200, 235);
const COMMENT_BODY_COLOR: Color = Color::Rgb(225, 228, 234);
const COMMENT_GUTTER_COLOR: Color = Color::Rgb(110, 200, 235);

fn body_lines(body: &str) -> Vec<&str> {
    if body.is_empty() { vec![""] } else { body.lines().collect() }
}

fn wrapped_rows_for_text(text: &str, content_width: usize) -> usize {
    let w = content_display_width(text);
    if content_width == 0 || w <= content_width { 1 } else { w.div_ceil(content_width) }
}

/// Number of screen rows required to render all comments attached to `line_idx`
/// at `content_width`. Must match `render_tui_comments`.
pub fn tui_comment_rows_for_line(
    comments: &AnchoredComments,
    line_idx: usize,
    content_width: usize,
) -> usize {
    let Some(list) = comments.for_line(line_idx) else {
        return 0;
    };
    let mut total = 0;
    for anchored in list {
        let header_text = anchored.header_label();
        total += wrapped_rows_for_text(&header_text, content_width);
        for raw in body_lines(&anchored.comment.body) {
            total += wrapped_rows_for_text(raw, content_width);
        }
    }
    total
}

/// Parameters for rendering TUI comments below a diff line.
pub struct TuiCommentRenderSpec<'a> {
    pub line_num_width: usize,
    pub prefix_width: usize,
    pub content_width: usize,
    pub screen_row_idx: usize,
    pub max_rows: usize,
    pub skip_rows: usize,
    pub file_path: Option<&'a str>,
}

/// Render comments attached to `line_idx` into `all_lines` and `all_row_infos`,
/// returning the number of screen rows emitted.
pub fn render_tui_comments(
    comments: &AnchoredComments,
    line_idx: usize,
    spec: &TuiCommentRenderSpec<'_>,
    all_lines: &mut Vec<Line<'static>>,
    all_row_infos: &mut Vec<ScreenRowInfo>,
) -> usize {
    let Some(list) = comments.for_line(line_idx) else {
        return 0;
    };
    if all_lines.len() >= spec.max_rows {
        return 0;
    }

    let blank_prefix =
        if spec.line_num_width > 0 { " ".repeat(spec.line_num_width + 1) } else { String::new() };
    let gutter_style = Style::default().fg(COMMENT_GUTTER_COLOR);
    let header_style = Style::default().fg(COMMENT_HEADER_COLOR).add_modifier(Modifier::BOLD);
    let body_style = Style::default().fg(COMMENT_BODY_COLOR);

    let mut remaining_skip = spec.skip_rows;
    let mut emitted = 0usize;

    let mut emit_piece = |text: String, gutter: &str, style: Style| {
        if spec.screen_row_idx + emitted >= spec.max_rows {
            return;
        }
        let piece_rows = wrapped_rows_for_text(&text, spec.content_width);
        if remaining_skip >= piece_rows {
            remaining_skip -= piece_rows;
            return;
        }
        let max_for_piece = spec.max_rows.saturating_sub(spec.screen_row_idx + emitted).max(1);
        let (lines, mut infos) = wrap_content(
            vec![Span::styled(text.clone(), style)],
            &text,
            blank_prefix.clone(),
            gutter.to_string(),
            gutter_style,
            spec.content_width,
            spec.prefix_width,
            remaining_skip,
            max_for_piece,
        );
        remaining_skip = 0;
        for info in &mut infos {
            info.file_path = spec.file_path.map(str::to_string);
        }
        emitted += lines.len();
        all_lines.extend(lines);
        all_row_infos.extend(infos);
    };

    for anchored in list {
        emit_piece(anchored.header_label(), " 💬 ", header_style);
        for raw in body_lines(&anchored.comment.body) {
            emit_piece(raw.to_string(), "  │ ", body_style);
        }
    }

    emitted
}

/// Write ANSI-styled review comments for `line_idx` in `--print` mode.
pub fn write_print_comments(
    out: &mut impl Write,
    comments: &AnchoredComments,
    line_idx: usize,
    line_num_width: usize,
) -> io::Result<()> {
    let Some(list) = comments.for_line(line_idx) else {
        return Ok(());
    };
    let indent = if line_num_width > 0 { " ".repeat(line_num_width + 1) } else { String::new() };
    for anchored in list {
        writeln!(out, "{indent}\x1b[36m\x1b[1m  💬 {}\x1b[0m", anchored.header_label())?;
        for raw in body_lines(&anchored.comment.body) {
            writeln!(out, "{indent}\x1b[36m  │ \x1b[0m {raw}")?;
        }
    }
    Ok(())
}

/// Write HTML table rows for comments attached to `line_idx` in `--html` mode.
pub fn write_html_comments(
    out: &mut impl Write,
    comments: &AnchoredComments,
    line_idx: usize,
) -> io::Result<()> {
    let Some(list) = comments.for_line(line_idx) else {
        return Ok(());
    };
    for anchored in list {
        let header = html_escape(&anchored.header_label());
        let body = html_escape(&anchored.comment.body);
        writeln!(
            out,
            "<tr class=\"comment-row\"><td colspan=\"3\" class=\"comment-cell\">\
             <div class=\"review-comment\">\
             <div class=\"comment-header\">💬 {header}</div>\
             <div class=\"comment-body\">{body}</div>\
             </div></td></tr>"
        )?;
    }
    Ok(())
}
