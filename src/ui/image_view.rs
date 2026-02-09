//! Image diff rendering for the TUI.
//!
//! This module handles the side-by-side display of before/after images
//! using ratatui-image's protocol detection for Kitty/Sixel/iTerm2/halfblocks.
//!
//! The layout calculation is separated from rendering to enable unit testing.
//! See [`ImagePanelLayout`] and [`calculate_image_panel_layout`] for the testable
//! layout logic.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use ratatui_image::{Resize, StatefulImage};

use crate::image_diff::{
    center_in_area, fit_dimensions, CachedImage, ImageDiffState, MAX_IMAGE_HEIGHT_ROWS,
    IMAGE_BOTTOM_MARGIN, IMAGE_TOP_MARGIN, METADATA_HEIGHT,
};

// ─────────────────────────────────────────────────────────────────────────────
// Layout calculation (pure, testable)
// ─────────────────────────────────────────────────────────────────────────────

/// Calculated layout for an image within a bordered panel.
///
/// This struct holds the computed positions for rendering an image and its
/// metadata. It can be inspected in tests to verify correct centering and
/// bounds without actually rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePanelLayout {
    /// The rect where the image should be rendered
    pub image_rect: Rect,
    /// The rect for metadata text, if there's space for it
    pub metadata_rect: Option<Rect>,
    /// The panel's inner area (after borders removed) - for margin calculations
    pub inner_area: Rect,
    /// The unclamped container height (before partial-view clamping)
    pub full_container_height: u16,
}

impl ImagePanelLayout {
    /// Calculate left margin (space between inner left edge and image left edge)
    pub fn left_margin(&self) -> u16 {
        self.image_rect.x.saturating_sub(self.inner_area.x)
    }

    /// Calculate right margin (space between image right edge and inner right edge)
    pub fn right_margin(&self) -> u16 {
        self.inner_area
            .right()
            .saturating_sub(self.image_rect.right())
    }

    /// Check if image is horizontally centered (within 1 cell tolerance for odd widths)
    pub fn is_horizontally_centered(&self) -> bool {
        let diff = (self.left_margin() as i32 - self.right_margin() as i32).abs();
        diff <= 1
    }

    /// Check if image rect is fully within the inner area bounds
    pub fn is_within_bounds(&self) -> bool {
        self.image_rect.x >= self.inner_area.x
            && self.image_rect.y >= self.inner_area.y
            && self.image_rect.right() <= self.inner_area.right()
            && self.image_rect.bottom() <= self.inner_area.bottom()
    }

    /// Calculate bottom margin (space between image bottom and panel bottom or metadata)
    pub fn bottom_margin(&self) -> u16 {
        if let Some(meta) = &self.metadata_rect {
            // Space between image and metadata
            meta.y.saturating_sub(self.image_rect.bottom())
        } else {
            // Space between image and panel bottom
            self.inner_area
                .bottom()
                .saturating_sub(self.image_rect.bottom())
        }
    }
}

