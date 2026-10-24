use std::io::{self, Write};

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::diff::{DiffLine, LineSource, content_display_width};
use crate::html::html_escape;
use crate::ui::ScreenRowInfo;
use crate::ui::wrapping::{wrap_content, wrapped_line_height};

use super::AnchoredComments;
use super::sanitize::printable;

const COMMENT_HEADER_COLOR: Color = Color::Rgb(110, 200, 235);
const COMMENT_BODY_COLOR: Color = Color::Rgb(225, 228, 234);
const COMMENT_GUTTER_COLOR: Color = Color::Rgb(110, 200, 235);

pub(super) fn body_lines(body: &str) -> Vec<&str> {
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
    pub diff_line: &'a DiffLine,
    pub line_rows: usize,
    pub line_num_width: usize,
    pub prefix_width: usize,
    pub content_width: usize,
    pub screen_row_idx: usize,
    pub max_rows: usize,
    pub skip_rows: usize,
    pub area_width: u16,
    pub font_size: (u16, u16),
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

    let line_full_rows = if spec.diff_line.source == LineSource::FileHeader
        || spec.diff_line.source == LineSource::Elided
    {
        1
    } else {
        wrapped_line_height(
            spec.diff_line, spec.content_width, None, spec.area_width, spec.font_size,
        )
    };
    let start_screen_row = spec.screen_row_idx + spec.line_rows;
    let file_path = spec.diff_line.file_path.as_deref();

    let blank_prefix =
        if spec.line_num_width > 0 { " ".repeat(spec.line_num_width + 1) } else { String::new() };
    let gutter_style = Style::default().fg(COMMENT_GUTTER_COLOR);
    let header_style = Style::default().fg(COMMENT_HEADER_COLOR).add_modifier(Modifier::BOLD);
    let body_style = Style::default().fg(COMMENT_BODY_COLOR);

    let mut remaining_skip = spec.skip_rows.saturating_sub(line_full_rows);
    let mut emitted = 0usize;

    let mut emit_piece = |text: String, gutter: &str, style: Style| {
        if start_screen_row + emitted >= spec.max_rows {
            return;
        }
        let piece_rows = wrapped_rows_for_text(&text, spec.content_width);
        if remaining_skip >= piece_rows {
            remaining_skip -= piece_rows;
            return;
        }
        let max_for_piece = spec.max_rows.saturating_sub(start_screen_row + emitted).max(1);
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
            info.file_path = file_path.map(str::to_string);
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
        let header = anchored.header_label();
        writeln!(out, "{indent}\x1b[36m\x1b[1m  💬 {}\x1b[0m", printable(&header))?;
        for raw in body_lines(&anchored.comment.body) {
            writeln!(out, "{indent}\x1b[36m  │ \x1b[0m {}", printable(raw))?;
        }
    }
    Ok(())
}

/// Write HTML table rows for comments attached to `line_idx` in `--html` mode.
pub fn write_html_comments(
    out: &mut impl Write,
    comments: &AnchoredComments,
    line_idx: usize,
    ln_cells: usize,
) -> io::Result<()> {
    let Some(list) = comments.for_line(line_idx) else {
        return Ok(());
    };
    let ln = "<td class=\"ln\"></td>".repeat(ln_cells);
    for anchored in list {
        let header = html_escape(&anchored.header_label());
        let body = html_escape(&anchored.comment.body);
        writeln!(
            out,
            "<tr class=\"comment-row\">{ln}<td class=\"gutter\"></td>\
             <td class=\"comment-cell\"><div class=\"review-comment\">\
             <div class=\"comment-header\">💬 {header}</div>\
             <div class=\"comment-body\">{body}</div>\
             </div></td></tr>"
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comments::{CommentSide, ReviewComment, anchor_comments};

    #[test]
    fn html_comment_rows_span_every_number_cell() {
        let lines = vec![DiffLine::file_header("a.rs")];
        let anchored = anchor_comments(
            &lines,
            &[ReviewComment {
                path: "a.rs".into(),
                start_line: None,
                end_line: None,
                side: CommentSide::New,
                body: "note".into(),
                author: None,
            }],
        );
        for (cells, expected) in [(1, 1), (2, 2)] {
            let mut buf = Vec::new();
            write_html_comments(&mut buf, &anchored, 0, cells).unwrap();
            let html = String::from_utf8(buf).unwrap();
            assert_eq!(html.matches("<td class=\"ln\"></td>").count(), expected, "{html}");
        }
    }

    #[test]
    fn print_comments_strip_escape_sequences_from_body_and_author() {
        let lines = vec![
            DiffLine::file_header("a.rs"),
            DiffLine::new(LineSource::Committed, "x".into(), '+', Some(1)).with_file_path("a.rs"),
        ];
        let anchored = anchor_comments(
            &lines,
            &[ReviewComment {
                path: "a.rs".into(),
                start_line: Some(1),
                end_line: Some(1),
                side: CommentSide::New,
                body: "title \x1b]0;PWNED\x07 then \u{9b}2J".into(),
                author: Some("eve\x1b[5m".into()),
            }],
        );
        let mut buf = Vec::new();
        write_print_comments(&mut buf, &anchored, 1, 1).unwrap();
        let out = String::from_utf8(buf).unwrap();
        let ours = out.replace("\x1b[36m", "").replace("\x1b[1m", "").replace("\x1b[0m", "");
        assert!(ours.chars().all(|c| !c.is_control() || c == '\n'), "{out:?}");
        assert!(ours.contains("@eve [5m · line 1 (+)"), "{out:?}");
        assert!(ours.contains("title  ]0;PWNED  then  2J"), "{out:?}");
    }
}
