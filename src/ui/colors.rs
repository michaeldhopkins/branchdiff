use ratatui::style::{Color, Modifier, Style};

use crate::diff::LineSource;

/// Default foreground color for text (neutral light gray)
pub const DEFAULT_FG: Color = Color::Rgb(200, 200, 200);

/// Minimum contrast ratio for readable text (WCAG AA large text is 3:1)
const MIN_CONTRAST_RATIO: f32 = 3.0;

/// Calculate perceived luminance of a color (0.0 = black, 1.0 = white)
/// Uses ITU-R BT.601 luma coefficients (simplified, without gamma correction)
fn luminance(color: Color) -> f32 {
    match color {
        Color::Rgb(r, g, b) => {
            // ITU-R BT.601 luma coefficients
            (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) / 255.0
        }
        Color::Black => 0.0,
        Color::White => 1.0,
        Color::DarkGray => 0.25,
        Color::Gray => 0.5,
        Color::Red | Color::LightRed => 0.3,
        Color::Green | Color::LightGreen => 0.59,
        Color::Yellow | Color::LightYellow => 0.89,
        Color::Blue | Color::LightBlue => 0.11,
        Color::Magenta | Color::LightMagenta => 0.41,
        Color::Cyan | Color::LightCyan => 0.7,
        // For indexed/reset colors, assume medium luminance
        _ => 0.5,
    }
}

/// Calculate contrast ratio between two colors (1.0 to 21.0, higher = better)
fn contrast_ratio(color1: Color, color2: Color) -> f32 {
    let l1 = luminance(color1);
    let l2 = luminance(color2);
    let (lighter, darker) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
    (lighter + 0.05) / (darker + 0.05)
}

/// Get a contrasting foreground color for a given background.
/// Picks whichever (light or dark) has better contrast with the background.
fn contrasting_fg(bg: Color) -> Color {
    let light = Color::Rgb(220, 220, 220);
    let dark = Color::Rgb(30, 30, 30);

    // Pick whichever foreground color has better contrast with the background
    if contrast_ratio(light, bg) > contrast_ratio(dark, bg) {
        light
    } else {
        dark
    }
}

/// Ensure foreground has sufficient contrast against background.
/// Returns the original fg if contrast is good, otherwise a contrasting color.
pub fn ensure_contrast(fg: Color, bg: Color) -> Color {
    if contrast_ratio(fg, bg) >= MIN_CONTRAST_RATIO {
        fg
    } else {
        contrasting_fg(bg)
    }
}

/// Background color for entire lines (subtle semantic tinting)
/// Note: DeletedBase = committed deletion, DeletedCommitted = staged deletion,
/// DeletedStaged = unstaged deletion (named for where line WAS, not where deletion IS)
pub fn line_bg_color(source: LineSource) -> Color {
    match source {
        LineSource::Base => Color::Reset,  // Use terminal's default background
        LineSource::Committed => Color::Rgb(25, 50, 50),
        LineSource::Staged => Color::Rgb(25, 50, 25),
        LineSource::Unstaged => Color::Rgb(60, 60, 18),
        // Deletion brightness: committed=dark, staged=medium, unstaged=bright
        LineSource::DeletedBase => Color::Rgb(50, 30, 30),         // committed deletion
        LineSource::DeletedCommitted => Color::Rgb(58, 30, 28),    // staged deletion
        LineSource::DeletedStaged => Color::Rgb(65, 30, 25),       // unstaged deletion
        LineSource::CanceledCommitted | LineSource::CanceledStaged => Color::Rgb(50, 25, 50),
        LineSource::FileHeader => Color::Reset,
        LineSource::Elided => Color::Reset,
    }
}

/// Stronger background for inline character-level highlights
pub fn highlight_bg_color(source: LineSource) -> Color {
    match source {
        LineSource::Committed => Color::Rgb(50, 100, 100),
        LineSource::Staged => Color::Rgb(50, 100, 50),
        LineSource::Unstaged => Color::Rgb(130, 130, 35),
        // Deletion brightness: committed=dark, staged=medium, unstaged=bright
        LineSource::DeletedBase => Color::Rgb(95, 55, 55),         // committed deletion
        LineSource::DeletedCommitted => Color::Rgb(105, 52, 52),   // staged deletion
        LineSource::DeletedStaged => Color::Rgb(115, 55, 45),      // unstaged deletion
        LineSource::CanceledCommitted | LineSource::CanceledStaged => Color::Rgb(100, 50, 100),
        _ => line_bg_color(source),
    }
}

/// Foreground color (neutral for syntax highlighting compatibility)
fn line_fg_color(source: LineSource) -> Color {
    match source {
        LineSource::FileHeader => Color::Rgb(220, 220, 220),
        LineSource::Elided => Color::Rgb(90, 90, 95),
        _ => Color::Rgb(200, 200, 200),
    }
}

/// Complete line style with background
pub fn line_style(source: LineSource) -> Style {
    let mut style = Style::default()
        .fg(line_fg_color(source))
        .bg(line_bg_color(source));

    if source == LineSource::FileHeader {
        style = style.add_modifier(Modifier::BOLD);
    }
    if source == LineSource::Elided {
        style = style.add_modifier(Modifier::DIM);
    }

    style
}

/// Style for inline highlighted portions (character-level changes)
pub fn line_style_with_highlight(source: LineSource) -> Style {
    let bg = highlight_bg_color(source);
    let fg = ensure_contrast(line_fg_color(source), bg);
    line_style(source).fg(fg).bg(bg)
}

/// Status symbol for the line source (C=committed, S=staged, U=unstaged)
/// Note: For deletions, the symbol reflects where the DELETION happened, not where the line was.
/// DeletedBase = line deleted in committed changes → C
/// DeletedCommitted = line deleted in staged changes → S
/// DeletedStaged = line deleted in working tree → U
pub fn status_symbol(source: LineSource) -> &'static str {
    match source {
        LineSource::Committed | LineSource::DeletedBase | LineSource::CanceledCommitted => "C",
        LineSource::Staged | LineSource::DeletedCommitted | LineSource::CanceledStaged => "S",
        _ => " ",
    }
}

/// Foreground-only style for non-TUI output (print.rs)
/// Uses the original color scheme without backgrounds
pub fn print_line_style(source: LineSource) -> Style {
    match source {
        LineSource::Base => Style::default().fg(Color::DarkGray),
        LineSource::Committed => Style::default().fg(Color::Cyan),
        LineSource::Staged => Style::default().fg(Color::Green),
        LineSource::Unstaged => Style::default().fg(Color::Yellow),
        LineSource::DeletedBase => Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::DIM),
        LineSource::DeletedCommitted => Style::default().fg(Color::Red),
        LineSource::DeletedStaged => Style::default().fg(Color::Red),
        LineSource::CanceledCommitted => Style::default().fg(Color::Magenta),
        LineSource::CanceledStaged => Style::default().fg(Color::Magenta),
        LineSource::FileHeader => Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
        LineSource::Elided => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    }
}
