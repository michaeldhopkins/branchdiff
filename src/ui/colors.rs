use ratatui::style::{Color, Modifier, Style};

use crate::diff::LineSource;

/// Get the style for a line based on its source
pub fn line_style(source: LineSource) -> Style {
    match source {
        LineSource::Base => Style::default().fg(Color::DarkGray),
        LineSource::Committed => Style::default().fg(Color::Cyan),
        LineSource::Staged => Style::default().fg(Color::Green),
        LineSource::Unstaged => Style::default().fg(Color::Yellow),
        LineSource::DeletedBase => Style::default().fg(Color::Red),
        LineSource::DeletedCommitted => Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::DIM),
        LineSource::DeletedStaged => Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::DIM),
        LineSource::FileHeader => Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
        LineSource::Elided => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    }
}
