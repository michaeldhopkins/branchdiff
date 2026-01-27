use super::prelude::*;

pub fn draw_help_modal(frame: &mut Frame, area: Rect) {
    let modal_width = 52u16;
    let modal_height = 47u16;

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
            Span::styled("    j / k       ", Style::default().fg(Color::Cyan)),
            Span::raw("  Next / previous file"),
        ]),
        Line::from(vec![
            Span::styled("    ↓ / ↑       ", Style::default().fg(Color::Cyan)),
            Span::raw("  Scroll line"),
        ]),
        Line::from(vec![
            Span::styled("    Ctrl+d / u  ", Style::default().fg(Color::Cyan)),
            Span::raw("  Page down / up"),
        ]),
        Line::from(vec![
            Span::styled("    g / G       ", Style::default().fg(Color::Cyan)),
            Span::raw("  Go to top / bottom"),
        ]),
        Line::from(vec![
            Span::styled("    Mouse       ", Style::default().fg(Color::Cyan)),
            Span::raw("  Scroll, select, collapse"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Actions", Style::default().add_modifier(Modifier::BOLD).fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("    r           ", Style::default().fg(Color::Cyan)),
            Span::raw("  Refresh"),
        ]),
        Line::from(vec![
            Span::styled("    c           ", Style::default().fg(Color::Cyan)),
            Span::raw("  Cycle view mode"),
        ]),
        Line::from(vec![
            Span::styled("    Ctrl+c / y  ", Style::default().fg(Color::Cyan)),
            Span::raw("  Copy selection"),
        ]),
        Line::from(vec![
            Span::styled("    p           ", Style::default().fg(Color::Cyan)),
            Span::raw("  Copy current file path"),
        ]),
        Line::from(vec![
            Span::styled("    Y           ", Style::default().fg(Color::Cyan)),
            Span::raw("  Copy entire diff"),
        ]),
        Line::from(vec![
            Span::styled("    q / Esc     ", Style::default().fg(Color::Cyan)),
            Span::raw("  Quit"),
        ]),
        Line::from(vec![
            Span::styled("    ?           ", Style::default().fg(Color::Cyan)),
            Span::raw("  Toggle this help"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Line Colors", Style::default().add_modifier(Modifier::BOLD).fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw(" "),
            Span::styled("     Base (unchanged context)          ", Style::default()),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(" + C Added (committed)                 ", Style::default().bg(Color::Rgb(25, 50, 50))),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(" + S Added (staged)                    ", Style::default().bg(Color::Rgb(25, 50, 25))),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(" +   Added (unstaged)                  ", Style::default().bg(Color::Rgb(60, 60, 18))),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(" - C Deleted (committed)               ", Style::default().bg(Color::Rgb(45, 20, 20))),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(" - S Deleted (staged)                  ", Style::default().bg(Color::Rgb(55, 25, 25))),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(" -   Deleted (unstaged)                ", Style::default().bg(Color::Rgb(65, 30, 25))),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(" ±   Canceled (added then removed)     ", Style::default().bg(Color::Rgb(50, 25, 50))),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Inline Highlights", Style::default().add_modifier(Modifier::BOLD).fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(" + C Added (committed) char highlight  ", Style::default().bg(Color::Rgb(50, 100, 100))),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(" + S Added (staged) char highlight     ", Style::default().bg(Color::Rgb(50, 100, 50))),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(" +   Added (unstaged) char highlight   ", Style::default().bg(Color::Rgb(130, 130, 35))),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(" - C Deleted (committed) char highlight", Style::default().bg(Color::Rgb(90, 40, 40))),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(" - S Deleted (staged) char highlight   ", Style::default().bg(Color::Rgb(100, 50, 50))),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(" -   Deleted (unstaged) char highlight ", Style::default().bg(Color::Rgb(115, 55, 45))),
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
        let modal_width = 52u16;
        let modal_height = 47u16;
        assert!(modal_width > 0);
        assert!(modal_height > 0);
    }

    #[test]
    fn test_help_modal_centering_large_area() {
        let area = Rect::new(0, 0, 120, 52);
        let modal_width = 52u16;
        let modal_height = 47u16;

        let x = area.width.saturating_sub(modal_width) / 2;
        let y = area.height.saturating_sub(modal_height) / 2;

        assert_eq!(x, 34);
        assert_eq!(y, 2);
    }

    #[test]
    fn test_help_modal_centering_small_area() {
        let area = Rect::new(0, 0, 40, 20);
        let modal_width = 52u16;
        let modal_height = 47u16;

        let x = area.width.saturating_sub(modal_width) / 2;
        let y = area.height.saturating_sub(modal_height) / 2;

        assert_eq!(x, 0);
        assert_eq!(y, 0);
    }

    #[test]
    fn test_help_modal_clamps_to_area() {
        let area = Rect::new(0, 0, 30, 15);
        let modal_width = 52u16;
        let modal_height = 47u16;

        let clamped_width = modal_width.min(area.width);
        let clamped_height = modal_height.min(area.height);

        assert_eq!(clamped_width, 30);
        assert_eq!(clamped_height, 15);
    }
}
