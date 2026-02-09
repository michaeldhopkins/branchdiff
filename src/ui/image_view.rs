//! Image diff rendering for the TUI.
//!
//! This module handles the side-by-side display of before/after images
//! using ratatui-image's protocol detection for Kitty/Sixel/iTerm2/halfblocks.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use ratatui_image::StatefulImage;

use crate::image_diff::{
    center_in_area, fit_dimensions, CachedImage, ImageDiffState, MAX_IMAGE_HEIGHT_ROWS,
};

/// Height of the metadata line below each image panel
const METADATA_HEIGHT: u16 = 1;

/// Minimum height for an image panel (excluding borders and metadata)
const MIN_IMAGE_HEIGHT: u16 = 4;

/// Render an image diff with side-by-side before/after panels.
///
/// Returns the total height used by the image diff display.
pub fn render_image_diff(
    frame: &mut Frame,
    area: Rect,
    state: &mut ImageDiffState,
    file_path: &str,
) -> u16 {
    // Calculate available height for image content
    let available_height = area.height.saturating_sub(4); // Borders + metadata + labels

    if available_height < MIN_IMAGE_HEIGHT {
        // Not enough space - render compact placeholder
        render_compact_placeholder(frame, area, file_path);
        return area.height;
    }

    // Split horizontally for before/after panels
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    // Render before panel (left)
    render_image_panel(
        frame,
        chunks[0],
        state.before.as_mut(),
        "Before (base)",
        true, // is_before
    );

    // Render after panel (right)
    render_image_panel(
        frame,
        chunks[1],
        state.after.as_mut(),
        "After (working)",
        false, // is_before
    );

    area.height
}

/// Render a single image panel with border, label, and actual image content
fn render_image_panel(
    frame: &mut Frame,
    area: Rect,
    image: Option<&mut CachedImage>,
    label: &str,
    is_before: bool,
) {
    let border_color = if is_before {
        Color::Red
    } else {
        Color::Green
    };

    let block = Block::default()
        .title(label)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    match image {
        Some(cached) => {
            // Calculate display dimensions
            let (display_w, display_h) = fit_dimensions(
                cached.original_width,
                cached.original_height,
                inner.width,
                inner.height.saturating_sub(METADATA_HEIGHT),
            );

            // Calculate image area (above metadata)
            let image_area = Rect::new(
                inner.x,
                inner.y,
                inner.width,
                inner.height.saturating_sub(METADATA_HEIGHT),
            );

            // Render actual image using StatefulImage if protocol is available
            if let Some(ref mut protocol) = cached.protocol {
                // Center the image within the available area
                let centered = center_in_area(display_w, display_h, image_area);

                // Create and render the StatefulImage widget
                let image_widget = StatefulImage::new();
                frame.render_stateful_widget(image_widget, centered, protocol);

                // Only show placeholder if encoding explicitly failed
                // Note: None means encoding not started yet (first frame) - don't overwrite
                if let Some(Err(_)) = protocol.last_encoding_result() {
                    render_image_placeholder_box(frame, image_area, cached);
                }
            } else {
                // No protocol - render placeholder box
                render_image_placeholder_box(frame, image_area, cached);
            }

            // Render metadata below image
            let metadata_area = Rect::new(
                inner.x,
                inner.y + inner.height.saturating_sub(METADATA_HEIGHT),
                inner.width,
                METADATA_HEIGHT,
            );
            let metadata = Paragraph::new(cached.metadata_string())
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center);
            frame.render_widget(metadata, metadata_area);
        }
        None => {
            // No image - show placeholder
            let msg = if is_before {
                "(new file)"
            } else {
                "(deleted)"
            };
            let para = Paragraph::new(msg)
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center);
            frame.render_widget(para, inner);
        }
    }
}

