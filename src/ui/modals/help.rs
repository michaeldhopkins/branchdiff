use super::prelude::*;
use crate::app::App;
use crate::diff::LineSource;
use crate::ui::colors::{highlight_bg_color, line_bg_color};
use crate::vcs::VcsBackend;

/// VCS-specific label and symbol set for the color legend.
struct ColorLabels {
    committed: &'static str,
    committed_sym: &'static str,
    staged: &'static str,
    staged_sym: &'static str,
    unstaged: &'static str,
    del_committed: &'static str,
    del_committed_sym: &'static str,
    del_staged: &'static str,
    del_staged_sym: &'static str,
    del_unstaged: &'static str,
}

fn color_labels(backend: VcsBackend) -> ColorLabels {
    match backend {
        VcsBackend::Jj => ColorLabels {
            committed: "Added (earlier commits)",
            committed_sym: " ",
            staged: "Added (current commit)",
            staged_sym: "@",
            unstaged: "Added (later commits)",
            del_committed: "Deleted (earlier commits)",
            del_committed_sym: " ",
            del_staged: "Deleted (current commit)",
            del_staged_sym: "@",
            del_unstaged: "Deleted (later commits)",
        },
        VcsBackend::Git => ColorLabels {
            committed: "Added (committed)",
            committed_sym: "C",
            staged: "Added (staged)",
            staged_sym: "S",
            unstaged: "Added (unstaged)",
            del_committed: "Deleted (committed)",
            del_committed_sym: "C",
            del_staged: "Deleted (staged)",
            del_staged_sym: "S",
            del_unstaged: "Deleted (unstaged)",
        },
    }
}

