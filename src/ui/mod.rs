use ratatui::{
    layout::{Constraint, Direction, Layout},
    Frame,
};

use crate::app::{App, FrameContext};

pub mod colors;
pub mod diff_view;
pub mod image_view;
pub mod modals;
pub mod selection;
pub mod spans;
pub mod status_bar;
pub mod wrapping;

// Re-export commonly used items
pub use modals::{draw_help_modal, draw_warning_banner};
pub use status_bar::{draw_status_bar, status_bar_height};

/// Width of the prefix after line numbers: prefix char + space + status symbol + trailing space
pub const PREFIX_CHAR_WIDTH: usize = 4;

/// Represents how a logical DiffLine maps to a screen row
#[derive(Debug, Clone)]
pub struct ScreenRowInfo {
    /// The actual text content of this screen row (for copy operations)
    pub content: String,
    /// Whether this row is a file header (for collapse detection)
    pub is_file_header: bool,
    /// The file path this row belongs to (for collapse toggle)
    pub file_path: Option<String>,
    /// Whether this row is a continuation of a wrapped line (not start of new logical line)
    pub is_continuation: bool,
}

/// Draw the main UI with a pre-computed frame context
pub fn draw_with_frame(frame: &mut Frame, app: &mut App, ctx: &FrameContext) {
    let size = frame.area();

    let has_warning = app.conflict_warning.is_some() || app.error.is_some();
    let status_height = status_bar_height(app, size.width);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if has_warning {
            vec![
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(status_height),
            ]
        } else {
            vec![
                Constraint::Min(1),
                Constraint::Length(status_height),
            ]
        })
        .split(size);

    let (warning_area, diff_area, status_area) = if has_warning {
        (Some(chunks[0]), chunks[1], chunks[2])
    } else {
        (None, chunks[0], chunks[1])
    };

    if let Some(area) = warning_area {
        if let Some(error) = &app.error {
            draw_warning_banner(frame, error, area);
        } else if let Some(warning) = &app.conflict_warning {
            draw_warning_banner(frame, warning, area);
        }
    }

    let search_bar_rows = u16::from(app.search.is_some());
    let content_height = diff_area.height.saturating_sub(2 + search_bar_rows) as usize;
    app.set_viewport_height(content_height);

    diff_view::draw_diff_view_with_frame(frame, app, diff_area, ctx);
    draw_status_bar(frame, app, status_area);

    if app.view.show_help {
        draw_help_modal(frame, size, app);
    }
}
