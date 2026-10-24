use std::io::{self, Write};

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::diff::{DiffLine, LineSource, content_display_width};
use crate::html::html_escape;
use crate::ui::ScreenRowInfo;
use crate::ui::wrapping::{wrap_content, wrapped_line_height};

use super::sanitize::printable;
use super::suggestion::SUGGESTION_LABEL;
use super::{AnchoredComment, AnchoredComments};

const COMMENT_HEADER_COLOR: Color = Color::Rgb(110, 200, 235);
const COMMENT_BODY_COLOR: Color = Color::Rgb(225, 228, 234);
const COMMENT_GUTTER_COLOR: Color = Color::Rgb(110, 200, 235);
const SUGGESTION_REMOVED_COLOR: Color = Color::Rgb(255, 107, 102);
const SUGGESTION_ADDED_COLOR: Color = Color::Rgb(63, 185, 80);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Header,
    Body,
    Label,
    Removed,
    Added,
}

/// Every row of text a comment shows, in order. Height and drawing both come
/// from this list, so they cannot disagree.
fn pieces(anchored: &AnchoredComment) -> Vec<(Role, String)> {
    let parts = anchored.parts();
    let mut out = vec![(Role::Header, printable(&anchored.header_label()).into_owned())];
    out.extend(parts.text.iter().map(|t| (Role::Body, printable(t).into_owned())));
    if parts.suggestion.is_some() {
        out.push((Role::Label, SUGGESTION_LABEL.to_string()));
        for (sign, text) in anchored.suggestion_rows() {
            let role = if sign == '-' { Role::Removed } else { Role::Added };
            out.push((role, printable(&format!("{sign} {text}")).into_owned()));
        }
    }
    out
}

fn wrapped_rows_for_text(text: &str, content_width: usize) -> usize {
    let w = content_display_width(text);
    if content_width == 0 || w <= content_width { 1 } else { w.div_ceil(content_width) }
}

