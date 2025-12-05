use super::prelude::*;

pub fn draw_warning_banner(frame: &mut Frame, message: &str, area: Rect) {
    let line = Line::from(Span::styled(
        format!(" ⚠ {} ", message),
        Style::default().fg(Color::Yellow),
    ));
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}

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

#[cfg(test)]
mod tests {
    #[test]
    fn test_warning_message_format() {
        let message = "Merge conflicts detected";
        let formatted = format!(" ⚠ {} ", message);
        assert_eq!(formatted, " ⚠ Merge conflicts detected ");
    }

    #[test]
    fn test_no_changes_message_format() {
        let base_branch = "main";
        let message = format!("No changes compared to {}", base_branch);
        assert_eq!(message, "No changes compared to main");
    }
}
