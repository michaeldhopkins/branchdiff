use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

/// Draw a warning banner at the top of the screen
pub fn draw_warning_banner(frame: &mut Frame, message: &str, area: Rect) {
    let line = Line::from(Span::styled(
        format!(" ⚠ {} ", message),
        Style::default().fg(Color::Yellow),
    ));
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}

/// Draw an error message
#[allow(dead_code)]
pub fn draw_error(frame: &mut Frame, message: &str, area: Rect) {
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
pub fn draw_no_changes(frame: &mut Frame, base_branch: &str, area: Rect) {
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

/// Draw the help modal
pub fn draw_help_modal(frame: &mut Frame, area: Rect) {
    // Center the modal
    let modal_width = 50u16;
    let modal_height = 22u16;

    let x = area.width.saturating_sub(modal_width) / 2;
    let y = area.height.saturating_sub(modal_height) / 2;

    let modal_area = Rect::new(x, y, modal_width.min(area.width), modal_height.min(area.height));

    // Clear the area behind the modal
    frame.render_widget(Clear, modal_area);

    // Build help content
    let help_lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Navigation", Style::default().add_modifier(Modifier::BOLD).fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("    j / ↓       ", Style::default().fg(Color::Cyan)),
            Span::raw("Scroll down"),
        ]),
        Line::from(vec![
            Span::styled("    k / ↑       ", Style::default().fg(Color::Cyan)),
            Span::raw("Scroll up"),
        ]),
        Line::from(vec![
            Span::styled("    Ctrl+d / PgDn", Style::default().fg(Color::Cyan)),
            Span::raw(" Page down"),
        ]),
        Line::from(vec![
            Span::styled("    Ctrl+u / PgUp", Style::default().fg(Color::Cyan)),
            Span::raw(" Page up"),
        ]),
        Line::from(vec![
            Span::styled("    g / Home    ", Style::default().fg(Color::Cyan)),
            Span::raw("Go to top"),
        ]),
        Line::from(vec![
            Span::styled("    G / End     ", Style::default().fg(Color::Cyan)),
            Span::raw("Go to bottom"),
        ]),
        Line::from(vec![
            Span::styled("    Mouse scroll", Style::default().fg(Color::Cyan)),
            Span::raw(" Scroll up/down"),
        ]),
        Line::from(vec![
            Span::styled("    Mouse drag  ", Style::default().fg(Color::Cyan)),
            Span::raw(" Select text"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Actions", Style::default().add_modifier(Modifier::BOLD).fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("    r           ", Style::default().fg(Color::Cyan)),
            Span::raw("Refresh"),
        ]),
        Line::from(vec![
            Span::styled("    c           ", Style::default().fg(Color::Cyan)),
            Span::raw("Cycle view mode"),
        ]),
        Line::from(vec![
            Span::styled("    y           ", Style::default().fg(Color::Cyan)),
            Span::raw("Copy selection"),
        ]),
        Line::from(vec![
            Span::styled("    q / Esc     ", Style::default().fg(Color::Cyan)),
            Span::raw("Quit"),
        ]),
        Line::from(vec![
            Span::styled("    ?           ", Style::default().fg(Color::Cyan)),
            Span::raw("Toggle this help"),
        ]),
    ];

    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(help_lines).block(block);

    frame.render_widget(paragraph, modal_area);
}
