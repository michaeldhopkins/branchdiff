use super::prelude::*;

pub fn draw_warning_banner(frame: &mut Frame, message: &str, area: Rect) {
    let line = Line::from(Span::styled(
        format!(" ⚠ {} ", message),
        Style::default().fg(Color::Yellow),
    ));
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_warning_message_format() {
        let message = "Merge conflicts detected";
        let formatted = format!(" ⚠ {} ", message);
        assert_eq!(formatted, " ⚠ Merge conflicts detected ");
    }
}
