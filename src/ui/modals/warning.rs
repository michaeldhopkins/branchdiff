use super::prelude::*;
use crate::update::RecoveryHint;

pub fn draw_warning_banner(frame: &mut Frame, message: &str, area: Rect) {
    draw_warning_banner_with_hint(frame, message, None, area);
}

/// Render the warning banner, optionally appending a recovery key hint.
///
/// When `hint` is `Some`, the banner trails with a styled `(press <key> to run
/// <command>)` so the user can accept the fix without leaving the TUI.
pub fn draw_warning_banner_with_hint(
    frame: &mut Frame,
    message: &str,
    hint: Option<&RecoveryHint>,
    area: Rect,
) {
    let spans = match hint {
        Some(h) => vec![
            Span::styled(
                format!(" ⚠ {} ", message),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!("(press '{}' to run `{}`) ", h.key_hint, h.command_label),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ],
        None => vec![Span::styled(
            format!(" ⚠ {} ", message),
            Style::default().fg(Color::Yellow),
        )],
    };
    let paragraph = Paragraph::new(Line::from(spans));
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn test_warning_message_format() {
        let message = "Merge conflicts detected";
        let formatted = format!(" ⚠ {} ", message);
        assert_eq!(formatted, " ⚠ Merge conflicts detected ");
    }

    /// Render a banner with a recovery hint and verify the key + command label
    /// both land in the buffer the user sees. We snapshot via TestBackend
    /// because the banner mixes styled spans — a string-only assertion would
    /// miss layout regressions.
    #[test]
    fn banner_with_hint_shows_key_and_command() {
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let hint = RecoveryHint::jj_update_stale();
        terminal
            .draw(|f| {
                draw_warning_banner_with_hint(
                    f,
                    "The working copy is stale",
                    Some(&hint),
                    f.area(),
                );
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for x in 0..buffer.area.width {
            text.push_str(buffer.cell((x, 0)).expect("cell").symbol());
        }
        assert!(text.contains("The working copy is stale"), "got: {text:?}");
        assert!(text.contains("press 'u'"), "got: {text:?}");
        assert!(text.contains("jj workspace update-stale"), "got: {text:?}");
    }

    #[test]
    fn banner_without_hint_omits_action_text() {
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| {
                draw_warning_banner_with_hint(f, "Merge conflicts", None, f.area());
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for x in 0..buffer.area.width {
            text.push_str(buffer.cell((x, 0)).expect("cell").symbol());
        }
        assert!(text.contains("Merge conflicts"), "got: {text:?}");
        assert!(!text.contains("press"), "should have no hint, got: {text:?}");
    }
}
