use ratatui::style::{Color, Modifier, Style};

use crate::diff::LineSource;

pub fn highlight_bg_color(source: LineSource) -> Color {
    match source {
        LineSource::DeletedBase => Color::Rgb(45, 22, 22),
        LineSource::DeletedCommitted | LineSource::DeletedStaged => Color::Rgb(100, 50, 50),
        LineSource::Committed => Color::Rgb(50, 100, 100),
        LineSource::Staged => Color::Rgb(50, 100, 50),
        LineSource::Unstaged => Color::Rgb(100, 100, 50),
        _ => Color::Reset,
    }
}

pub fn line_style_with_highlight(source: LineSource) -> Style {
    line_style(source).bg(highlight_bg_color(source))
}

pub fn line_style(source: LineSource) -> Style {
    match source {
        LineSource::Base => Style::default().fg(Color::DarkGray),
        LineSource::Committed => Style::default().fg(Color::Cyan),
        LineSource::Staged => Style::default().fg(Color::Green),
        LineSource::Unstaged => Style::default().fg(Color::Yellow),
        LineSource::DeletedBase => Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::DIM),
        LineSource::DeletedCommitted => Style::default().fg(Color::Red),
        LineSource::DeletedStaged => Style::default().fg(Color::Red),
        LineSource::CanceledCommitted => Style::default().fg(Color::Magenta),
        LineSource::CanceledStaged => Style::default().fg(Color::Magenta),
        LineSource::FileHeader => Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
        LineSource::Elided => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    }
}