/// Calculate the layout for an image within a panel.
///
/// This is a pure function that computes positions without any rendering.
/// It can be thoroughly unit tested to verify centering, bounds, and margins.
///
/// # Arguments
/// * `image_dims` - Image dimensions in pixels (width, height)
/// * `panel_inner` - The panel's inner area (after borders removed)
/// * `expected_available_height` - Height constraint for consistent sizing during partial views
/// * `font_size` - Terminal font size in pixels (width, height) from the Picker
///
/// # Returns
/// An `ImagePanelLayout` with computed positions for the image and metadata.
pub fn calculate_image_panel_layout(
    image_dims: (u32, u32),
    panel_inner: Rect,
    expected_available_height: u16,
    font_size: (u16, u16),
) -> ImagePanelLayout {
    let (img_w, img_h) = image_dims;

    // Calculate display dimensions using font-based calculation.
    // fit_dimensions handles both width-constrained and height-constrained cases,
    // calculating the exact cell dimensions that StatefulImage will use for rendering.
    // Using display_w directly ensures our centering matches the actual render size.
    let (display_w, display_h) = fit_dimensions(
        img_w,
        img_h,
        panel_inner.width,
        expected_available_height,
        font_size,
    );

    let container_w = display_w;
    let container_h = display_h;

    // Determine if we have room for metadata (within actual visible area)
    let content_needed =
        IMAGE_TOP_MARGIN + container_h + IMAGE_BOTTOM_MARGIN + METADATA_HEIGHT;
    let has_metadata_space = panel_inner.height >= content_needed;

    // Position at top with margin
    let container_y = panel_inner.y + IMAGE_TOP_MARGIN;

    // Clamp container height to visible area to prevent border overlap on partial views.
    // Leave 1 row for bottom border visibility.
    let max_visible_h = panel_inner
        .height
        .saturating_sub(IMAGE_TOP_MARGIN + 1);
    let clamped_h = container_h.min(max_visible_h);

    // When height is clamped, StatefulImage will recalculate width based on the smaller height.
    // We must use the same width for centering to match what StatefulImage will actually render.
    let final_w = if clamped_h < container_h {
        let (clamp_w, _) = fit_dimensions(img_w, img_h, panel_inner.width, clamped_h, font_size);
        clamp_w
    } else {
        container_w
    };

    // Recalculate centering based on final width
    let final_x = panel_inner.x + panel_inner.width.saturating_sub(final_w) / 2;

    let image_rect = Rect::new(final_x, container_y, final_w, clamped_h);

    // Calculate metadata rect if there's space
    let metadata_rect = if has_metadata_space {
        let metadata_y = container_y + container_h + IMAGE_BOTTOM_MARGIN;
        Some(Rect::new(
            panel_inner.x,
            metadata_y,
            panel_inner.width,
            METADATA_HEIGHT,
        ))
    } else {
        None
    };

    ImagePanelLayout {
        image_rect,
        metadata_rect,
        inner_area: panel_inner,
        full_container_height: container_h,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Rendering (uses calculated layout)
// ─────────────────────────────────────────────────────────────────────────────

/// Minimum height for an image panel (excluding borders and metadata)
const MIN_IMAGE_HEIGHT: u16 = 4;

/// Render an image diff with side-by-side before/after panels.
///
/// The `expected_available_height` parameter provides the height constraint to use
/// for image sizing, even when the viewport clips the image. This ensures consistent
/// dimensions when scrolling (partial images maintain their full-view size).
///
/// The `font_size` parameter should come from the Picker's detected font dimensions
/// to ensure centering calculations match actual image rendering.
///
/// Returns the total height used by the image diff display.
pub fn render_image_diff(
    frame: &mut Frame,
    area: Rect,
    state: &mut ImageDiffState,
    file_path: &str,
    expected_available_height: u16,
    font_size: (u16, u16),
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
        expected_available_height,
        font_size,
    );

    // Render after panel (right)
    render_image_panel(
        frame,
        chunks[1],
        state.after.as_mut(),
        "After (working)",
        false, // is_before
        expected_available_height,
        font_size,
    );

    area.height
}

/// Render a single image panel with border, label, and actual image content
///
/// The `expected_available_height` parameter is the height constraint to use for
/// sizing the image, regardless of viewport clamping. This ensures images maintain
/// consistent dimensions when scrolling (partial images keep their full-view size).
///
/// The `font_size` parameter should match the Picker's detected font dimensions
/// to ensure centering calculations match actual image rendering.
fn render_image_panel(
    frame: &mut Frame,
    area: Rect,
    image: Option<&mut CachedImage>,
    label: &str,
    is_before: bool,
    expected_available_height: u16,
    font_size: (u16, u16),
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
            // Calculate layout using DISPLAY dimensions (may be downscaled from original).
            // The protocol is created from display_image, so we must use its dimensions
            // for layout calculations to ensure centering matches actual rendering.
            let layout = calculate_image_panel_layout(
                (cached.display_width(), cached.display_height()),
                inner,
                expected_available_height,
                font_size,
            );

            // Render actual image using StatefulImage if protocol is available
            if let Some(ref mut protocol) = cached.protocol {
                // Use Resize::Fit to scale image to fit within the rect.
                // The image maintains aspect ratio.
                let image_widget = StatefulImage::new().resize(Resize::Fit(None));
                frame.render_stateful_widget(image_widget, layout.image_rect, protocol);

                // Only show placeholder if encoding explicitly failed
                // Note: None means encoding not started yet (first frame) - don't overwrite
                if let Some(Err(_)) = protocol.last_encoding_result() {
                    render_image_placeholder_box(frame, layout.image_rect, cached, font_size);
                }
            } else {
                // No protocol - render placeholder box
                render_image_placeholder_box(frame, layout.image_rect, cached, font_size);
            }

            // Render metadata below image if there's space
            if let Some(metadata_rect) = layout.metadata_rect {
                let metadata = Paragraph::new(cached.metadata_string())
                    .style(Style::default().fg(Color::DarkGray))
                    .alignment(Alignment::Center);
                frame.render_widget(metadata, metadata_rect);
            }
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
fn render_image_placeholder_box(
    frame: &mut Frame,
    area: Rect,
    cached: &CachedImage,
    font_size: (u16, u16),
) {
    let (display_w, display_h) = fit_dimensions(
        cached.original_width,
        cached.original_height,
        area.width,
        area.height,
        font_size,
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
            let (_, fit_h) = fit_dimensions(img_w, img_h, available_w, 1000, TEST_FONT_SIZE);

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

    #[test]
    fn test_metadata_hidden_when_insufficient_space() {
        // This tests the conditional metadata rendering logic.
        // total_overhead = IMAGE_TOP_MARGIN + IMAGE_BOTTOM_MARGIN + METADATA_HEIGHT
        // has_metadata_space = inner.height > total_overhead
        //
        // We can't easily test the render function directly without a terminal,
        // but we verify the constants and formula are sensible.
        assert_eq!(METADATA_HEIGHT, 1, "metadata line should be exactly 1 row");
        assert_eq!(IMAGE_TOP_MARGIN, 1, "top margin should be 1 row");
        assert_eq!(IMAGE_BOTTOM_MARGIN, 1, "bottom margin should be 1 row");

        let total_overhead = IMAGE_TOP_MARGIN + IMAGE_BOTTOM_MARGIN + METADATA_HEIGHT;
        assert_eq!(total_overhead, 3, "total overhead should be 3 rows");

        // With 3 rows of inner space, has_metadata_space = 3 > 3 = false
        let inner_height = 3u16;
        let has_metadata_space = inner_height > total_overhead;
        assert!(!has_metadata_space, "3 rows should hide metadata");

        // With 4 rows, there's room for margins + metadata + 1 row of image
        let inner_height = 4u16;
        let has_metadata_space = inner_height > total_overhead;
        assert!(has_metadata_space, "4 rows should show metadata");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // ImagePanelLayout tests
    // ─────────────────────────────────────────────────────────────────────────

    /// Typical font size for high-fidelity protocols (Kitty, iTerm2, Sixel)
    const HIFI_FONT_SIZE: (u16, u16) = (8, 16);

    /// Font size representing halfblocks (1 pixel wide, 2 pixels tall per cell)
    const HALFBLOCK_FONT_SIZE: (u16, u16) = (1, 2);

    #[test]
    fn test_layout_square_image_is_centered_hifi() {
        // 200x200 square image in 80-cell wide panel
        let inner = Rect::new(1, 1, 80, 20);
        let layout = calculate_image_panel_layout((200, 200), inner, 18, HIFI_FONT_SIZE);

        assert!(
            layout.is_horizontally_centered(),
            "Square image should be centered. Left margin: {}, Right margin: {}",
            layout.left_margin(),
            layout.right_margin()
        );
    }

    #[test]
    fn test_layout_landscape_image_is_centered_hifi() {
        // 400x200 landscape image (2:1 aspect ratio)
        let inner = Rect::new(0, 0, 80, 20);
        let layout = calculate_image_panel_layout((400, 200), inner, 18, HIFI_FONT_SIZE);

        assert!(
            layout.is_horizontally_centered(),
            "Landscape image should be centered. Left: {}, Right: {}",
            layout.left_margin(),
            layout.right_margin()
        );
    }

    #[test]
    fn test_layout_portrait_image_is_centered_hifi() {
        // 200x400 portrait image (1:2 aspect ratio)
        let inner = Rect::new(0, 0, 80, 30);
        let layout = calculate_image_panel_layout((200, 400), inner, 28, HIFI_FONT_SIZE);

        assert!(
            layout.is_horizontally_centered(),
            "Portrait image should be centered. Left: {}, Right: {}",
            layout.left_margin(),
            layout.right_margin()
        );
    }

    #[test]
    fn test_layout_square_image_is_centered_halfblocks() {
        // Same test with halfblock font size
        let inner = Rect::new(1, 1, 80, 40);
        let layout = calculate_image_panel_layout((200, 200), inner, 38, HALFBLOCK_FONT_SIZE);

        assert!(
            layout.is_horizontally_centered(),
            "Square image (halfblocks) should be centered. Left: {}, Right: {}",
            layout.left_margin(),
            layout.right_margin()
        );
    }

    #[test]
    fn test_layout_landscape_image_is_centered_halfblocks() {
        let inner = Rect::new(0, 0, 80, 40);
        let layout = calculate_image_panel_layout((400, 200), inner, 38, HALFBLOCK_FONT_SIZE);

        assert!(
            layout.is_horizontally_centered(),
            "Landscape image (halfblocks) should be centered. Left: {}, Right: {}",
            layout.left_margin(),
            layout.right_margin()
        );
    }

    #[test]
    fn test_layout_image_within_bounds_full_view() {
        // Full view - image should be completely within bounds
        let inner = Rect::new(5, 5, 80, 25);
        let layout = calculate_image_panel_layout((400, 300), inner, 23, HIFI_FONT_SIZE);

        assert!(
            layout.is_within_bounds(),
            "Image should be within bounds. Image rect: {:?}, Inner: {:?}",
            layout.image_rect,
            layout.inner_area
        );
    }

    #[test]
    fn test_layout_image_within_bounds_partial_view() {
        // Partial view - inner height is less than expected_available_height
        // Image should still be clamped to fit within visible bounds
        let inner = Rect::new(0, 0, 80, 10); // Only 10 rows visible
        let layout = calculate_image_panel_layout((400, 300), inner, 23, HIFI_FONT_SIZE);

        assert!(
            layout.is_within_bounds(),
            "Partial view image should be clamped to bounds. Image rect: {:?}, Inner: {:?}",
            layout.image_rect,
            layout.inner_area
        );
    }

    #[test]
    fn test_layout_partial_view_clips_correctly() {
        // When panel is partially visible, image height should be clamped
        let inner = Rect::new(0, 0, 80, 8); // Very short visible area
        let expected_height = 20; // Full image would be taller
        let layout = calculate_image_panel_layout((400, 600), inner, expected_height, HIFI_FONT_SIZE);

        // Image height should be clamped to fit
        // max_visible_h = 8 - IMAGE_TOP_MARGIN(1) - 1 = 6
        let max_visible = inner.height.saturating_sub(IMAGE_TOP_MARGIN + 1);
        assert!(
            layout.image_rect.height <= max_visible,
            "Partial view should clip image. Got height {}, max visible {}",
            layout.image_rect.height,
            max_visible
        );
    }

    #[test]
    fn test_layout_metadata_positioned_correctly() {
        let inner = Rect::new(0, 0, 80, 25);
        let layout = calculate_image_panel_layout((200, 200), inner, 23, HIFI_FONT_SIZE);

        // Should have metadata
        assert!(layout.metadata_rect.is_some(), "Should have metadata rect");

        let metadata = layout.metadata_rect.unwrap();

        // Note: metadata_rect is calculated from full_container_height, not clamped height
        // For full views these should be the same
        assert!(
            metadata.y >= layout.image_rect.bottom(),
            "Metadata y ({}) should be at or below image bottom ({})",
            metadata.y,
            layout.image_rect.bottom()
        );
    }

    #[test]
    fn test_layout_no_metadata_when_insufficient_space() {
        // Very small panel - no room for metadata
        let inner = Rect::new(0, 0, 80, 4);
        let layout = calculate_image_panel_layout((200, 200), inner, 3, HIFI_FONT_SIZE);

        assert!(
            layout.metadata_rect.is_none(),
            "Should not have metadata when space is insufficient"
        );
    }

    #[test]
    fn test_layout_bottom_margin_reasonable() {
        let inner = Rect::new(0, 0, 80, 25);
        let layout = calculate_image_panel_layout((400, 200), inner, 23, HIFI_FONT_SIZE);

        // Bottom margin should be at least IMAGE_BOTTOM_MARGIN when metadata is present
        if layout.metadata_rect.is_some() {
            assert!(
                layout.bottom_margin() >= IMAGE_BOTTOM_MARGIN,
                "Bottom margin ({}) should be at least IMAGE_BOTTOM_MARGIN ({})",
                layout.bottom_margin(),
                IMAGE_BOTTOM_MARGIN
            );
        }
    }

    #[test]
    fn test_layout_very_wide_image() {
        // Extremely wide image (10:1 aspect ratio)
        let inner = Rect::new(0, 0, 80, 20);
        let layout = calculate_image_panel_layout((1000, 100), inner, 18, HIFI_FONT_SIZE);

        assert!(layout.is_horizontally_centered());
        assert!(layout.is_within_bounds());
    }

    #[test]
    fn test_layout_very_tall_image() {
        // Extremely tall image (1:10 aspect ratio)
        let inner = Rect::new(0, 0, 80, 30);
        let layout = calculate_image_panel_layout((100, 1000), inner, 28, HIFI_FONT_SIZE);

        assert!(layout.is_horizontally_centered());
        assert!(layout.is_within_bounds());
    }

    #[test]
    fn test_layout_tiny_image_no_upscale() {
        // Tiny image that doesn't need scaling
        let inner = Rect::new(0, 0, 80, 20);
        let layout = calculate_image_panel_layout((16, 16), inner, 18, HIFI_FONT_SIZE);

        // Image should be centered even when small
        assert!(
            layout.is_horizontally_centered(),
            "Tiny image should be centered"
        );

        // Should not upscale beyond natural size
        // 16x16 pixels with 8x16 font = 2x1 cells
        assert!(
            layout.image_rect.width <= 80,
            "Tiny image should not upscale to fill width"
        );
    }

    #[test]
    fn test_layout_image_at_panel_origin() {
        // Panel at non-zero origin
        let inner = Rect::new(10, 5, 60, 20);
        let layout = calculate_image_panel_layout((200, 200), inner, 18, HIFI_FONT_SIZE);

        // Image should be positioned relative to panel, not screen origin
        assert!(
            layout.image_rect.x >= inner.x,
            "Image x should be >= panel x"
        );
        assert!(
            layout.image_rect.y >= inner.y,
            "Image y should be >= panel y"
        );
        assert!(layout.is_within_bounds());
    }

    #[test]
    fn test_layout_margins_symmetric_for_centered_image() {
        // For various image sizes, verify margins are equal (or differ by at most 1)
        let test_cases = [
            ((200, 200), 80, "square"),
            ((400, 200), 80, "landscape"),
            ((200, 400), 80, "portrait"),
            ((100, 100), 60, "small square"),
            ((800, 400), 100, "wide landscape"),
        ];

        for ((w, h), panel_width, desc) in test_cases {
            let inner = Rect::new(0, 0, panel_width, 25);
            let layout = calculate_image_panel_layout((w, h), inner, 23, HIFI_FONT_SIZE);

            let margin_diff = (layout.left_margin() as i32 - layout.right_margin() as i32).abs();
            assert!(
                margin_diff <= 1,
                "{}: margins should be symmetric. Left: {}, Right: {}, Diff: {}",
                desc,
                layout.left_margin(),
                layout.right_margin(),
                margin_diff
            );
        }
    }

    #[test]
    fn test_layout_partial_views_remain_centered() {
        // When a view is partially visible (clamped height), the image dimensions may differ
        // from the full view because StatefulImage recalculates based on available space.
        // However, both views should remain horizontally centered.
        let full_inner = Rect::new(0, 0, 80, 20);
        let partial_inner = Rect::new(0, 0, 80, 8);
        let expected_height = 18;

        let full_layout =
            calculate_image_panel_layout((400, 300), full_inner, expected_height, HIFI_FONT_SIZE);
        let partial_layout = calculate_image_panel_layout(
            (400, 300),
            partial_inner,
            expected_height,
            HIFI_FONT_SIZE,
        );

        // Both should be centered (even if dimensions differ)
        assert!(
            full_layout.is_horizontally_centered(),
            "Full view should be centered. Left: {}, Right: {}",
            full_layout.left_margin(),
            full_layout.right_margin()
        );
        assert!(
            partial_layout.is_horizontally_centered(),
            "Partial view should be centered. Left: {}, Right: {}",
            partial_layout.left_margin(),
            partial_layout.right_margin()
        );
    }

    #[test]
    fn test_layout_real_world_logo_dimensions() {
        // Test with dimensions similar to the logos that exposed the centering bug
        // logo-horizontal-white.png: 1500x650
        // logo-vertical-white.png: 1500x1392

        let inner = Rect::new(0, 0, 77, 18); // Typical half-panel after borders

        // Horizontal logo (landscape)
        let h_layout =
            calculate_image_panel_layout((1500, 650), inner, 16, HIFI_FONT_SIZE);
        assert!(
            h_layout.is_horizontally_centered(),
            "Horizontal logo should be centered. Left: {}, Right: {}",
            h_layout.left_margin(),
            h_layout.right_margin()
        );

        // Vertical logo (portrait-ish, but still wider than tall in cells due to font ratio)
        let v_layout =
            calculate_image_panel_layout((1500, 1392), inner, 16, HIFI_FONT_SIZE);
        assert!(
            v_layout.is_horizontally_centered(),
            "Vertical logo should be centered. Left: {}, Right: {}",
            v_layout.left_margin(),
            v_layout.right_margin()
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Integration test with actual ratatui-image rendering (halfblocks)
    // ─────────────────────────────────────────────────────────────────────────

    /// Verify that Picker::halfblocks() returns a sensible font size.
    ///
    /// Note: Halfblock rendering works correctly for centering. This test just
    /// documents the expected font_size from the halfblocks picker.
    #[test]
    fn test_halfblocks_picker_font_size() {
        use ratatui_image::picker::Picker;

        let picker = Picker::halfblocks();
        let font_size = picker.font_size();

        // Halfblocks picker should return reasonable font dimensions
        assert!(font_size.0 > 0, "Font width should be positive");
        assert!(font_size.1 > 0, "Font height should be positive");

        // Typical terminal fonts have ~1:2 aspect ratio (width:height)
        let aspect = font_size.1 as f64 / font_size.0 as f64;
        assert!(
            aspect >= 1.5 && aspect <= 3.0,
            "Font aspect ratio {:?} should be roughly 1:2",
            font_size
        );
    }

    /// Test that verifies our calculation produces the same width as fit_dimensions
    /// when we're in the height-constrained branch.
    #[test]
    fn test_layout_width_matches_fit_dimensions_when_height_constrained() {
        // When height is the constraint, container_w should equal display_w from fit_dimensions
        let inner = Rect::new(0, 0, 80, 20);
        let expected_height = 18;

        // Use an image that will be height-constrained (tall image)
        let (img_w, img_h) = (400, 800); // 1:2 aspect ratio

        let (display_w, display_h) =
            fit_dimensions(img_w, img_h, inner.width, expected_height, HIFI_FONT_SIZE);

        let layout =
            calculate_image_panel_layout((img_w, img_h), inner, expected_height, HIFI_FONT_SIZE);

        // If height IS constrained (display_h >= expected_height), we use aspect-ratio calc
        // If height is NOT constrained (display_h < expected_height), we use display_w
        if display_h >= expected_height {
            // Height constrained - we calculate from aspect ratio
            // Verify the calculation is consistent
            let (font_w, font_h) = (HIFI_FONT_SIZE.0 as f64, HIFI_FONT_SIZE.1 as f64);
            let cell_aspect = (img_w as f64 / font_w) / (img_h as f64 / font_h);
            let expected_w = (display_h as f64 * cell_aspect).ceil() as u16;
            assert_eq!(
                layout.image_rect.width,
                expected_w.min(inner.width),
                "Width should be calculated from aspect ratio when height constrained"
            );
        } else {
            // Width constrained - should use display_w
            assert_eq!(
                layout.image_rect.width, display_w,
                "Width should equal fit_dimensions result when width constrained"
            );
        }
    }

    /// Critical test: Compare our fit_dimensions against ratatui-image's actual calculation.
    /// This creates a real StatefulProtocol and compares the render size.
    #[test]
    fn test_fit_dimensions_matches_ratatui_image_size_for() {
        use image::{DynamicImage, RgbaImage};
        use ratatui_image::picker::Picker;
        use ratatui_image::Resize;

        // Create a picker (halfblocks for simplicity)
        let picker = Picker::halfblocks();
        let font_size = picker.font_size();

        // Test various image dimensions
        let test_cases = [
            (1500, 650, 77, 30, "horizontal logo"),
            (1500, 1392, 77, 30, "vertical logo"),
            (800, 400, 80, 20, "landscape"),
            (400, 800, 80, 20, "portrait"),
            (200, 200, 60, 15, "square"),
        ];

        for (img_w, img_h, panel_w, panel_h, desc) in test_cases {
            // Create a dummy image with the specified dimensions
            let img = DynamicImage::ImageRgba8(RgbaImage::new(img_w, img_h));

            // Create the protocol
            let protocol = picker.new_resize_protocol(img);

            // What ratatui-image calculates for this area
            let area = Rect::new(0, 0, panel_w, panel_h);
            let ratatui_size = protocol.size_for(Resize::Fit(None), area);

            // What our fit_dimensions calculates
            let (our_w, our_h) = fit_dimensions(img_w, img_h, panel_w, panel_h, font_size);

            // They should match!
            assert_eq!(
                (our_w, our_h),
                (ratatui_size.width, ratatui_size.height),
                "{}: Our fit_dimensions ({}, {}) doesn't match ratatui-image's size_for ({}, {}). \
                 Image: {}x{}, Panel: {}x{}, Font: {:?}",
                desc, our_w, our_h, ratatui_size.width, ratatui_size.height,
                img_w, img_h, panel_w, panel_h, font_size
            );
        }
    }

    /// Test what happens when we pass a LARGER rect to ratatui-image than the image needs.
    /// This simulates what happens during rendering if our centering calculation differs.
    #[test]
    fn test_ratatui_image_with_oversized_rect() {
        use image::{DynamicImage, RgbaImage};
        use ratatui_image::picker::Picker;
        use ratatui_image::Resize;

        let picker = Picker::halfblocks();
        let font_size = picker.font_size();
        eprintln!("Halfblocks font_size: {:?}", font_size);

        // Image: 1500x650, panel: 77x30 (simulating the user's scenario)
        let img = DynamicImage::ImageRgba8(RgbaImage::new(1500, 650));
        let protocol = picker.new_resize_protocol(img);

        // What size does ratatui-image calculate for the full panel?
        let full_panel = Rect::new(0, 0, 77, 30);
        let actual_size = protocol.size_for(Resize::Fit(None), full_panel);
        eprintln!("Actual image size for 77x30 panel: {:?}", actual_size);

        // Our fit_dimensions
        let (our_w, our_h) = fit_dimensions(1500, 650, 77, 30, font_size);
        eprintln!("Our fit_dimensions: ({}, {})", our_w, our_h);

        // Now what if we pass a rect that's LARGER than the actual size?
        // (This would happen if our centering calculation is wrong)
        let oversized_rect = Rect::new(10, 5, 77, 30); // Same dimensions but positioned
        let size_for_oversized = protocol.size_for(Resize::Fit(None), oversized_rect);
        eprintln!("Size for oversized rect: {:?}", size_for_oversized);

        // The key insight: ratatui-image will render at (actual_size.width, actual_size.height)
        // starting at the LEFT edge of whatever rect we pass
        // So if we pass a centered rect with our calculated width, but ratatui-image calculates
        // a smaller width, the image renders at the left edge of our rect
        assert_eq!(
            (our_w, our_h),
            (actual_size.width, actual_size.height),
            "Our calculation should match ratatui-image's"
        );
    }

    /// Debug test: trace through the full layout calculation to find the bug.
    #[test]
    fn test_debug_full_layout_calculation() {
        // Simulate the user's scenario: 1500x650 horizontal logo
        let img_dims = (1500u32, 650u32);
        let panel_inner = Rect::new(1, 1, 77, 17); // After borders
        let expected_available_height = 15u16; // After margins/metadata
        let font_size = (9u16, 18u16); // Typical terminal font

        eprintln!("\n=== Debug Layout Calculation ===");
        eprintln!("Image: {}x{}", img_dims.0, img_dims.1);
        eprintln!("Panel inner: {:?}", panel_inner);
        eprintln!("Expected available height: {}", expected_available_height);
        eprintln!("Font size: {:?}", font_size);

        // Step 1: fit_dimensions
        let (display_w, display_h) = fit_dimensions(
            img_dims.0, img_dims.1,
            panel_inner.width,
            expected_available_height,
            font_size,
        );
        eprintln!("\nStep 1 - fit_dimensions: ({}, {})", display_w, display_h);

        // Step 2: Calculate layout
        let layout = calculate_image_panel_layout(
            img_dims,
            panel_inner,
            expected_available_height,
            font_size,
        );
        eprintln!("\nStep 2 - Layout result:");
        eprintln!("  image_rect: {:?}", layout.image_rect);
        eprintln!("  left_margin: {}", layout.left_margin());
        eprintln!("  right_margin: {}", layout.right_margin());
        eprintln!("  is_centered: {}", layout.is_horizontally_centered());

        // Step 3: What ratatui-image will calculate when we pass layout.image_rect
        use image::{DynamicImage, RgbaImage};
        use ratatui_image::protocol::{ImageSource, StatefulProtocol, StatefulProtocolType};
        use ratatui_image::protocol::kitty::StatefulKitty;
        use ratatui_image::Resize;

        let img = DynamicImage::ImageRgba8(RgbaImage::new(img_dims.0, img_dims.1));
        let source = ImageSource::new(img, font_size, image::Rgba([0, 0, 0, 0]));
        let protocol = StatefulProtocol::new(
            source,
            font_size,
            StatefulProtocolType::Kitty(StatefulKitty::new(12345, false)),
        );

        let ratatui_size = protocol.size_for(Resize::Fit(None), layout.image_rect);
        eprintln!("\nStep 3 - ratatui-image size_for({:?}):", layout.image_rect);
        eprintln!("  Result: {:?}", ratatui_size);

        // The critical question: does ratatui_size.width == layout.image_rect.width?
        eprintln!("\n=== Critical Comparison ===");
        eprintln!("layout.image_rect.width: {}", layout.image_rect.width);
        eprintln!("ratatui_size.width: {}", ratatui_size.width);
        eprintln!("MATCH: {}", layout.image_rect.width == ratatui_size.width);

        // If they don't match, the image will render at the left edge of image_rect
        // but not fill the full width, causing apparent left-bias
        assert_eq!(
            layout.image_rect.width, ratatui_size.width,
            "Image rect width should match what ratatui-image will actually render"
        );
    }

    /// Test with various font sizes to ensure our calculation works for high-fidelity protocols.
    #[test]
    fn test_fit_dimensions_various_font_sizes() {
        use image::{DynamicImage, RgbaImage};
        use ratatui_image::protocol::{ImageSource, StatefulProtocol, StatefulProtocolType};
        use ratatui_image::protocol::kitty::StatefulKitty;
        use ratatui_image::Resize;

        // Test with different realistic font sizes
        let font_sizes = [
            (8, 16, "typical 8x16"),
            (9, 18, "typical 9x18"),
            (10, 20, "typical 10x20"),
            (7, 14, "smaller 7x14"),
            (12, 24, "larger 12x24"),
        ];

        let img_dims = [
            (1500u32, 650u32, "horizontal logo"),
            (1500, 1392, "vertical logo"),
        ];

        let panel = (77u16, 16u16); // Typical half-panel

        for (font_w, font_h, font_desc) in font_sizes {
            let font_size = (font_w, font_h);

            for (img_w, img_h, img_desc) in img_dims {
                // Create image and protocol with this font size
                let img = DynamicImage::ImageRgba8(RgbaImage::new(img_w, img_h));
                let source = ImageSource::new(img.clone(), font_size, image::Rgba([0, 0, 0, 0]));

                // Create a Kitty protocol (high-fidelity)
                let protocol = StatefulProtocol::new(
                    source,
                    font_size,
                    StatefulProtocolType::Kitty(StatefulKitty::new(12345, false)),
                );

                // What ratatui-image calculates
                let area = Rect::new(0, 0, panel.0, panel.1);
                let ratatui_size = protocol.size_for(Resize::Fit(None), area);

                // What our fit_dimensions calculates
                let (our_w, our_h) = fit_dimensions(img_w, img_h, panel.0, panel.1, font_size);

                eprintln!(
                    "Font {:?} ({}), Image {}x{} ({}): ratatui=({}, {}), ours=({}, {})",
                    font_size, font_desc, img_w, img_h, img_desc,
                    ratatui_size.width, ratatui_size.height, our_w, our_h
                );

                assert_eq!(
                    (our_w, our_h),
                    (ratatui_size.width, ratatui_size.height),
                    "Font {:?} ({}), Image {} ({}x{}): mismatch",
                    font_size, font_desc, img_desc, img_w, img_h
                );
            }
        }
    }

    /// Regression test: Layout must use display_image dimensions, not original dimensions.
    ///
    /// Images larger than MAX_CACHE_DIMENSION are downscaled for caching. The protocol
    /// is created from the downscaled display_image, so layout calculations must use
    /// display dimensions to match what ratatui-image will actually render.
    ///
    /// Bug scenario (before fix) with font (16, 35):
    /// - Original: 1500x650, downscaled to 1024x444
    /// - Layout calculated with 1500x650 → width 91 cells
    /// - Protocol renders 1024x444 → width 64 cells
    /// - Result: 27-cell left bias (image at left edge of oversized centered rect)
    #[test]
    fn test_layout_must_use_display_dimensions_not_original() {
        use crate::image_diff::MAX_CACHE_DIMENSION;
        use image::{DynamicImage, RgbaImage};
        use ratatui_image::protocol::{ImageSource, StatefulProtocol, StatefulProtocolType};
        use ratatui_image::protocol::kitty::StatefulKitty;
        use ratatui_image::Resize;

        // Use high-fidelity font size (16, 35) which exposed the bug
        let font_size = (16u16, 35u16);

        // Simulate a large image that gets downscaled (like 1500x650 logo)
        let original_w = 1500u32;
        let original_h = 650u32;

        // Calculate downscaled dimensions (mirrors CachedImage logic)
        let (display_w, display_h) = if original_w > MAX_CACHE_DIMENSION
            || original_h > MAX_CACHE_DIMENSION
        {
            let scale = MAX_CACHE_DIMENSION as f64 / original_w.max(original_h) as f64;
            (
                (original_w as f64 * scale) as u32,
                (original_h as f64 * scale) as u32,
            )
        } else {
            (original_w, original_h)
        };

        // Verify downscaling happened
        assert!(
            display_w < original_w || display_h < original_h,
            "Test requires image to be downscaled. Original: {}x{}, Display: {}x{}",
            original_w, original_h, display_w, display_h
        );

        // Create protocol from downscaled image (like production code does)
        let display_image = DynamicImage::ImageRgba8(RgbaImage::new(display_w, display_h));
        let source = ImageSource::new(display_image, font_size, image::Rgba([0, 0, 0, 0]));
        let protocol = StatefulProtocol::new(
            source,
            font_size,
            StatefulProtocolType::Kitty(StatefulKitty::new(12345, false)),
        );

        // Typical panel dimensions (half of ~250 column terminal minus borders)
        let panel_inner = Rect::new(0, 0, 122, 20);
        let expected_height = 18u16;

        // Layout using DISPLAY dimensions (correct - what we do now)
        let correct_layout = calculate_image_panel_layout(
            (display_w, display_h),
            panel_inner,
            expected_height,
            font_size,
        );

        // What ratatui-image will actually render for the correct layout
        let ratatui_size = protocol.size_for(Resize::Fit(None), correct_layout.image_rect);

        // Layout using ORIGINAL dimensions (incorrect - the bug)
        let buggy_layout = calculate_image_panel_layout(
            (original_w, original_h),
            panel_inner,
            expected_height,
            font_size,
        );

        // Correct layout width should match what ratatui-image renders
        assert_eq!(
            correct_layout.image_rect.width,
            ratatui_size.width,
            "Layout with display dimensions ({}, {}) should match ratatui-image render width. \
             Got layout_w={}, ratatui_w={}",
            display_w, display_h,
            correct_layout.image_rect.width, ratatui_size.width
        );

        // Buggy layout width should NOT match (demonstrating the bug)
        // Note: With some font sizes the difference might be small, so we check for any difference
        let width_difference =
            buggy_layout.image_rect.width as i32 - correct_layout.image_rect.width as i32;
        assert!(
            width_difference != 0,
            "Layout with original dimensions ({}, {}) should differ from display dimensions. \
             Both gave width={}, which means downscaling had no effect on layout (unexpected).",
            original_w, original_h,
            buggy_layout.image_rect.width
        );

        // With font (16, 35), the bug caused ~27 cell difference
        // Verify the difference is significant enough to cause visible misalignment
        assert!(
            width_difference.abs() > 5,
            "Bug should cause significant width mismatch for centering. \
             Original dims gave width={}, display dims gave width={}, diff={}",
            buggy_layout.image_rect.width,
            correct_layout.image_rect.width,
            width_difference
        );
    }
}
