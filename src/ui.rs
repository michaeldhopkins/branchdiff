use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::App;
use crate::diff::LineSource;

/// Color scheme for different line sources
fn line_style(source: LineSource) -> Style {
    match source {
        LineSource::Base => Style::default().fg(Color::DarkGray),
        LineSource::Committed => Style::default().fg(Color::Cyan),
        LineSource::Staged => Style::default().fg(Color::Green),
        LineSource::Unstaged => Style::default().fg(Color::Yellow),
        LineSource::DeletedBase => Style::default().fg(Color::Red),
        LineSource::DeletedCommitted => Style::default().fg(Color::LightRed),
        LineSource::DeletedStaged => Style::default().fg(Color::Rgb(255, 150, 150)),
        LineSource::FileHeader => Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    }
}

/// Draw the main UI
pub fn draw(frame: &mut Frame, app: &mut App) {
    let size = frame.area();

    // Layout: main content area + status bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // Main diff view
            Constraint::Length(1), // Status bar
        ])
        .split(size);

    // Update viewport height in app
    let content_height = chunks[0].height.saturating_sub(2) as usize; // -2 for borders
    app.set_viewport_height(content_height);

    // Draw main diff view
    draw_diff_view(frame, app, chunks[0]);

    // Draw status bar
    draw_status_bar(frame, app, chunks[1]);
}

/// Draw the diff content
fn draw_diff_view(frame: &mut Frame, app: &App, area: Rect) {
    let visible_lines = app.visible_lines();

    // Calculate the width needed for line numbers
    let max_line_num = visible_lines
        .iter()
        .filter_map(|l| l.line_number)
        .max()
        .unwrap_or(0);
    let line_num_width = if max_line_num > 0 {
        max_line_num.to_string().len() + 1
    } else {
        0
    };

    // Build display lines
    let lines: Vec<Line> = visible_lines
        .iter()
        .map(|diff_line| {
            let style = line_style(diff_line.source);

            // Build the line with optional line number
            let mut spans = Vec::new();

            // Line number (if present)
            if let Some(num) = diff_line.line_number {
                let num_str = format!("{:>width$} ", num, width = line_num_width);
                spans.push(Span::styled(num_str, Style::default().fg(Color::DarkGray)));
            } else if line_num_width > 0 {
                // Pad for alignment
                spans.push(Span::styled(
                    " ".repeat(line_num_width + 1),
                    Style::default(),
                ));
            }

            // Prefix character
            if diff_line.source == LineSource::FileHeader {
                // File headers get special formatting
                spans.push(Span::styled("── ", Style::default().fg(Color::DarkGray)));
                spans.push(Span::styled(&diff_line.content, style));
                spans.push(Span::styled(" ──", Style::default().fg(Color::DarkGray)));
            } else {
                spans.push(Span::styled(
                    format!("{} ", diff_line.prefix),
                    style,
                ));
                spans.push(Span::styled(&diff_line.content, style));
            }

            Line::from(spans)
        })
        .collect();

    let title = format!(" branchdiff ");
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(lines).block(block);

    frame.render_widget(paragraph, area);
}

/// Draw the status bar
fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let status = app.status_text();

    // Build help text
    let help = " q:quit  j/k:scroll  g/G:top/bottom  r:refresh ";

    // Calculate available width
    let width = area.width as usize;
    let status_len = status.len();
    let help_len = help.len();

    let line = if status_len + help_len + 2 <= width {
        // Both fit
        let padding = width - status_len - help_len;
        Line::from(vec![
            Span::styled(&status, Style::default().fg(Color::Cyan)),
            Span::raw(" ".repeat(padding)),
            Span::styled(help, Style::default().fg(Color::DarkGray)),
        ])
    } else {
        // Just show status
        Line::from(Span::styled(&status, Style::default().fg(Color::Cyan)))
    };

    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}

/// Draw an error message
#[allow(dead_code)]
fn draw_error(frame: &mut Frame, message: &str, area: Rect) {
    let block = Block::default()
        .title(" Error ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));

    let paragraph = Paragraph::new(message)
        .block(block)
        .style(Style::default().fg(Color::Red));

    frame.render_widget(paragraph, area);
}

/// Draw "no changes" message
#[allow(dead_code)]
fn draw_no_changes(frame: &mut Frame, base_branch: &str, area: Rect) {
    let message = format!("No changes compared to {}", base_branch);

    let block = Block::default()
        .title(" branchdiff ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(message)
        .block(block)
        .style(Style::default().fg(Color::DarkGray));

    frame.render_widget(paragraph, area);
}
