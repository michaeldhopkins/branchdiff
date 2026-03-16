use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, ViewMode};
use super::selection::{apply_selection_to_span, get_line_selection_range};

/// View mode indicator for the status bar (e.g., " [context]", " [commit knmq]")
fn view_mode_label(app: &App) -> String {
    match app.view.view_mode {
        ViewMode::Full => " [all lines]".to_string(),
        ViewMode::Context => " [context]".to_string(),
        ViewMode::ChangesOnly => " [changed lines only]".to_string(),
        ViewMode::CommitOnly => format!(" [commit {}]", app.comparison.to_label),
        ViewMode::BookmarkOnly => match &app.comparison.bookmark_name {
            Some(name) => format!(" [bookmark {}]", name),
            None => " [bookmark]".to_string(),
        },
    }
}

/// Get the repo directory name for display
fn repo_name(app: &App) -> String {
    app.repo_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo")
        .to_string()
}

fn branch_info(app: &App) -> String {
    let base = format!(
        "{} | {} vs {}",
        repo_name(app),
        app.comparison.to_label,
        app.comparison.from_label
    );
    match app.comparison.stack_position {
        Some(pos) if pos.head_count > 1 => {
            format!("{base} [{}/{} head 1/{}]", pos.current, pos.total, pos.head_count)
        }
        Some(pos) => format!("{base} [{}/{}]", pos.current, pos.total),
        None => base,
    }
}

