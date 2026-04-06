use std::io::{self, Write};

use anyhow::Result;
use ratatui::style::{Color, Modifier, Style};

use branchdiff::diff::LineSource;
use branchdiff::output::{OutputData, OutputFile};
use branchdiff::ui::colors::print_line_style as line_style;
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

    let line_num_width = max_line_num_width(&data.files);

    for file in &data.files {
        print_file(&mut stdout, file, line_num_width)?;
    }

    Ok(())
}

fn max_line_num_width(files: &[OutputFile]) -> usize {
    files
        .iter()
        .flat_map(|f| &f.lines)
        .filter_map(|l| l.line_number)
        .max()
        .map(|n| n.to_string().len())
        .unwrap_or(0)
}

fn print_file(stdout: &mut impl Write, file: &OutputFile, line_num_width: usize) -> Result<()> {
    for line in &file.lines {
        let is_moved = line.move_target.is_some();
        let style = if is_moved {
            // Moved lines use magenta (same as canceled — lateral changes)
            Style::default().fg(Color::Magenta)
        } else {
            line_style(line.source)
        };
        let ansi = style_to_ansi(style);

        let line_num_str = if let Some(num) = line.line_number {
            format!("{:>width$}", num, width = line_num_width)
        } else if line_num_width > 0 {
            " ".repeat(line_num_width)
        } else {
            String::new()
        };

        if line.source == LineSource::FileHeader {
            if !line_num_str.is_empty() {
                write!(stdout, "\x1b[90m{} {}", line_num_str, RESET)?;
            }
            writeln!(stdout, "{}── {} ──{}", ansi, line.content, RESET)?;
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
        let style = Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::BOLD);
        let ansi = style_to_ansi(style);
        assert!(ansi.contains("\x1b[31m"));
        assert!(ansi.contains("\x1b[1m"));
    }

    #[test]
    fn test_style_to_ansi_with_dim() {
        let style = Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::DIM);
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
            }],
            total_additions: 1,
            total_deletions: 0,
        };

        let result = print_diff(&data);
        assert!(result.is_ok());
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
