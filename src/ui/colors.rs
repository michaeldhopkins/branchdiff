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

#[cfg(test)]
mod tests {
    use super::*;

    // === Luminance tests ===

    #[test]
    fn test_luminance_black() {
        assert!((luminance(Color::Black) - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_luminance_white() {
        assert!((luminance(Color::White) - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_luminance_rgb_black() {
        assert!((luminance(Color::Rgb(0, 0, 0)) - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_luminance_rgb_white() {
        assert!((luminance(Color::Rgb(255, 255, 255)) - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_luminance_rgb_mid_gray() {
        let lum = luminance(Color::Rgb(128, 128, 128));
        // Should be around 0.5
        assert!(lum > 0.4 && lum < 0.6, "Mid gray luminance: {}", lum);
    }

    #[test]
    fn test_luminance_green_brighter_than_blue() {
        // Green has higher perceptual luminance than blue
        let green_lum = luminance(Color::Rgb(0, 255, 0));
        let blue_lum = luminance(Color::Rgb(0, 0, 255));
        assert!(green_lum > blue_lum, "Green ({}) should be brighter than blue ({})", green_lum, blue_lum);
    }

    #[test]
    fn test_luminance_named_colors() {
        // Verify named colors return reasonable values
        assert!(luminance(Color::Red) > 0.0 && luminance(Color::Red) < 1.0);
        assert!(luminance(Color::Green) > 0.0 && luminance(Color::Green) < 1.0);
        assert!(luminance(Color::Blue) > 0.0 && luminance(Color::Blue) < 1.0);
        assert!(luminance(Color::Yellow) > luminance(Color::Blue)); // Yellow is bright
    }

    // === Contrast ratio tests ===

    #[test]
    fn test_contrast_ratio_black_white() {
        let ratio = contrast_ratio(Color::Black, Color::White);
        // Maximum contrast should be close to 21:1
        assert!(ratio > 20.0, "Black/white contrast: {}", ratio);
    }

    #[test]
    fn test_contrast_ratio_same_color() {
        let ratio = contrast_ratio(Color::Rgb(100, 100, 100), Color::Rgb(100, 100, 100));
        // Same color should have ratio 1.0
        assert!((ratio - 1.0).abs() < 0.01, "Same color contrast: {}", ratio);
    }

    #[test]
    fn test_contrast_ratio_symmetric() {
        let ratio1 = contrast_ratio(Color::Red, Color::Blue);
        let ratio2 = contrast_ratio(Color::Blue, Color::Red);
        assert!((ratio1 - ratio2).abs() < 0.01, "Contrast should be symmetric");
    }

    #[test]
    fn test_contrast_ratio_similar_colors_low() {
        // Similar colors should have low contrast
        let ratio = contrast_ratio(Color::Rgb(100, 100, 100), Color::Rgb(110, 110, 110));
        assert!(ratio < 1.5, "Similar colors contrast: {}", ratio);
    }

    // === ensure_contrast tests ===

    #[test]
    fn test_ensure_contrast_good_contrast_unchanged() {
        // White on black has great contrast - should remain unchanged
        let result = ensure_contrast(Color::White, Color::Black);
        assert_eq!(result, Color::White);
    }

    #[test]
    fn test_ensure_contrast_bad_contrast_fixed() {
        // Black on dark gray has poor contrast - should be fixed
        let dark_gray = Color::Rgb(30, 30, 30);
        let result = ensure_contrast(Color::Black, dark_gray);
        // Result should have good contrast with dark_gray
        let ratio = contrast_ratio(result, dark_gray);
        assert!(ratio >= MIN_CONTRAST_RATIO, "Fixed contrast ratio: {}", ratio);
    }

    #[test]
    fn test_ensure_contrast_light_on_light_fixed() {
        // Light gray on white - should switch to dark
        let light = Color::Rgb(240, 240, 240);
        let result = ensure_contrast(light, Color::White);
        // Result should be darker
        let ratio = contrast_ratio(result, Color::White);
        assert!(ratio >= MIN_CONTRAST_RATIO, "Fixed light-on-light ratio: {}", ratio);
    }

    // === line_bg_color tests ===

    #[test]
    fn test_line_bg_color_base_is_reset() {
        assert_eq!(line_bg_color(LineSource::Base), Color::Reset);
    }

    #[test]
    fn test_line_bg_color_file_header_is_reset() {
        assert_eq!(line_bg_color(LineSource::FileHeader), Color::Reset);
    }

    #[test]
    fn test_line_bg_color_additions_have_distinct_colors() {
        let committed = line_bg_color(LineSource::Committed);
        let staged = line_bg_color(LineSource::Staged);
        let unstaged = line_bg_color(LineSource::Unstaged);

        assert_ne!(committed, staged);
        assert_ne!(staged, unstaged);
        assert_ne!(committed, unstaged);
    }

    #[test]
    fn test_line_bg_color_deletions_have_distinct_colors() {
        let del_base = line_bg_color(LineSource::DeletedBase);
        let del_committed = line_bg_color(LineSource::DeletedCommitted);
        let del_staged = line_bg_color(LineSource::DeletedStaged);

        assert_ne!(del_base, del_committed);
        assert_ne!(del_committed, del_staged);
        assert_ne!(del_base, del_staged);
    }

    #[test]
    fn test_line_bg_color_canceled_same_color() {
        // Both canceled sources use the same color
        assert_eq!(
            line_bg_color(LineSource::CanceledCommitted),
            line_bg_color(LineSource::CanceledStaged)
        );
    }

    // === highlight_bg_color tests ===

    #[test]
    fn test_highlight_bg_color_brighter_than_line_bg() {
        // Highlight colors should generally be brighter/more saturated
        let line_bg = line_bg_color(LineSource::Committed);
        let highlight_bg = highlight_bg_color(LineSource::Committed);
        assert_ne!(line_bg, highlight_bg, "Highlight should differ from line bg");

        // For sources that fall through to line_bg_color, they should be equal
        assert_eq!(
            highlight_bg_color(LineSource::Base),
            line_bg_color(LineSource::Base)
        );
    }

    #[test]
    fn test_highlight_bg_color_all_sources_have_values() {
        // Verify all sources return valid colors
        let sources = [
            LineSource::Committed,
            LineSource::Staged,
            LineSource::Unstaged,
            LineSource::DeletedBase,
            LineSource::DeletedCommitted,
            LineSource::DeletedStaged,
            LineSource::CanceledCommitted,
            LineSource::CanceledStaged,
        ];

        for source in sources {
            let color = highlight_bg_color(source);
            // Just verify it returns something (not panicking)
            assert!(!matches!(color, Color::Reset) || source == LineSource::Base);
        }
    }

    // === line_style tests ===

    #[test]
    fn test_line_style_file_header_bold() {
        let style = line_style(LineSource::FileHeader);
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_line_style_elided_dim() {
        let style = line_style(LineSource::Elided);
        assert!(style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn test_line_style_has_fg_and_bg() {
        let style = line_style(LineSource::Committed);
        assert!(style.fg.is_some(), "Style should have foreground color");
        assert!(style.bg.is_some(), "Style should have background color");
    }

    // === line_style_with_highlight tests ===

    #[test]
    fn test_line_style_with_highlight_uses_contrast_correction() {
        // Verify ensure_contrast is applied - fg should differ from raw line_fg_color
        // when the highlight bg doesn't have sufficient contrast
        let style = line_style_with_highlight(LineSource::Committed);
        let highlight_bg = highlight_bg_color(LineSource::Committed);
        let raw_fg = Color::Rgb(200, 200, 200); // line_fg_color default

        if let (Some(fg), Some(bg)) = (style.fg, style.bg) {
            assert_eq!(bg, highlight_bg, "Should use highlight bg color");
            // If raw contrast was bad, ensure_contrast should have corrected it
            let raw_contrast = contrast_ratio(raw_fg, bg);
            if raw_contrast < MIN_CONTRAST_RATIO {
                // The corrected fg should have better or equal contrast
                let corrected_contrast = contrast_ratio(fg, bg);
                assert!(corrected_contrast >= raw_contrast,
                    "Corrected contrast ({}) should be >= raw ({})",
                    corrected_contrast, raw_contrast);
            }
        }
    }

    #[test]
    fn test_line_style_with_highlight_all_sources() {
        let sources = [
            LineSource::Committed,
            LineSource::Staged,
            LineSource::Unstaged,
            LineSource::DeletedBase,
            LineSource::DeletedCommitted,
            LineSource::DeletedStaged,
        ];

        for source in sources {
            let style = line_style_with_highlight(source);
            assert!(style.fg.is_some(), "{:?} should have fg", source);
            assert!(style.bg.is_some(), "{:?} should have bg", source);
        }
    }

    // === status_symbol tests ===

    #[test]
    fn test_status_symbol_committed() {
        assert_eq!(status_symbol(LineSource::Committed), "C");
    }

    #[test]
    fn test_status_symbol_staged() {
        assert_eq!(status_symbol(LineSource::Staged), "S");
    }

    #[test]
    fn test_status_symbol_base() {
        assert_eq!(status_symbol(LineSource::Base), " ");
    }

    #[test]
    fn test_status_symbol_unstaged() {
        assert_eq!(status_symbol(LineSource::Unstaged), " ");
    }

    #[test]
    fn test_status_symbol_deletions() {
        // Deletion symbols reflect where deletion happened
        assert_eq!(status_symbol(LineSource::DeletedBase), "C"); // committed deletion
        assert_eq!(status_symbol(LineSource::DeletedCommitted), "S"); // staged deletion
        assert_eq!(status_symbol(LineSource::DeletedStaged), " "); // working tree deletion
    }

    #[test]
    fn test_status_symbol_canceled() {
        assert_eq!(status_symbol(LineSource::CanceledCommitted), "C");
        assert_eq!(status_symbol(LineSource::CanceledStaged), "S");
    }

    // === print_line_style tests ===

    #[test]
    fn test_print_line_style_base() {
        let style = print_line_style(LineSource::Base);
        assert_eq!(style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn test_print_line_style_committed() {
        let style = print_line_style(LineSource::Committed);
        assert_eq!(style.fg, Some(Color::Cyan));
    }

    #[test]
    fn test_print_line_style_staged() {
        let style = print_line_style(LineSource::Staged);
        assert_eq!(style.fg, Some(Color::Green));
    }

    #[test]
    fn test_print_line_style_unstaged() {
        let style = print_line_style(LineSource::Unstaged);
        assert_eq!(style.fg, Some(Color::Yellow));
    }

    #[test]
    fn test_print_line_style_deletions_red() {
        assert_eq!(print_line_style(LineSource::DeletedBase).fg, Some(Color::Red));
        assert_eq!(print_line_style(LineSource::DeletedCommitted).fg, Some(Color::Red));
        assert_eq!(print_line_style(LineSource::DeletedStaged).fg, Some(Color::Red));
    }

    #[test]
    fn test_print_line_style_deleted_base_dim() {
        let style = print_line_style(LineSource::DeletedBase);
        assert!(style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn test_print_line_style_canceled_magenta() {
        assert_eq!(print_line_style(LineSource::CanceledCommitted).fg, Some(Color::Magenta));
        assert_eq!(print_line_style(LineSource::CanceledStaged).fg, Some(Color::Magenta));
    }

    #[test]
    fn test_print_line_style_file_header_bold_white() {
        let style = print_line_style(LineSource::FileHeader);
        assert_eq!(style.fg, Some(Color::White));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_print_line_style_elided_dim() {
        let style = print_line_style(LineSource::Elided);
        assert_eq!(style.fg, Some(Color::DarkGray));
        assert!(style.add_modifier.contains(Modifier::DIM));
    }
}