pub fn draw_help_modal(frame: &mut Frame, area: Rect, app: &App) {
    let modal_width = 54u16;
    let modal_height = 53u16;

    let x = area.width.saturating_sub(modal_width) / 2;
    let y = area.height.saturating_sub(modal_height) / 2;

    let modal_area = Rect::new(x, y, modal_width.min(area.width), modal_height.min(area.height));

    frame.render_widget(Clear, modal_area);

    let labels = color_labels(app.comparison.vcs_backend);

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
            Span::raw("  Scroll up/down"),
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
            Span::styled("    c           ", Style::default().fg(Color::Cyan)),
            Span::raw("  Cycle view (full/ctx/chg/cmt/bm)"),
        ]),
        Line::from(vec![
            Span::styled("    m           ", Style::default().fg(Color::Cyan)),
            Span::raw("  Toggle diff base (fork/tip)"),
        ]),
        Line::from(vec![
            Span::styled("    r           ", Style::default().fg(Color::Cyan)),
            Span::raw("  Mark file reviewed"),
        ]),
        Line::from(vec![
            Span::styled("    R           ", Style::default().fg(Color::Cyan)),
            Span::raw("  Review/unreview all files"),
        ]),
        Line::from(vec![
            Span::styled("    p           ", Style::default().fg(Color::Cyan)),
            Span::raw("  Copy file path"),
        ]),
        Line::from(vec![
            Span::styled("    Y           ", Style::default().fg(Color::Cyan)),
            Span::raw("  Copy entire diff"),
        ]),
        Line::from(vec![
            Span::styled("    D           ", Style::default().fg(Color::Cyan)),
            Span::raw("  Copy git patch format"),
        ]),
        Line::from(vec![
            Span::styled("    q / Esc / ^c", Style::default().fg(Color::Cyan)),
            Span::raw("  Quit"),
        ]),
        Line::from(vec![
            Span::styled("    / or Ctrl+f ", Style::default().fg(Color::Cyan)),
            Span::raw("  Search in diff"),
        ]),
        Line::from(vec![
            Span::styled("    Enter       ", Style::default().fg(Color::Cyan)),
            Span::raw("  Next search match"),
        ]),
        Line::from(vec![
            Span::styled("    Shift+Enter ", Style::default().fg(Color::Cyan)),
            Span::raw("  Previous search match"),
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
            Span::raw("   "),
            Span::styled("     Base (unchanged context)        ", Style::default()),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(format!(" + {} {:<32}", labels.committed_sym, labels.committed), Style::default().bg(line_bg_color(LineSource::Committed))),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(format!(" + {} {:<32}", labels.staged_sym, labels.staged), Style::default().bg(line_bg_color(LineSource::Staged))),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(format!(" +   {:<32}", labels.unstaged), Style::default().bg(line_bg_color(LineSource::Unstaged))),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(format!(" - {} {:<32}", labels.del_committed_sym, labels.del_committed), Style::default().bg(line_bg_color(LineSource::DeletedBase))),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(format!(" - {} {:<32}", labels.del_staged_sym, labels.del_staged), Style::default().bg(line_bg_color(LineSource::DeletedCommitted))),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(format!(" -   {:<32}", labels.del_unstaged), Style::default().bg(line_bg_color(LineSource::DeletedStaged))),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(" ±   Canceled (added then removed)   ", Style::default().bg(line_bg_color(LineSource::CanceledCommitted))),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Inline Highlights", Style::default().add_modifier(Modifier::BOLD).fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(format!(" + {} {:<32}", labels.committed_sym, format!("{} highlight", labels.committed)), Style::default().bg(highlight_bg_color(LineSource::Committed))),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(format!(" + {} {:<32}", labels.staged_sym, format!("{} highlight", labels.staged)), Style::default().bg(highlight_bg_color(LineSource::Staged))),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(format!(" +   {:<32}", format!("{} highlight", labels.unstaged)), Style::default().bg(highlight_bg_color(LineSource::Unstaged))),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(format!(" - {} {:<32}", labels.del_committed_sym, format!("{} highlight", labels.del_committed)), Style::default().bg(highlight_bg_color(LineSource::DeletedBase))),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(format!(" - {} {:<32}", labels.del_staged_sym, format!("{} highlight", labels.del_staged)), Style::default().bg(highlight_bg_color(LineSource::DeletedCommitted))),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(format!(" -   {:<32}", format!("{} highlight", labels.del_unstaged)), Style::default().bg(highlight_bg_color(LineSource::DeletedStaged))),
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
fn build_color_labels(backend: VcsBackend) -> Vec<String> {
    let labels = color_labels(backend);
    vec![
        labels.committed.to_string(),
        labels.staged.to_string(),
        labels.unstaged.to_string(),
        labels.del_committed.to_string(),
        labels.del_staged.to_string(),
        labels.del_unstaged.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_help_modal_dimensions() {
        let modal_width = 54u16;
        let modal_height = 51u16;
        assert!(modal_width > 0);
        assert!(modal_height > 0);
    }

    #[test]
    fn test_help_modal_centering_large_area() {
        let area = Rect::new(0, 0, 120, 60);
        let modal_width = 54u16;
        let modal_height = 51u16;

        let x = area.width.saturating_sub(modal_width) / 2;
        let y = area.height.saturating_sub(modal_height) / 2;

        assert_eq!(x, 33);
        assert_eq!(y, 4);
    }

    #[test]
    fn test_help_modal_centering_small_area() {
        let area = Rect::new(0, 0, 40, 20);
        let modal_width = 54u16;
        let modal_height = 51u16;

        let x = area.width.saturating_sub(modal_width) / 2;
        let y = area.height.saturating_sub(modal_height) / 2;

        assert_eq!(x, 0);
        assert_eq!(y, 0);
    }

    #[test]
    fn test_help_modal_clamps_to_area() {
        let area = Rect::new(0, 0, 30, 15);
        let modal_width = 54u16;
        let modal_height = 51u16;

        let clamped_width = modal_width.min(area.width);
        let clamped_height = modal_height.min(area.height);

        assert_eq!(clamped_width, 30);
        assert_eq!(clamped_height, 15);
    }

    #[test]
    fn test_help_labels_git_mode() {
        let labels = build_color_labels(VcsBackend::Git);
        assert_eq!(labels[0], "Added (committed)");
        assert_eq!(labels[1], "Added (staged)");
        assert_eq!(labels[2], "Added (unstaged)");
        assert_eq!(labels[3], "Deleted (committed)");
        assert_eq!(labels[4], "Deleted (staged)");
        assert_eq!(labels[5], "Deleted (unstaged)");
    }

    #[test]
    fn test_help_labels_jj_mode() {
        let labels = build_color_labels(VcsBackend::Jj);
        assert_eq!(labels[0], "Added (earlier commits)");
        assert_eq!(labels[1], "Added (current commit)");
        assert_eq!(labels[2], "Added (later commits)");
        assert_eq!(labels[3], "Deleted (earlier commits)");
        assert_eq!(labels[4], "Deleted (current commit)");
        assert_eq!(labels[5], "Deleted (later commits)");
    }

    #[test]
    fn test_help_symbols_git_mode() {
        let labels = color_labels(VcsBackend::Git);
        assert_eq!(labels.committed_sym, "C");
        assert_eq!(labels.staged_sym, "S");
        assert_eq!(labels.del_committed_sym, "C");
        assert_eq!(labels.del_staged_sym, "S");
    }

    #[test]
    fn test_help_symbols_jj_mode() {
        let labels = color_labels(VcsBackend::Jj);
        assert_eq!(labels.committed_sym, " ");
        assert_eq!(labels.staged_sym, "@");
        assert_eq!(labels.del_committed_sym, " ");
        assert_eq!(labels.del_staged_sym, "@");
    }
}
