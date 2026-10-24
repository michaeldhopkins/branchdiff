use std::io::{self, Write};

use anyhow::Result;
use ratatui::style::{Color, Modifier, Style};

use branchdiff::comments::render::write_print_comments;
use branchdiff::diff::LineSource;
use branchdiff::output::{OutputData, OutputFile};
use branchdiff::ui::colors::print_line_style as line_style;
use branchdiff::ui::gutter::Gutter;
use branchdiff::ui::spans::coalesce_spans;

const RESET: &str = "\x1b[0m";

fn color_to_ansi(color: Color) -> Option<&'static str> {
    match color {
        Color::Black => Some("\x1b[30m"),
        Color::Red => Some("\x1b[31m"),
        Color::Green => Some("\x1b[32m"),
        Color::Yellow => Some("\x1b[33m"),
        Color::Blue => Some("\x1b[34m"),
        Color::Magenta => Some("\x1b[35m"),
        Color::Cyan => Some("\x1b[36m"),
        Color::Gray => Some("\x1b[37m"),
        Color::DarkGray => Some("\x1b[90m"),
        Color::LightRed => Some("\x1b[91m"),
        Color::LightGreen => Some("\x1b[92m"),
        Color::LightYellow => Some("\x1b[93m"),
        Color::LightBlue => Some("\x1b[94m"),
        Color::LightMagenta => Some("\x1b[95m"),
        Color::LightCyan => Some("\x1b[96m"),
        Color::White => Some("\x1b[97m"),
        _ => None,
    }
}

fn style_to_ansi(style: Style) -> String {
    let mut codes = Vec::new();

    if let Some(fg) = style.fg
        && let Some(code) = color_to_ansi(fg)
    {
        codes.push(code);
    }

    if style.add_modifier.contains(Modifier::BOLD) {
        codes.push("\x1b[1m");
    }
    if style.add_modifier.contains(Modifier::DIM) {
        codes.push("\x1b[2m");
    }

    codes.join("")
}

pub fn print_diff(data: &OutputData) -> Result<()> {
    let mut stdout = io::stdout().lock();

    let branch_info = format!("{} | {} vs {}", data.repo_name, data.to_label, data.from_label);
    let file_count = data.files.len();
    let line_count = data.total_additions + data.total_deletions;

    let status = format!(
        "{} | {} file{} | {} line{}",
        branch_info,
        file_count,
        if file_count == 1 { "" } else { "s" },
        line_count,
        if line_count == 1 { "" } else { "s" },
    );

    writeln!(stdout, "\x1b[36m{}{}", status, RESET)?;
    writeln!(stdout)?;

    let gutter = Gutter::measure(data.files.iter().flat_map(|f| &f.lines), None);

    for file in &data.files {
        print_file(&mut stdout, file, gutter)?;
    }

    Ok(())
}

