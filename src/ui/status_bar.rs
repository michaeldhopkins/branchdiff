use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::App;

/// Get the repo directory name for display
fn repo_name(app: &App) -> String {
    app.repo_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo")
        .to_string()
}

/// Determine how many lines the status bar needs based on content and width
pub fn status_bar_height(app: &App, width: u16) -> u16 {
    let width = width as usize;

    let help = " q:quit  j/k:files  g/G:top/bottom  ?:help ";

    let branch_info = match &app.current_branch {
        Some(b) => format!("{} | {} vs {}", repo_name(app), b, app.base_branch),
        None => format!("{} | HEAD vs {}", repo_name(app), app.base_branch),
    };

    let file_count = app.files.len();
    let additions = app.additions_count();
    let deletions = app.deletions_count();
    let mode = match app.view_mode {
        crate::app::ViewMode::Full => "",
        crate::app::ViewMode::Context => " [context]",
        crate::app::ViewMode::ChangesOnly => " [changes]",
    };

    let stats = format!(
        "{} file{} | +{} -{}{} | {}%",
        file_count,
        if file_count == 1 { "" } else { "s" },
        additions,
        deletions,
        mode,
        app.scroll_percentage()
    );

    let full_status = format!("{} | {}", branch_info, stats);

    // Check if everything fits on one line
    if full_status.len() + help.len() + 2 <= width {
        1
    } else {
        2
    }
}

/// Build stats spans with colored +/- counts
fn build_stats_spans(app: &App) -> Vec<Span<'static>> {
    let file_count = app.files.len();
    let additions = app.additions_count();
    let deletions = app.deletions_count();
    let mode = match app.view_mode {
        crate::app::ViewMode::Full => "",
        crate::app::ViewMode::Context => " [context]",
        crate::app::ViewMode::ChangesOnly => " [changes]",
    };

    let mut spans = vec![
        Span::styled(
            format!("{} file{} | ", file_count, if file_count == 1 { "" } else { "s" }),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(format!("+{}", additions), Style::default().fg(Color::LightGreen)),
        Span::styled(" ", Style::default().fg(Color::Cyan)),
        Span::styled(format!("-{}", deletions), Style::default().fg(Color::Red)),
        Span::styled(
            format!("{} | {}%", mode, app.scroll_percentage()),
            Style::default().fg(Color::Cyan),
        ),
    ];

    // Add performance warning if present
    if let Some(ref warning) = app.performance_warning {
        spans.push(Span::styled(
            format!(" [{}]", warning),
            Style::default().fg(Color::Yellow),
        ));
    }

    spans
}

/// Build full status spans (branch info + stats) with colored +/- counts
fn build_full_status_spans(app: &App) -> Vec<Span<'static>> {
    let branch_info = match &app.current_branch {
        Some(b) => format!("{} | {} vs {}", repo_name(app), b, app.base_branch),
        None => format!("{} | HEAD vs {}", repo_name(app), app.base_branch),
    };

    let file_count = app.files.len();
    let additions = app.additions_count();
    let deletions = app.deletions_count();
    let mode = match app.view_mode {
        crate::app::ViewMode::Full => "",
        crate::app::ViewMode::Context => " [context]",
        crate::app::ViewMode::ChangesOnly => " [changes]",
    };

    let mut spans = vec![
        Span::styled(
            format!("{} | {} file{} | ", branch_info, file_count, if file_count == 1 { "" } else { "s" }),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(format!("+{}", additions), Style::default().fg(Color::LightGreen)),
        Span::styled(" ", Style::default().fg(Color::Cyan)),
        Span::styled(format!("-{}", deletions), Style::default().fg(Color::Red)),
        Span::styled(
            format!("{} | {}%", mode, app.scroll_percentage()),
            Style::default().fg(Color::Cyan),
        ),
    ];

    // Add performance warning if present
    if let Some(ref warning) = app.performance_warning {
        spans.push(Span::styled(
            format!(" [{}]", warning),
            Style::default().fg(Color::Yellow),
        ));
    }

    spans
}

/// Truncate a string with ellipsis if it exceeds max_len (char count, not bytes)
pub fn truncate_with_ellipsis(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        s.to_string()
    } else if max_len <= 3 {
        ".".repeat(max_len)
    } else {
        let truncated: String = s.chars().take(max_len - 3).collect();
        format!("{}...", truncated)
    }
}

