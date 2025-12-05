use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::App;

/// Determine how many lines the status bar needs based on content and width
pub fn status_bar_height(app: &App, width: u16) -> u16 {
    let width = width as usize;

    let help = " q:quit  j/k:files  g/G:top/bottom  ?:help ";

    let branch_info = match &app.current_branch {
        Some(b) => format!("{} vs {}", b, app.base_branch),
        None => format!("HEAD vs {}", app.base_branch),
    };

    let file_count = app.files.len();
    let line_count = app.changed_line_count();
    let mode = match app.view_mode {
        crate::app::ViewMode::Full => "",
        crate::app::ViewMode::Context => " [context]",
        crate::app::ViewMode::ChangesOnly => " [changes]",
    };

    let stats = format!(
        "{} file{} | {} line{}{} | {}%",
        file_count,
        if file_count == 1 { "" } else { "s" },
        line_count,
        if line_count == 1 { "" } else { "s" },
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

/// Truncate a string with ellipsis if it exceeds max_len
pub fn truncate_with_ellipsis(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len <= 3 {
        ".".repeat(max_len)
    } else {
        format!("{}...", &s[..max_len - 3])
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
    let line_count = app.changed_line_count();
    let mode = match app.view_mode {
        crate::app::ViewMode::Full => "",
        crate::app::ViewMode::Context => " [context]",
        crate::app::ViewMode::ChangesOnly => " [changes]",
    };

    let stats = format!(
        "{} file{} | {} line{}{} | {}%",
        file_count,
        if file_count == 1 { "" } else { "s" },
        line_count,
        if line_count == 1 { "" } else { "s" },
        mode,
        app.scroll_percentage()
    );

    let branch_info = match &app.current_branch {
        Some(b) => format!("{} vs {}", b, app.base_branch),
        None => format!("HEAD vs {}", app.base_branch),
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
            Line::from(Span::styled(&stats, Style::default().fg(Color::Cyan)))
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
            Line::from(vec![
                Span::styled(&full_status, Style::default().fg(Color::Cyan)),
                Span::raw(" ".repeat(padding)),
                Span::styled(help, Style::default().fg(Color::DarkGray)),
            ])
        } else if full_status.len() + help_short.len() + 2 <= width {
            // Full status + short help fit
            let padding = width.saturating_sub(full_status.len() + help_short.len());
            Line::from(vec![
                Span::styled(&full_status, Style::default().fg(Color::Cyan)),
                Span::raw(" ".repeat(padding)),
                Span::styled(help_short, Style::default().fg(Color::DarkGray)),
            ])
        } else if full_status.len() <= width {
            // Just status fits
            Line::from(Span::styled(&full_status, Style::default().fg(Color::Cyan)))
        } else {
            // Need to truncate - try without branch info first, then truncate branch
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
