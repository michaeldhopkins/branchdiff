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

use crate::image_diff::{center_in_area, fit_dimensions, CachedImage, ImageDiffState};

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

                // Check for encoding errors and fall back to placeholder if needed
                if let Some(Err(_)) = protocol.last_encoding_result() {
                    render_image_placeholder_box(frame, image_area, cached);
                }
            } else {
                // Fallback: render placeholder box
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

/// Calculate the ideal height for an image diff display
pub fn calculate_image_height(terminal_height: u16) -> u16 {
    // Use about 60% of terminal height for images, with reasonable bounds
    let ideal = (terminal_height as f32 * 0.6) as u16;
    ideal.clamp(MIN_IMAGE_HEIGHT + 4, terminal_height.saturating_sub(6))
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

    #[test]
    fn test_image_diff_state_creation() {
        let state = ImageDiffState {
            before: None,
            after: None,
        };
        assert!(state.before.is_none());
        assert!(state.after.is_none());
    }
}