/// Render a placeholder box when protocol is not available
fn render_image_placeholder_box(frame: &mut Frame, area: Rect, cached: &CachedImage) {
    let (display_w, display_h) = fit_dimensions(
        cached.original_width,
        cached.original_height,
        area.width,
        area.height,
    );

    let centered = center_in_area(display_w, display_h, area);

    let placeholder = format!(
        "{}x{} {}",
        cached.original_width, cached.original_height, cached.format_name
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(centered);
    frame.render_widget(block, centered);

    if inner.height > 0 && inner.width > 0 {
        let para = Paragraph::new(placeholder)
            .style(Style::default().fg(Color::Cyan))
            .alignment(Alignment::Center);
        frame.render_widget(para, inner);
    }
}

/// Render a compact placeholder when there's not enough vertical space
fn render_compact_placeholder(frame: &mut Frame, area: Rect, file_path: &str) {
    let text = format!("[image: {}]", file_path);
    let para = Paragraph::new(text)
        .style(Style::default().fg(Color::Cyan))
        .alignment(Alignment::Left);
    frame.render_widget(para, area);
}

/// Calculate the ideal height for an image diff display based on actual image dimensions.
/// Takes the larger of the two images (before/after) to determine panel height.
///
/// Converts image pixel dimensions to cell dimensions using font metrics,
/// then calculates the height needed to display without upscaling.
/// Height is capped at MAX_IMAGE_HEIGHT_ROWS to prevent excessive allocation.
///
/// Note: This only constrains by width (not height) because we're *calculating* the
/// required height, not fitting into a fixed area. The width is fixed by the panel,
/// and we determine what height results from scaling to fit that width. This differs
/// from `fit_dimensions()` which constrains both dimensions to fit a given area.
///
/// The `font_size` parameter should come from the Picker's detected font dimensions
/// to ensure consistency between height calculation and actual image rendering.
pub fn calculate_image_height_for_images(
    before: Option<(u32, u32)>,
    after: Option<(u32, u32)>,
    panel_width: u16,
    font_size: (u16, u16),
) -> u16 {
    let font_w = font_size.0 as f64;
    let font_h = font_size.1 as f64;

    // Get the max dimensions from both images
    let (img_w, img_h) = match (before, after) {
        (Some((bw, bh)), Some((aw, ah))) => (bw.max(aw), bh.max(ah)),
        (Some((w, h)), None) | (None, Some((w, h))) => (w, h),
        (None, None) => return MIN_IMAGE_HEIGHT + 4,
    };

    // Each panel gets half the width, minus borders (2 chars each side)
    let available_cells_w = panel_width.saturating_sub(4) / 2;
    if available_cells_w == 0 {
        return MIN_IMAGE_HEIGHT + 4;
    }

    // Convert image pixel dimensions to cell dimensions
    let img_cells_w = img_w as f64 / font_w;
    let img_cells_h = img_h as f64 / font_h;

    // Calculate scale to fit width (never upscale)
    let scale = (available_cells_w as f64 / img_cells_w).min(1.0);

    // Apply scale to get display height in cells
    let display_h = (img_cells_h * scale).ceil() as u16;

    // Add space for borders (2) + title (1) + metadata (1) = 4
    let total_height = display_h.saturating_add(4);

    // Enforce minimum and maximum
    total_height.clamp(MIN_IMAGE_HEIGHT + 4, MAX_IMAGE_HEIGHT_ROWS)
}

/// Calculate the ideal height for an image diff display (fallback when dimensions unknown)
pub fn calculate_image_height(terminal_height: u16) -> u16 {
    // Fallback: use about 40% of terminal height for images
    let ideal = (terminal_height as f32 * 0.4) as u16;
    let min_height = MIN_IMAGE_HEIGHT + 4; // 8 rows minimum
    let max_height = terminal_height.saturating_sub(6);

    // Handle very small terminals where max < min
    if max_height < min_height {
        // Use half the terminal or minimum viable, whichever is larger
        return terminal_height.saturating_div(2).max(MIN_IMAGE_HEIGHT);
    }

    ideal.clamp(min_height, max_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_image_height() {
        // Small terminal
        let height = calculate_image_height(24);
        assert!(height >= MIN_IMAGE_HEIGHT + 4);
        assert!(height <= 24 - 6);

        // Large terminal
        let height = calculate_image_height(80);
        assert!(height >= MIN_IMAGE_HEIGHT + 4);
        assert!(height <= 80 - 6);
    }

    // Default font size for tests (matches FONT_WIDTH_PX, FONT_HEIGHT_PX)
    const TEST_FONT_SIZE: (u16, u16) = (8, 16);

    #[test]
    fn test_calculate_image_height_for_images_with_dimensions() {
        // 192x192 image in 100-char wide panel (50 per side, minus borders = 46)
        let height = calculate_image_height_for_images(Some((192, 192)), None, 100, TEST_FONT_SIZE);
        // With 8x16 font: 192/8=24 cells wide, 192/16=12 cells tall
        // Since 24 < 46 (available width), no scaling needed
        // Height = 12 + 4 (borders/metadata) = 16
        assert!(height >= MIN_IMAGE_HEIGHT + 4);
        assert!(height < 100); // Should be much less than panel width
    }

    #[test]
    fn test_calculate_image_height_small_image_no_upscale() {
        // Small 64x64 image should NOT be upscaled to fill panel
        // At 8x16 font: 64/8=8 cells wide, 64/16=4 cells tall
        let height = calculate_image_height_for_images(Some((64, 64)), None, 120, TEST_FONT_SIZE);
        // Height should be 4 (image) + 4 (borders) = 8
        assert_eq!(height, MIN_IMAGE_HEIGHT + 4);
    }

    #[test]
    fn test_calculate_image_height_large_image_scales_down() {
        // Large 1024x1024 image in 120-char panel
        // Available width per panel: (120-4)/2 = 58 cells
        // Image in cells: 1024/8 = 128 cells wide, 1024/16 = 64 cells tall
        // Scale to fit width: 58/128 = 0.453
        // Scaled height: 64 * 0.453 = 29 cells
        let height = calculate_image_height_for_images(Some((1024, 1024)), None, 120, TEST_FONT_SIZE);
        // Height should be 29 (image) + 4 (borders) = 33
        assert!(height >= 30); // At least 30 rows for a large scaled image
        assert!(height <= 40); // But not excessive
    }

    #[test]
    fn test_calculate_image_height_for_images_uses_larger_dimension() {
        // Before is 100x100, After is 200x200 - should use 200x200
        let height_both = calculate_image_height_for_images(Some((100, 100)), Some((200, 200)), 100, TEST_FONT_SIZE);
        let height_large_only = calculate_image_height_for_images(None, Some((200, 200)), 100, TEST_FONT_SIZE);
        assert_eq!(height_both, height_large_only);
    }

    #[test]
    fn test_calculate_image_height_for_images_no_dimensions() {
        // No image dimensions - should return minimum
        let height = calculate_image_height_for_images(None, None, 100, TEST_FONT_SIZE);
        assert_eq!(height, MIN_IMAGE_HEIGHT + 4);
    }

    #[test]
    fn test_calculate_image_height_for_images_narrow_panel() {
        // Very narrow panel - should still work
        let height = calculate_image_height_for_images(Some((192, 192)), None, 20, TEST_FONT_SIZE);
        assert!(height >= MIN_IMAGE_HEIGHT + 4);
    }

    #[test]
    fn test_calculate_image_height_landscape_image() {
        // Wide landscape image: 800x200 pixels
        // At 8x16 font: 800/8 = 100 cells wide, 200/16 = 12.5 cells tall
        // In 120-char panel: available = (120-4)/2 = 58 cells
        // Image needs 100 cells but only 58 available, so scale = 58/100 = 0.58
        // Scaled height: 12.5 * 0.58 = 7.25, ceil = 8 cells
        let height = calculate_image_height_for_images(Some((800, 200)), None, 120, TEST_FONT_SIZE);
        // Height should be modest: 8 (image) + 4 (borders) = 12
        assert!(height >= MIN_IMAGE_HEIGHT + 4);
        assert!(height <= 20); // Landscape should not be tall
    }

    #[test]
    fn test_calculate_image_height_portrait_image() {
        // Tall portrait image: 200x800 pixels
        // At 8x16 font: 25 cells wide, 50 cells tall (800/16 = 50)
        // In 120-char panel: available = (120-4)/2 = 58 cells
        // Image fits at natural width (25 < 58), no scaling needed
        // Height: 50 cells + 4 (borders) = 54, but capped at MAX_IMAGE_HEIGHT_ROWS
        let height = calculate_image_height_for_images(Some((200, 800)), None, 120, TEST_FONT_SIZE);
        assert!(height <= MAX_IMAGE_HEIGHT_ROWS);
    }

    #[test]
    fn test_calculate_image_height_respects_max_cap() {
        use crate::image_diff::MAX_IMAGE_HEIGHT_ROWS;
        // Very tall image: 64x4096 pixels
        // At 8x16 font: 8 cells wide, 256 cells tall
        // Without cap, this would be 256 + 4 = 260 rows
        let height = calculate_image_height_for_images(Some((64, 4096)), None, 120, TEST_FONT_SIZE);
        assert_eq!(height, MAX_IMAGE_HEIGHT_ROWS);
    }

    #[test]
    fn test_image_diff_state_creation() {
        let state = ImageDiffState {
            before: None,
            after: None,
        };
        assert!(state.before.is_none());
        assert!(state.after.is_none());
    }

    #[test]
    fn test_height_calculation_consistent_with_fit_dimensions() {
        // Verify that calculate_image_height_for_images and fit_dimensions
        // produce consistent results for the same image and width constraint.
        //
        // calculate_image_height_for_images: calculates required height given width
        // fit_dimensions: fits into area, constrained by both dimensions
        //
        // When fit_dimensions is given an unconstrained height (very large),
        // it should produce the same height as calculate_image_height_for_images.

        let test_cases = [
            (192, 192, 120),   // Square, fits without scaling
            (1024, 1024, 120), // Square, needs scaling
            (800, 200, 120),   // Landscape
            (200, 800, 120),   // Portrait
            (64, 64, 120),     // Small, no upscale
        ];

        for (img_w, img_h, panel_width) in test_cases {
            // Available width per panel (same calculation as in calculate_image_height_for_images)
            let available_w = (panel_width - 4) / 2;

            // Get height from calculate_image_height_for_images (subtract 4 for borders/metadata)
            let calc_total = calculate_image_height_for_images(Some((img_w, img_h)), None, panel_width, TEST_FONT_SIZE);
            let calc_image_h = calc_total.saturating_sub(4);

            // Get height from fit_dimensions with unconstrained height
            let (_, fit_h) = fit_dimensions(img_w, img_h, available_w, 1000);

            // They should match (within the bounds of MAX cap)
            if calc_total < MAX_IMAGE_HEIGHT_ROWS {
                assert_eq!(
                    calc_image_h, fit_h,
                    "Mismatch for {}x{} in {} panel: calc={}, fit={}",
                    img_w, img_h, panel_width, calc_image_h, fit_h
                );
            }
        }
    }
}