/// Determine how many lines the status bar needs based on content and width
pub fn status_bar_height(app: &App, width: u16) -> u16 {
    let width = width as usize;

    let help = " q:quit  j/k:files  g/G:top/bottom  ?:help ";

    let branch_info = branch_info(app);

    let mode = view_mode_label(app);
    let stats = format!(
        "{} file{} | +{} -{}{} | {}%",
        app.files.len(),
        if app.files.len() == 1 { "" } else { "s" },
        app.additions_count(),
        app.deletions_count(),
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
    let mode = view_mode_label(app);

    let mut spans = vec![
        Span::styled(
            format!("{} file{} | ", file_count, if file_count == 1 { "" } else { "s" }),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(format!("+{}", app.additions_count()), Style::default().fg(Color::LightGreen)),
        Span::styled(" ", Style::default().fg(Color::Cyan)),
        Span::styled(format!("-{}", app.deletions_count()), Style::default().fg(Color::Red)),
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
    let branch_info = branch_info(app);
    let file_count = app.files.len();
    let mode = view_mode_label(app);

    let mut spans = vec![
        Span::styled(
            format!("{} | {} file{} | ", branch_info, file_count, if file_count == 1 { "" } else { "s" }),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(format!("+{}", app.additions_count()), Style::default().fg(Color::LightGreen)),
        Span::styled(" ", Style::default().fg(Color::Cyan)),
        Span::styled(format!("-{}", app.deletions_count()), Style::default().fg(Color::Red)),
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

/// Apply selection highlighting to a status bar line.
/// `virtual_row` is `row_map.len() + line_index` — the virtual row index used by the selection system.
fn apply_status_bar_selection(line: Line<'static>, selection: &Option<crate::app::Selection>, virtual_row: usize) -> Line<'static> {
    let Some((sel_start, sel_end)) = get_line_selection_range(selection, virtual_row) else {
        return line;
    };

    let mut new_spans = Vec::new();
    let mut char_offset = 0;
    for span in line.spans {
        let span_len = span.content.chars().count();
        let result = apply_selection_to_span(span, char_offset, sel_start, sel_end);
        new_spans.extend(result);
        char_offset += span_len;
    }
    Line::from(new_spans)
}

/// Draw the status bar (may use 1 or 2 lines depending on available width)
pub fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;

    // Build help text
    let help = " q:quit  j/k:files  g/G:top/bottom  ?:help ";
    let help_short = " ?:help ";

    // Get status components
    let mode = view_mode_label(app);
    let stats = format!(
        "{} file{} | +{} -{}{} | {}%",
        app.files.len(),
        if app.files.len() == 1 { "" } else { "s" },
        app.additions_count(),
        app.deletions_count(),
        mode,
        app.scroll_percentage()
    );

    let branch_info = branch_info(app);

    // Try different layouts based on available width
    let full_status = format!("{} | {}", branch_info, stats);

    if area.height >= 2 {
        // We have 2 lines available - use them
        let line1_content = if branch_info.len() + help.len() + 2 <= width {
            // Branch info + full help fit on line 1
            let padding = width.saturating_sub(branch_info.len() + help.len());
            Line::from(vec![
                Span::styled(branch_info.clone(), Style::default().fg(Color::Cyan)),
                Span::raw(" ".repeat(padding)),
                Span::styled(help, Style::default().fg(Color::DarkGray)),
            ])
        } else if branch_info.len() + help_short.len() + 2 <= width {
            // Branch info + short help fit
            let padding = width.saturating_sub(branch_info.len() + help_short.len());
            Line::from(vec![
                Span::styled(branch_info.clone(), Style::default().fg(Color::Cyan)),
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

        let row_map_len = app.view.row_map.len();
        let line1_content = apply_status_bar_selection(line1_content, &app.view.selection, row_map_len);
        let line2_content = apply_status_bar_selection(line2_content, &app.view.selection, row_map_len + 1);

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

        let row_map_len = app.view.row_map.len();
        let line = apply_status_bar_selection(line, &app.view.selection, row_map_len);

        let paragraph = Paragraph::new(line);
        frame.render_widget(paragraph, area);
    }
}

/// Get the plain text content of the status bar for selection support.
/// Returns one or two lines matching the same layout logic as `draw_status_bar`.
pub fn status_bar_plain_text(app: &App, width: u16) -> Vec<String> {
    let width = width as usize;
    let help = " q:quit  j/k:files  g/G:top/bottom  ?:help ";
    let help_short = " ?:help ";

    let mode = view_mode_label(app);
    let stats = format!(
        "{} file{} | +{} -{}{} | {}%",
        app.files.len(),
        if app.files.len() == 1 { "" } else { "s" },
        app.additions_count(),
        app.deletions_count(),
        mode,
        app.scroll_percentage()
    );

    let branch_info = branch_info(app);
    let full_status = format!("{} | {}", branch_info, stats);

    let height = status_bar_height(app, width as u16);
    if height >= 2 {
        let line1 = if branch_info.len() + help.len() + 2 <= width {
            let padding = width.saturating_sub(branch_info.len() + help.len());
            format!("{}{}{}", branch_info, " ".repeat(padding), help)
        } else if branch_info.len() + help_short.len() + 2 <= width {
            let padding = width.saturating_sub(branch_info.len() + help_short.len());
            format!("{}{}{}", branch_info, " ".repeat(padding), help_short)
        } else {
            let max_branch_len = width.saturating_sub(help_short.len() + 1);
            let truncated = truncate_with_ellipsis(&branch_info, max_branch_len);
            let padding = width.saturating_sub(truncated.len() + help_short.len());
            format!("{}{}{}", truncated, " ".repeat(padding), help_short)
        };

        let line2 = if stats.len() <= width {
            stats
        } else {
            truncate_with_ellipsis(&stats, width)
        };

        vec![line1, line2]
    } else {
        let line = if full_status.len() + help.len() + 2 <= width {
            let padding = width.saturating_sub(full_status.len() + help.len());
            format!("{}{}{}", full_status, " ".repeat(padding), help)
        } else if full_status.len() + help_short.len() + 2 <= width {
            let padding = width.saturating_sub(full_status.len() + help_short.len());
            format!("{}{}{}", full_status, " ".repeat(padding), help_short)
        } else if full_status.len() <= width {
            full_status
        } else if stats.len() + 3 <= width {
            let max_branch_len = width.saturating_sub(stats.len() + 4);
            let truncated_branch = truncate_with_ellipsis(&branch_info, max_branch_len);
            format!("{} | {}", truncated_branch, stats)
        } else {
            truncate_with_ellipsis(&full_status, width)
        };
        vec![line]
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

    #[test]
    fn test_branch_info_stack_position_linear() {
        use crate::vcs::StackPosition;

        let app = TestAppBuilder::new()
            .with_current_branch(Some("qvkxrzts"))
            .with_base_branch("main")
            .with_stack_position(StackPosition { current: 2, total: 3, head_count: 1 })
            .build();

        let info = branch_info(&app);
        assert!(info.contains("[2/3]"),
            "linear stack should show [2/3], got: {info}");
        assert!(!info.contains("head"),
            "linear stack should not show head count, got: {info}");
    }

    #[test]
    fn test_branch_info_stack_position_branching() {
        use crate::vcs::StackPosition;

        let app = TestAppBuilder::new()
            .with_current_branch(Some("qvkxrzts"))
            .with_base_branch("main")
            .with_stack_position(StackPosition { current: 3, total: 5, head_count: 2 })
            .build();

        let info = branch_info(&app);
        assert!(info.contains("[3/5 head 1/2]"),
            "branching stack should show [3/5 head 1/2], got: {info}");
    }

    #[test]
    fn test_branch_info_no_stack_position() {
        let app = TestAppBuilder::new()
            .with_current_branch(Some("feature"))
            .with_base_branch("main")
            .build();

        let info = branch_info(&app);
        assert!(!info.contains('['),
            "no stack position should show no brackets, got: {info}");
    }

    #[test]
    fn test_truncate_with_ellipsis_no_truncation_needed() {
        assert_eq!(truncate_with_ellipsis("hello", 10), "hello");
        assert_eq!(truncate_with_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_with_ellipsis_truncates_with_dots() {
        assert_eq!(truncate_with_ellipsis("hello world", 8), "hello...");
        assert_eq!(truncate_with_ellipsis("hello world", 6), "hel...");
    }

    #[test]
    fn test_truncate_with_ellipsis_very_short_max() {
        assert_eq!(truncate_with_ellipsis("hello", 3), "...");
        assert_eq!(truncate_with_ellipsis("hello", 2), "..");
        assert_eq!(truncate_with_ellipsis("hello", 1), ".");
        assert_eq!(truncate_with_ellipsis("hello", 0), "");
    }

    #[test]
    fn test_truncate_with_ellipsis_exactly_at_boundary() {
        assert_eq!(truncate_with_ellipsis("hello", 5), "hello");
        assert_eq!(truncate_with_ellipsis("hello", 4), "h...");
    }

    #[test]
    fn test_truncate_with_ellipsis_utf8_characters() {
        assert_eq!(truncate_with_ellipsis("日本語", 3), "日本語"); // fits exactly
        assert_eq!(truncate_with_ellipsis("日本語", 2), ".."); // too short for any char + ...

        assert_eq!(truncate_with_ellipsis("日本語です", 5), "日本語です");
        assert_eq!(truncate_with_ellipsis("日本語です", 4), "日...");

        assert_eq!(truncate_with_ellipsis("🎉🎊🎈", 3), "🎉🎊🎈");
        assert_eq!(truncate_with_ellipsis("🎉🎊🎈", 2), "..");

        assert_eq!(truncate_with_ellipsis("hello日本語", 10), "hello日本語");
        assert_eq!(truncate_with_ellipsis("hello日本語", 8), "hello日本語");
        assert_eq!(truncate_with_ellipsis("hello日本語", 7), "hell...");
    }

    #[test]
    fn test_view_mode_label_commit_only_includes_to_label() {
        let mut app = TestAppBuilder::new()
            .with_current_branch(Some("knmq"))
            .build();
        app.view.view_mode = crate::app::ViewMode::CommitOnly;

        assert_eq!(view_mode_label(&app), " [commit knmq]");
    }

    #[test]
    fn test_view_mode_label_full_mode_is_empty() {
        let app = TestAppBuilder::new().build();
        assert_eq!(view_mode_label(&app), " [all lines]");
    }

    #[test]
    fn test_view_mode_label_context_mode() {
        let mut app = TestAppBuilder::new().build();
        app.view.view_mode = crate::app::ViewMode::Context;
        assert_eq!(view_mode_label(&app), " [context]");
    }

    #[test]
    fn test_view_mode_label_changes_only_mode() {
        let mut app = TestAppBuilder::new().build();
        app.view.view_mode = crate::app::ViewMode::ChangesOnly;
        assert_eq!(view_mode_label(&app), " [changed lines only]");
    }

    #[test]
    fn test_view_mode_label_commit_only_with_bookmarks() {
        let mut app = TestAppBuilder::new()
            .with_current_branch(Some("knmq (main)"))
            .build();
        app.view.view_mode = crate::app::ViewMode::CommitOnly;
        assert_eq!(view_mode_label(&app), " [commit knmq (main)]");
    }

    #[test]
    fn test_view_mode_label_bookmark_only_with_name() {
        let mut app = TestAppBuilder::new().build();
        app.view.view_mode = crate::app::ViewMode::BookmarkOnly;
        app.comparison.bookmark_name = Some("feat/abc".to_string());
        assert_eq!(view_mode_label(&app), " [bookmark feat/abc]");
    }

    #[test]
    fn test_view_mode_label_bookmark_only_without_name() {
        let mut app = TestAppBuilder::new().build();
        app.view.view_mode = crate::app::ViewMode::BookmarkOnly;
        app.comparison.bookmark_name = None;
        assert_eq!(view_mode_label(&app), " [bookmark]");
    }

    fn create_status_bar_test_app(
        current_branch: Option<&str>,
        base_branch: &str,
        file_count: usize,
    ) -> crate::app::App {
        use crate::diff::{DiffLine, FileDiff};

        let files: Vec<FileDiff> = (0..file_count)
            .map(|i| FileDiff {
                lines: vec![DiffLine::file_header(&format!("file{}.rs", i))],
            })
            .collect();

        TestAppBuilder::new()
            .with_files(files)
            .with_base_branch(base_branch)
            .with_current_branch(current_branch)
            .build()
    }

    #[test]
    fn test_status_bar_height_wide_terminal_uses_one_line() {
        let app = create_status_bar_test_app(Some("feature-branch"), "main", 5);
        assert_eq!(status_bar_height(&app, 120), 1);
    }

    #[test]
    fn test_status_bar_height_narrow_terminal_uses_two_lines() {
        let app = create_status_bar_test_app(Some("feature-branch"), "main", 5);
        assert_eq!(status_bar_height(&app, 40), 2);
    }

    #[test]
    fn test_status_bar_height_long_branch_name_needs_two_lines() {
        let app = create_status_bar_test_app(
            Some("very-long-feature-branch-name-that-takes-space"),
            "main",
            5,
        );
        assert_eq!(status_bar_height(&app, 80), 2);
    }

    #[test]
    fn test_status_bar_height_no_current_branch_uses_head() {
        let app = create_status_bar_test_app(None, "main", 5);
        assert_eq!(status_bar_height(&app, 120), 1);
    }

    #[test]
    fn test_status_bar_height_boundary_case() {
        let app = create_status_bar_test_app(Some("feat"), "main", 1);

        let help = " q:quit  j/k:files  g/G:top/bottom  ?:help ";
        let branch_info = "test | feat vs main";

        let stats = format!(
            "{} file{} | +{} -{}{} | {}%",
            app.files.len(),
            if app.files.len() == 1 { "" } else { "s" },
            app.additions_count(),
            app.deletions_count(),
            " [all lines]",
            app.scroll_percentage()
        );
        let full_status = format!("{} | {}", branch_info, stats);

        let threshold = full_status.len() + help.len() + 2;

        assert_eq!(status_bar_height(&app, threshold as u16), 1,
            "At threshold width {} should use 1 line", threshold);

        assert_eq!(status_bar_height(&app, (threshold - 1) as u16), 2,
            "At width {} (one below threshold) should use 2 lines", threshold - 1);
    }
}
