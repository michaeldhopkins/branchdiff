use super::prelude::*;
use crate::update::RecoveryHint;
use ratatui::widgets::Wrap;
use unicode_width::UnicodeWidthStr;

/// Hard cap on banner rows. Beyond this we stop growing the banner so the diff
/// view still gets meaningful space; the message just truncates after.
const MAX_BANNER_ROWS: u16 = 4;

pub fn draw_warning_banner(frame: &mut Frame, message: &str, area: Rect) {
    draw_warning_banner_with_hint(frame, message, None, area);
}

/// Estimate how many rows the banner will need at the given width.
///
/// For actionable errors (with a recovery hint), we render the friendly
/// summary in one row and the hint in a second row. For other errors, the raw
/// message wraps — we ceil-divide its display width by the available width and
/// cap at `MAX_BANNER_ROWS` so a runaway error message can't eat the screen.
pub fn banner_row_count(message: &str, hint: Option<&RecoveryHint>, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }
    if hint.is_some() {
        // One row for the summary, one for the press-key hint.
        return 2;
    }
    // " ⚠ " + message + " " — 3 columns of decoration, plus the message.
    let cols = (UnicodeWidthStr::width(message) as u16).saturating_add(4);
    cols.div_ceil(width).clamp(1, MAX_BANNER_ROWS)
}

/// Render the warning banner, optionally appending a recovery key hint.
///
/// When `hint` is `Some`, the banner shows the friendly summary on row 1 and
/// the `(press <key> to run <command>)` invitation on row 2 — keeping the
/// actionable bit visible even on narrow terminals. The raw subprocess error
/// is intentionally suppressed in that case; it's verbose and clips the hint.
pub fn draw_warning_banner_with_hint(
    frame: &mut Frame,
    message: &str,
    hint: Option<&RecoveryHint>,
    area: Rect,
) {
    let lines = match hint {
        Some(h) => vec![
            Line::from(Span::styled(
                format!(" ⚠ {} ", h.friendly_summary),
                Style::default().fg(Color::Yellow),
            )),
            Line::from(Span::styled(
                format!("   press '{}' to run `{}` ", h.key_hint, h.command_label),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
        ],
        None => vec![Line::from(Span::styled(
            format!(" ⚠ {} ", message),
            Style::default().fg(Color::Yellow),
        ))],
    };
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
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

    fn row_text(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
        let mut s = String::new();
        for x in 0..buffer.area.width {
            s.push_str(buffer.cell((x, y)).expect("cell").symbol());
        }
        s
    }

    /// Banner with a recovery hint shows the friendly summary on row 1 and the
    /// press-key invitation on row 2. The raw subprocess error (which can be
    /// hundreds of characters) is intentionally suppressed — it clipped the
    /// key hint off the right edge before this change.
    #[test]
    fn banner_with_hint_renders_friendly_summary_and_press_hint() {
        let backend = TestBackend::new(80, 2);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let hint = RecoveryHint::jj_update_stale();
        terminal
            .draw(|f| {
                // Pass an intentionally noisy raw error to confirm we
                // suppress it in favor of the friendly summary.
                draw_warning_banner_with_hint(
                    f,
                    "Error: jj diff --from @- --to @ exited with status: 1: \
                     Error: The working copy is stale (not updated since op ...)",
                    Some(&hint),
                    f.area(),
                );
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let row0 = row_text(&buffer, 0);
        let row1 = row_text(&buffer, 1);
        assert!(row0.contains("jj working copy is stale"), "row0: {row0:?}");
        assert!(
            !row0.contains("--from @-"),
            "raw subprocess error must not leak into the user-facing banner: {row0:?}"
        );
        assert!(row1.contains("press 'u'"), "row1: {row1:?}");
        assert!(row1.contains("jj workspace update-stale"), "row1: {row1:?}");
    }

    /// A long unrecognized error must wrap rather than clip — otherwise the
    /// user sees a truncated message with no way to recover the rest. This is
    /// the regression that the screenshot in the original report exposed.
    #[test]
    fn banner_without_hint_wraps_long_message() {
        let backend = TestBackend::new(40, 4);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let long = "this is a deliberately long error message that exceeds forty cols and must wrap onto multiple rows";
        terminal
            .draw(|f| {
                draw_warning_banner_with_hint(f, long, None, f.area());
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        // Concatenate all rows then normalize whitespace — the wrap algorithm
        // can elide spaces at row boundaries, so we just verify the words all
        // landed in order across rows.
        let full: String = (0..buffer.area.height)
            .map(|y| row_text(&buffer, y))
            .collect::<Vec<_>>()
            .join(" ");
        let normalized: String = full.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalized.contains("deliberately long error message that exceeds forty cols"),
            "early words missing — wrap dropped content. got: {normalized:?}"
        );
        assert!(
            normalized.contains("must wrap onto multiple rows"),
            "trailing words missing — content clipped at banner boundary. got: {normalized:?}"
        );
    }

    /// `banner_row_count` must give actionable errors exactly 2 rows so the
    /// layout reserves space for both the summary and the press-key hint.
    /// Regression: if this returns 1, the press-key hint never renders.
    #[test]
    fn banner_row_count_actionable_always_two_rows() {
        let hint = RecoveryHint::jj_update_stale();
        // Short or long, narrow or wide — always 2 rows for hinted banners.
        assert_eq!(banner_row_count("short", Some(&hint), 80), 2);
        assert_eq!(banner_row_count("short", Some(&hint), 20), 2);
        assert_eq!(banner_row_count("x".repeat(500).as_str(), Some(&hint), 80), 2);
    }

    /// `banner_row_count` for non-actionable errors must grow with message
    /// length but stay capped, so a runaway error message can't squeeze the
    /// diff view down to zero rows.
    #[test]
    fn banner_row_count_unhinted_wraps_and_caps() {
        assert_eq!(banner_row_count("short", None, 80), 1);
        // 200 chars at width 40 → 5 rows raw, capped at MAX_BANNER_ROWS = 4.
        let huge = "x".repeat(200);
        let rows = banner_row_count(&huge, None, 40);
        assert!(rows <= 4, "must cap at MAX_BANNER_ROWS, got {rows}");
        assert!(rows >= 2, "must grow beyond 1 row for long message, got {rows}");
    }

    #[test]
    fn banner_row_count_zero_width_is_safe() {
        let hint = RecoveryHint::jj_update_stale();
        assert_eq!(banner_row_count("anything", None, 0), 1);
        assert_eq!(banner_row_count("anything", Some(&hint), 0), 1);
    }
}