fn print_file(stdout: &mut impl Write, file: &OutputFile, gutter: Gutter) -> Result<()> {
    let line_num_width = gutter.print_width();
    for (line_idx, line) in file.lines.iter().enumerate() {
        let is_moved = line.move_target.is_some();
        let style = if is_moved {
            // Moved lines use magenta (same as canceled — lateral changes)
            Style::default().fg(Color::Magenta)
        } else {
            line_style(line.source)
        };
        let ansi = style_to_ansi(style);

        let line_num_str = gutter.print_cell(line);

        if line.source == LineSource::FileHeader {
            if !line_num_str.is_empty() {
                write!(stdout, "\x1b[90m{} {}", line_num_str, RESET)?;
            }
            writeln!(stdout, "{}── {} ──{}", ansi, line.content, RESET)?;
            write_print_comments(stdout, &file.comments, line_idx, line_num_width)?;
            continue;
        }

        if line.source == LineSource::Elided {
            if !line_num_str.is_empty() {
                write!(stdout, "\x1b[90m{} {}", line_num_str, RESET)?;
            }
            writeln!(stdout, "{}┈┈ ⋮ {} ⋮ ┈┈{}", ansi, line.content, RESET)?;
            continue;
        }

        if !line_num_str.is_empty() {
            write!(stdout, "\x1b[90m{} {}", line_num_str, RESET)?;
        }

        let prefix = if is_moved { 'M' } else { line.prefix };
        write!(stdout, "{}{} ", ansi, prefix)?;

        if !line.inline_spans.is_empty() {
            let display_spans = coalesce_spans(&line.inline_spans);
            for span in display_spans {
                let span_style = match span.source {
                    Some(source) => line_style(source),
                    None => style,
                };
                let span_ansi = style_to_ansi(span_style);
                write!(stdout, "{}{}{}", span_ansi, span.text, RESET)?;
            }
            writeln!(stdout)?;
        } else {
            writeln!(stdout, "{}{}", line.content, RESET)?;
        }
        write_print_comments(stdout, &file.comments, line_idx, line_num_width)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use branchdiff::diff::DiffLine;

    #[test]
    fn test_color_to_ansi_basic_colors() {
        assert_eq!(color_to_ansi(Color::Red), Some("\x1b[31m"));
        assert_eq!(color_to_ansi(Color::Green), Some("\x1b[32m"));
        assert_eq!(color_to_ansi(Color::Yellow), Some("\x1b[33m"));
        assert_eq!(color_to_ansi(Color::Cyan), Some("\x1b[36m"));
        assert_eq!(color_to_ansi(Color::DarkGray), Some("\x1b[90m"));
        assert_eq!(color_to_ansi(Color::White), Some("\x1b[97m"));
        assert_eq!(color_to_ansi(Color::Magenta), Some("\x1b[35m"));
    }

    #[test]
    fn test_style_to_ansi_with_color() {
        let style = Style::default().fg(Color::Cyan);
        assert_eq!(style_to_ansi(style), "\x1b[36m");
    }

    #[test]
    fn test_style_to_ansi_with_modifiers() {
        let style = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
        let ansi = style_to_ansi(style);
        assert!(ansi.contains("\x1b[31m"));
        assert!(ansi.contains("\x1b[1m"));
    }

    #[test]
    fn test_style_to_ansi_with_dim() {
        let style = Style::default().fg(Color::Red).add_modifier(Modifier::DIM);
        let ansi = style_to_ansi(style);
        assert!(ansi.contains("\x1b[31m"));
        assert!(ansi.contains("\x1b[2m"));
    }

    #[test]
    fn test_style_to_ansi_empty() {
        let style = Style::default();
        assert_eq!(style_to_ansi(style), "");
    }

    #[test]
    fn test_print_diff_produces_output() {
        let data = OutputData {
            repo_name: "test".to_string(),
            to_label: "feature".to_string(),
            from_label: "main".to_string(),
            files: vec![OutputFile {
                path: "test.rs".to_string(),
                lines: vec![
                    DiffLine::file_header("test.rs"),
                    DiffLine::new(LineSource::Base, "unchanged".to_string(), ' ', Some(1)),
                    DiffLine::new(LineSource::Committed, "added".to_string(), '+', Some(2)),
                ],
                additions: 1,
                deletions: 0,
                collapsed: false,
                comments: Default::default(),
            }],
            total_additions: 1,
            total_deletions: 0,
        };

        let result = print_diff(&data);
        assert!(result.is_ok());
    }

    #[test]
    fn test_print_file_writes_comments_under_anchored_lines() {
        use branchdiff::comments::{CommentSide, ReviewComment, anchor_comments};

        let lines = vec![
            DiffLine::file_header("src/lib.rs"),
            DiffLine::new(LineSource::DeletedBase, "old_call()".to_string(), '-', None)
                .with_old_line_number(4)
                .with_file_path("src/lib.rs"),
            DiffLine::new(LineSource::Committed, "new_call()".to_string(), '+', Some(4))
                .with_file_path("src/lib.rs"),
        ];
        let comments = anchor_comments(
            &lines,
            &[
                ReviewComment {
                    path: "src/lib.rs".into(),
                    start_line: None,
                    end_line: None,
                    side: CommentSide::New,
                    body: "Header comment body".into(),
                    author: Some("alice".into()),
                },
                ReviewComment {
                    path: "src/lib.rs".into(),
                    start_line: Some(4),
                    end_line: Some(4),
                    side: CommentSide::Old,
                    body: "Deleted line comment".into(),
                    author: None,
                },
                ReviewComment {
                    path: "src/lib.rs".into(),
                    start_line: Some(4),
                    end_line: Some(4),
                    side: CommentSide::New,
                    body: "Added line comment".into(),
                    author: Some("bob".into()),
                },
            ],
        );
        let file = OutputFile {
            path: "src/lib.rs".into(),
            lines,
            additions: 1,
            deletions: 1,
            collapsed: false,
            comments,
        };

        let mut buf = Vec::new();
        print_file(&mut buf, &file, Gutter::measure(&file.lines, None)).unwrap();
        let out = String::from_utf8(buf).unwrap();

        let header_pos = out.find("── src/lib.rs ──").unwrap();
        let file_comment_pos = out.find("Header comment body").unwrap();
        let old_line_pos = out.find("old_call()").unwrap();
        let old_comment_pos = out.find("Deleted line comment").unwrap();
        let new_line_pos = out.find("new_call()").unwrap();
        let new_comment_pos = out.find("Added line comment").unwrap();

        assert!(header_pos < file_comment_pos && file_comment_pos < old_line_pos, "{out}");
        assert!(old_line_pos < old_comment_pos && old_comment_pos < new_line_pos, "{out}");
        assert!(new_line_pos < new_comment_pos, "{out}");
    }

    fn strip_ansi(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for d in chars.by_ref() {
                    if d == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn print_shows_old_and_new_numbers_in_two_columns() {
        let lines = vec![
            DiffLine::file_header("a.rs"),
            DiffLine::new(LineSource::Base, "same".into(), ' ', Some(9)).with_old_line_number(8),
            DiffLine::new(LineSource::DeletedBase, "gone".into(), '-', None)
                .with_old_line_number(9),
            DiffLine::new(LineSource::Committed, "fresh".into(), '+', Some(10)),
        ];
        let file = OutputFile {
            path: "a.rs".into(),
            lines,
            additions: 1,
            deletions: 1,
            collapsed: false,
            comments: Default::default(),
        };
        let mut buf = Vec::new();
        print_file(&mut buf, &file, Gutter::measure(&file.lines, None)).unwrap();
        let out = strip_ansi(&String::from_utf8(buf).unwrap());
        let rows: Vec<&str> = out.lines().collect();
        assert_eq!(rows[1], "8  9   same", "{out}");
        assert_eq!(rows[2], "9    - gone", "{out}");
        assert_eq!(rows[3], "  10 + fresh", "{out}");
    }

    #[test]
    fn print_without_old_numbers_keeps_the_single_column() {
        let lines = vec![DiffLine::new(LineSource::Committed, "fresh".into(), '+', Some(10))];
        let file = OutputFile {
            path: "a.rs".into(),
            lines,
            additions: 1,
            deletions: 0,
            collapsed: false,
            comments: Default::default(),
        };
        let mut buf = Vec::new();
        print_file(&mut buf, &file, Gutter::measure(&file.lines, None)).unwrap();
        assert_eq!(strip_ansi(&String::from_utf8(buf).unwrap()), "10 + fresh\n");
    }

    #[test]
    fn test_line_style_produces_correct_ansi() {
        let committed_style = line_style(LineSource::Committed);
        assert_eq!(committed_style.fg, Some(Color::Cyan));

        let staged_style = line_style(LineSource::Staged);
        assert_eq!(staged_style.fg, Some(Color::Green));

        let unstaged_style = line_style(LineSource::Unstaged);
        assert_eq!(unstaged_style.fg, Some(Color::Yellow));

        let deleted_style = line_style(LineSource::DeletedBase);
        assert_eq!(deleted_style.fg, Some(Color::Red));

        let file_header_style = line_style(LineSource::FileHeader);
        assert_eq!(file_header_style.fg, Some(Color::White));
        assert!(file_header_style.add_modifier.contains(Modifier::BOLD));
    }
}
