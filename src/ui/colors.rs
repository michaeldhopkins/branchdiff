use ratatui::style::{Color, Modifier, Style};

use crate::diff::LineSource;

/// Get background highlight color for changed portions in multiline diff display
pub fn highlight_bg_color(source: LineSource) -> Color {
    match source {
        LineSource::DeletedBase => Color::Rgb(45, 22, 22), // Lighter red for committed deletions
        LineSource::DeletedCommitted | LineSource::DeletedStaged => {
            Color::Rgb(100, 50, 50) // Bold red background
        }
        LineSource::Committed => Color::Rgb(50, 100, 100), // Bold cyan background
        LineSource::Staged => Color::Rgb(50, 100, 50),     // Bold green background
        LineSource::Unstaged => Color::Rgb(100, 100, 50),  // Bold yellow background
        _ => Color::Reset,
    }
}

/// Get style with background highlight for changed portions
pub fn line_style_with_highlight(source: LineSource) -> Style {
    line_style(source).bg(highlight_bg_color(source))
}

/// Get the style for a line based on its source
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
