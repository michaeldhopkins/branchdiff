use super::prelude::*;

pub fn draw_help_modal(frame: &mut Frame, area: Rect) {
    let modal_width = 50u16;
    let modal_height = 23u16;

    let x = area.width.saturating_sub(modal_width) / 2;
    let y = area.height.saturating_sub(modal_height) / 2;

    let modal_area = Rect::new(x, y, modal_width.min(area.width), modal_height.min(area.height));

    frame.render_widget(Clear, modal_area);

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
        Line::from(vec![
            Span::styled("    Click header", Style::default().fg(Color::Cyan)),
            Span::raw(" Collapse/expand file"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_help_modal_dimensions() {
        let modal_width = 50u16;
        let modal_height = 23u16;
        assert!(modal_width > 0);
        assert!(modal_height > 0);
    }

    #[test]
    fn test_help_modal_centering_large_area() {
        let area = Rect::new(0, 0, 120, 40);
        let modal_width = 50u16;
        let modal_height = 23u16;

        let x = area.width.saturating_sub(modal_width) / 2;
        let y = area.height.saturating_sub(modal_height) / 2;

        assert_eq!(x, 35);
        assert_eq!(y, 8);
    }

    #[test]
    fn test_help_modal_centering_small_area() {
        let area = Rect::new(0, 0, 40, 20);
        let modal_width = 50u16;
        let modal_height = 23u16;

        let x = area.width.saturating_sub(modal_width) / 2;
        let y = area.height.saturating_sub(modal_height) / 2;

        assert_eq!(x, 0);
        assert_eq!(y, 0);
    }

    #[test]
    fn test_help_modal_clamps_to_area() {
        let area = Rect::new(0, 0, 30, 15);
        let modal_width = 50u16;
        let modal_height = 23u16;

        let clamped_width = modal_width.min(area.width);
        let clamped_height = modal_height.min(area.height);

        assert_eq!(clamped_width, 30);
        assert_eq!(clamped_height, 15);
    }
}
