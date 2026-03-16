//! View-related state extracted from App.
//!
//! Handles scrolling, layout, selection, and display settings.

use std::collections::HashSet;
use std::time::Instant;

use crate::ui::ScreenRowInfo;

use super::selection::Selection;
use super::ViewMode;

/// View-related state for the diff viewer.
///
/// Extracted from App to reduce god-object complexity and improve testability.
/// Contains all state related to:
/// - Scrolling and viewport
/// - View mode and help display
/// - Layout information
/// - Text selection
/// - File collapse state
/// - UI timing/feedback
#[derive(Debug)]
pub struct ViewState {
    // Scrolling & Viewport
    pub scroll_offset: usize,
    pub viewport_height: usize,

    // View mode
    pub view_mode: ViewMode,

    // Layout information (set during render)
    pub content_offset: (u16, u16),
    pub line_num_width: usize,
    pub content_width: usize,
    pub panel_width: u16,

    // Help modal
    pub show_help: bool,

    // Selection state
    pub selection: Option<Selection>,
    pub word_selection_anchor: Option<(usize, usize, usize)>,
    pub line_selection_anchor: Option<(usize, usize)>,
    pub row_map: Vec<ScreenRowInfo>,

    // File collapse
    pub collapsed_files: HashSet<String>,
    pub manually_toggled: HashSet<String>,

    // Dirty flags & UI timing
    pub needs_inline_spans: bool,
    pub path_copied_at: Option<Instant>,
    pub last_click: Option<(Instant, u16, u16, u8)>,

    // Deferred auto-copy (waits for multi-click window to expire)
    pub pending_copy: Option<Instant>,

    // Status bar text for selection support
    pub status_bar_lines: Vec<String>,
    pub status_bar_screen_y: u16,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            scroll_offset: 0,
            viewport_height: 0,
            view_mode: ViewMode::default(),
            content_offset: (0, 0),
            line_num_width: 0,
            content_width: 0,
            panel_width: 0,
            show_help: false,
            selection: None,
            word_selection_anchor: None,
            line_selection_anchor: None,
            row_map: Vec::new(),
            collapsed_files: HashSet::new(),
            manually_toggled: HashSet::new(),
            needs_inline_spans: true,
            path_copied_at: None,
            last_click: None,
            pending_copy: None,
            status_bar_lines: Vec::new(),
            status_bar_screen_y: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_view_state_default_values() {
        let state = ViewState::default();

        // Scrolling starts at top
        assert_eq!(state.scroll_offset, 0);
        assert_eq!(state.viewport_height, 0);

        // Context view mode by default (shows changes with surrounding context)
        assert_eq!(state.view_mode, ViewMode::Context);

        // No help on startup
        assert!(!state.show_help);

        // No selection initially
        assert!(state.selection.is_none());
        assert!(state.word_selection_anchor.is_none());
        assert!(state.line_selection_anchor.is_none());

        // No files collapsed initially
        assert!(state.collapsed_files.is_empty());
        assert!(state.manually_toggled.is_empty());

        // Critical: needs_inline_spans MUST be true to trigger initial computation
        assert!(state.needs_inline_spans, "needs_inline_spans must default to true");

        // No UI feedback timestamps
        assert!(state.path_copied_at.is_none());
        assert!(state.last_click.is_none());

        // No pending auto-copy
        assert!(state.pending_copy.is_none());

        // No status bar text
        assert!(state.status_bar_lines.is_empty());
        assert_eq!(state.status_bar_screen_y, 0);
    }
}