/// The comment's rows after its header, as plain sanitized text: prose, then the
/// suggested change as a small diff.
pub(super) fn body_rows(anchored: &AnchoredComment) -> Vec<String> {
    pieces(anchored).into_iter().skip(1).map(|(_, text)| text).collect()
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
        for (_, text) in pieces(anchored) {
            total += wrapped_rows_for_text(&text, content_width);
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
    let label_style = Style::default().fg(COMMENT_BODY_COLOR).add_modifier(Modifier::BOLD);
    let removed_style = Style::default().fg(SUGGESTION_REMOVED_COLOR);
    let added_style = Style::default().fg(SUGGESTION_ADDED_COLOR);

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
        for (role, text) in pieces(anchored) {
            let (gutter, style) = match role {
                Role::Header => (" 💬 ", header_style),
                Role::Body => ("  │ ", body_style),
                Role::Label => ("  │ ", label_style),
                Role::Removed => ("  │ ", removed_style),
                Role::Added => ("  │ ", added_style),
            };
            emit_piece(text, gutter, style);
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
        for (role, text) in pieces(anchored) {
            match role {
                Role::Header => writeln!(out, "{indent}\x1b[36m\x1b[1m  💬 {text}\x1b[0m")?,
                Role::Body => writeln!(out, "{indent}\x1b[36m  │ \x1b[0m {text}")?,
                Role::Label => writeln!(out, "{indent}\x1b[36m  │ \x1b[0m \x1b[1m{text}\x1b[0m")?,
                Role::Removed => {
                    writeln!(out, "{indent}\x1b[36m  │ \x1b[0m \x1b[31m{text}\x1b[0m")?
                }
                Role::Added => writeln!(out, "{indent}\x1b[36m  │ \x1b[0m \x1b[32m{text}\x1b[0m")?,
            }
        }
    }
    Ok(())
}

/// The "Suggested change" block of a comment, or nothing.
fn html_suggestion(anchored: &AnchoredComment) -> String {
    if anchored.parts().suggestion.is_none() {
        return String::new();
    }
    let mut out = format!(
        "<div class=\"comment-suggestion\"><div class=\"suggestion-label\">{SUGGESTION_LABEL}</div>"
    );
    for (sign, text) in anchored.suggestion_rows() {
        let class = if sign == '-' { "sug-del" } else { "sug-add" };
        out.push_str(&format!("<div class=\"{class}\">{} {}</div>", sign, html_escape(&text)));
    }
    out.push_str("</div>");
    out
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
        let parts = anchored.parts();
        let body = html_escape(&parts.text.join("\n"));
        let suggestion = html_suggestion(anchored);
        writeln!(
            out,
            "<tr class=\"comment-row\">{ln}<td class=\"gutter\"></td>\
             <td class=\"comment-cell\"><div class=\"review-comment\">\
             <div class=\"comment-header\">💬 {header}</div>\
             <div class=\"comment-body\">{body}</div>{suggestion}\
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
                suggestion: None,
                outdated: false,
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
                suggestion: None,
                outdated: false,
            }],
        );
        let mut buf = Vec::new();
        write_print_comments(&mut buf, &anchored, 1, 1).unwrap();
        let out = String::from_utf8(buf).unwrap();
        let ours = out.replace("\x1b[36m", "").replace("\x1b[1m", "").replace("\x1b[0m", "");
        assert!(ours.chars().all(|c| !c.is_control() || c == '\n'), "{out:?}");
        assert!(ours.contains("@eve [5m · new line 1"), "{out:?}");
        assert!(ours.contains("title  ]0;PWNED  then  2J"), "{out:?}");
    }

    fn suggestion_fixture(
        side: CommentSide,
        body: &str,
        suggestion: Option<&str>,
    ) -> (Vec<DiffLine>, AnchoredComments) {
        use crate::diff::{DiffInput, compute_four_way_diff};
        let base = "one\ntwo\nthree\nfour\n";
        let working = "one\nTWO\nthree\nfour\n";
        let diff = compute_four_way_diff(DiffInput {
            path: "a.rs",
            base: Some(base),
            head: Some(base),
            index: Some(base),
            working: Some(working),
            old_path: None,
        });
        let comment = ReviewComment {
            path: "a.rs".into(),
            start_line: Some(2),
            end_line: Some(3),
            side,
            body: body.into(),
            author: None,
            suggestion: suggestion.map(str::to_string),
            outdated: false,
        };
        let anchored = anchor_comments(&diff.lines, &[comment]);
        (diff.lines, anchored)
    }

    fn print_text(anchored: &AnchoredComments, idx: usize) -> String {
        let mut buf = Vec::new();
        write_print_comments(&mut buf, anchored, idx, 1).unwrap();
        String::from_utf8(buf)
            .unwrap()
            .replace("\x1b[36m", "")
            .replace("\x1b[1m", "")
            .replace("\x1b[31m", "")
            .replace("\x1b[32m", "")
            .replace("\x1b[0m", "")
    }

    #[test]
    fn a_suggestion_replaces_the_covered_new_lines_in_print() {
        let (lines, anchored) = suggestion_fixture(
            CommentSide::New,
            "Rename it.\n```suggestion\nTwo\nthree!\n```",
            None,
        );
        let idx = (0..lines.len()).find(|i| anchored.for_line(*i).is_some()).unwrap();
        let out = print_text(&anchored, idx);
        let rows: Vec<&str> = out.lines().map(str::trim_start).collect();
        assert_eq!(
            rows[1..],
            [
                "│  Rename it.", "│  Suggested change", "│  - TWO", "│  - three", "│  + Two",
                "│  + three!"
            ],
            "{out}"
        );
    }

    #[test]
    fn the_json_field_shows_the_same_as_a_fence_and_old_side_replaces_old_text() {
        let (lines, fenced) = suggestion_fixture(CommentSide::New, "```suggestion\nX\n```", None);
        let (_, field) = suggestion_fixture(CommentSide::New, "", Some("X"));
        let idx = (0..lines.len()).find(|i| fenced.for_line(*i).is_some()).unwrap();
        let rows = |a: &AnchoredComments| a.for_line(idx).unwrap()[0].suggestion_rows();
        assert_eq!(rows(&fenced), rows(&field));
        let (_, old) = suggestion_fixture(CommentSide::Old, "", Some("X"));
        assert_eq!(old.for_line(idx).unwrap()[0].suggestion_rows()[0], ('-', "two".to_string()));
    }

    #[test]
    fn a_comment_without_a_suggestion_has_no_label() {
        let (lines, anchored) = suggestion_fixture(CommentSide::New, "plain ```text```", None);
        let idx = (0..lines.len()).find(|i| anchored.for_line(*i).is_some()).unwrap();
        assert!(!print_text(&anchored, idx).contains(SUGGESTION_LABEL));
    }

    #[test]
    fn the_tui_height_counts_every_suggestion_row() {
        let (lines, anchored) = suggestion_fixture(
            CommentSide::New,
            "Rename it.\n```suggestion\nTwo\nthree!\n```",
            None,
        );
        let idx = (0..lines.len()).find(|i| anchored.for_line(*i).is_some()).unwrap();
        // header, one prose line, label, two removed, two added
        assert_eq!(tui_comment_rows_for_line(&anchored, idx, 80), 1 + 1 + 1 + 2 + 2);
        // a narrow width wraps the longer rows
        assert!(tui_comment_rows_for_line(&anchored, idx, 8) > 7);
    }

    #[test]
    fn html_shows_the_suggestion_as_a_small_diff_and_escapes_it() {
        let (lines, anchored) =
            suggestion_fixture(CommentSide::New, "```suggestion\n<b>&\n```", None);
        let idx = (0..lines.len()).find(|i| anchored.for_line(*i).is_some()).unwrap();
        let mut buf = Vec::new();
        write_html_comments(&mut buf, &anchored, idx, 1).unwrap();
        let html = String::from_utf8(buf).unwrap();
        assert!(html.contains("<div class=\"suggestion-label\">Suggested change</div>"), "{html}");
        assert!(html.contains("<div class=\"sug-del\">- TWO</div>"), "{html}");
        assert!(html.contains("<div class=\"sug-add\">+ &lt;b&gt;&amp;</div>"), "{html}");
        assert!(!html.contains("```"), "{html}");
    }
}