/// Draw the status bar (may use 1 or 2 lines depending on available width)
pub fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;

    // Build help text
    let help = " q:quit  j/k:files  g/G:top/bottom  ?:help ";
    let help_short = " ?:help ";

    // Get status components
    let file_count = app.files.len();
    let additions = app.additions_count();
    let deletions = app.deletions_count();
    let mode = match app.view_mode {
        crate::app::ViewMode::Full => "",
        crate::app::ViewMode::Context => " [context]",
        crate::app::ViewMode::ChangesOnly => " [changes]",
    };

    let stats = format!(
        "{} file{} | +{} -{}{} | {}%",
        file_count,
        if file_count == 1 { "" } else { "s" },
        additions,
        deletions,
        mode,
        app.scroll_percentage()
    );

    let branch_info = match &app.current_branch {
        Some(b) => format!("{} | {} vs {}", repo_name(app), b, app.base_branch),
        None => format!("{} | HEAD vs {}", repo_name(app), app.base_branch),
    };

    // Try different layouts based on available width
    let full_status = format!("{} | {}", branch_info, stats);

    if area.height >= 2 {
        // We have 2 lines available - use them
        let line1_content = if branch_info.len() + help.len() + 2 <= width {
            // Branch info + full help fit on line 1
            let padding = width.saturating_sub(branch_info.len() + help.len());
            Line::from(vec![
                Span::styled(&branch_info, Style::default().fg(Color::Cyan)),
                Span::raw(" ".repeat(padding)),
                Span::styled(help, Style::default().fg(Color::DarkGray)),
            ])
        } else if branch_info.len() + help_short.len() + 2 <= width {
            // Branch info + short help fit
            let padding = width.saturating_sub(branch_info.len() + help_short.len());
            Line::from(vec![
                Span::styled(&branch_info, Style::default().fg(Color::Cyan)),
                Span::raw(" ".repeat(padding)),
                Span::styled(help_short, Style::default().fg(Color::DarkGray)),
            ])
        } else {
            // Truncate branch info
            let max_branch_len = width.saturating_sub(help_short.len() + 1);
            let truncated = truncate_with_ellipsis(&branch_info, max_branch_len);
            let padding = width.saturating_sub(truncated.len() + help_short.len());
            Line::from(vec![
                Span::styled(truncated, Style::default().fg(Color::Cyan)),
                Span::raw(" ".repeat(padding)),
                Span::styled(help_short, Style::default().fg(Color::DarkGray)),
            ])
        };

        let line2_content = if stats.len() <= width {
            Line::from(build_stats_spans(app))
        } else {
            let truncated = truncate_with_ellipsis(&stats, width);
            Line::from(Span::styled(truncated, Style::default().fg(Color::Cyan)))
        };

        let paragraph = Paragraph::new(vec![line1_content, line2_content]);
        frame.render_widget(paragraph, area);
    } else {
        // Only 1 line available
        let line = if full_status.len() + help.len() + 2 <= width {
            // Full status + help fit
            let padding = width.saturating_sub(full_status.len() + help.len());
            let mut spans = build_full_status_spans(app);
            spans.push(Span::raw(" ".repeat(padding)));
            spans.push(Span::styled(help, Style::default().fg(Color::DarkGray)));
            Line::from(spans)
        } else if full_status.len() + help_short.len() + 2 <= width {
            // Full status + short help fit
            let padding = width.saturating_sub(full_status.len() + help_short.len());
            let mut spans = build_full_status_spans(app);
            spans.push(Span::raw(" ".repeat(padding)));
            spans.push(Span::styled(help_short, Style::default().fg(Color::DarkGray)));
            Line::from(spans)
        } else if full_status.len() <= width {
            // Just status fits
            Line::from(build_full_status_spans(app))
        } else {
            // Need to truncate - fall back to plain cyan (truncation loses coloring)
            if stats.len() + 3 <= width {
                // Show truncated branch + stats
                let max_branch_len = width.saturating_sub(stats.len() + 4); // " | " + some branch
                let truncated_branch = truncate_with_ellipsis(&branch_info, max_branch_len);
                let truncated_status = format!("{} | {}", truncated_branch, stats);
                Line::from(Span::styled(truncated_status, Style::default().fg(Color::Cyan)))
            } else {
                // Just truncate the whole thing
                let truncated = truncate_with_ellipsis(&full_status, width);
                Line::from(Span::styled(truncated, Style::default().fg(Color::Cyan)))
            }
        };

        let paragraph = Paragraph::new(line);
        frame.render_widget(paragraph, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestAppBuilder;

    #[test]
    fn test_repo_name_extracts_directory_name() {
        let app = TestAppBuilder::new().build();
        // TestAppBuilder uses "/tmp/test" as repo_path
        assert_eq!(repo_name(&app), "test");
    }

    #[test]
    fn test_branch_info_includes_repo_name() {
        let app = TestAppBuilder::new()
            .with_current_branch(Some("feature"))
            .with_base_branch("main")
            .build();

        // Manually construct what build_full_status_spans produces
        let spans = build_full_status_spans(&app);
        let combined: String = spans.iter().map(|s| s.content.to_string()).collect();

        assert!(
            combined.starts_with("test | feature vs main"),
            "Expected branch info to start with 'test | feature vs main', got: {}",
            combined
        );
    }

    #[test]
    fn test_branch_info_uses_head_when_no_current_branch() {
        let app = TestAppBuilder::new()
            .with_current_branch(None)
            .with_base_branch("master")
            .build();

        let spans = build_full_status_spans(&app);
        let combined: String = spans.iter().map(|s| s.content.to_string()).collect();

        assert!(
            combined.starts_with("test | HEAD vs master"),
            "Expected branch info to start with 'test | HEAD vs master', got: {}",
            combined
        );
    }
}
