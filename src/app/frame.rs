//! Per-frame context for efficient rendering and input handling.
//!
//! FrameContext computes derived state once per frame and shares it across
//! input handling and rendering, using indices instead of cloned lines
//! and lazy computation within the frame.

use std::cell::OnceCell;

use crate::diff::{DiffLine, LineSource};

use super::App;

/// Represents an item in the displayable list.
/// Uses indices instead of cloning entire DiffLines for efficiency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayableItem {
    /// Index into app.lines
    Line(usize),
    /// Count of elided/hidden lines (Context mode only)
    Elided(usize),
}

impl DisplayableItem {
    /// Get the line index if this is a Line variant
    pub fn as_line_index(&self) -> Option<usize> {
        match self {
            DisplayableItem::Line(idx) => Some(*idx),
            DisplayableItem::Elided(_) => None,
        }
    }

    /// Check if this is an elided marker
    pub fn is_elided(&self) -> bool {
        matches!(self, DisplayableItem::Elided(_))
    }

    /// Get the elided count if this is an Elided variant
    pub fn elided_count(&self) -> Option<usize> {
        match self {
            DisplayableItem::Elided(count) => Some(*count),
            DisplayableItem::Line(_) => None,
        }
    }
}

/// Per-frame computed context shared across input handling and rendering.
///
/// Created fresh at the start of each render cycle. Uses lazy computation
/// for expensive operations that may not be needed every frame.
pub struct FrameContext<'a> {
    app: &'a App,

    /// Eagerly computed: displayable items (cheap relative to cloning all lines)
    items: Vec<DisplayableItem>,

    /// Lazily computed: maximum valid scroll offset
    max_scroll: OnceCell<usize>,

    /// Lazily computed: wrapped line heights for each item
    wrap_heights: OnceCell<Vec<usize>>,

    /// Lazily computed: visible range (start, end) indices into items
    visible_range: OnceCell<(usize, usize)>,
}

impl<'a> FrameContext<'a> {
    /// Create a new frame context from the current app state
    pub fn new(app: &'a App) -> Self {
        let items = app.compute_displayable_items();
        Self {
            app,
            items,
            max_scroll: OnceCell::new(),
            wrap_heights: OnceCell::new(),
            visible_range: OnceCell::new(),
        }
    }

    /// Get a reference to the underlying app
    pub fn app(&self) -> &App {
        self.app
    }

    /// Get item at display index
    pub fn item(&self, display_idx: usize) -> &DisplayableItem {
        &self.items[display_idx]
    }

    /// Get all items
    pub fn items(&self) -> &[DisplayableItem] {
        &self.items
    }

    /// Get line at display index (panics if Elided)
    pub fn line(&self, display_idx: usize) -> &DiffLine {
        match self.items[display_idx] {
            DisplayableItem::Line(idx) => &self.app.lines[idx],
            DisplayableItem::Elided(_) => panic!("Called line() on Elided item at index {}", display_idx),
        }
    }

    /// Try to get line at display index (None if Elided)
    pub fn try_line(&self, display_idx: usize) -> Option<&DiffLine> {
        match self.items[display_idx] {
            DisplayableItem::Line(idx) => Some(&self.app.lines[idx]),
            DisplayableItem::Elided(_) => None,
        }
    }

    /// Get the original line index for a display index (None if Elided)
    pub fn original_index(&self, display_idx: usize) -> Option<usize> {
        self.items[display_idx].as_line_index()
    }

    /// Total count of displayable items (including Elided markers)
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    /// Count of actual lines (excludes Elided markers)
    pub fn line_count(&self) -> usize {
        self.items.iter().filter(|i| matches!(i, DisplayableItem::Line(_))).count()
    }

    /// Get maximum valid scroll offset (lazily computed)
    pub fn max_scroll(&self) -> usize {
        *self.max_scroll.get_or_init(|| self.compute_max_scroll())
    }

    /// Get visible range as (start, end) indices into items (lazily computed)
    pub fn visible_range(&self) -> (usize, usize) {
        *self.visible_range.get_or_init(|| self.compute_visible_range())
    }

    /// Iterate over all items
    pub fn iter_items(&self) -> impl Iterator<Item = &DisplayableItem> {
        self.items.iter()
    }

    /// Iterate over visible items (Lines and Elided markers)
    pub fn iter_visible_items(&self) -> impl Iterator<Item = &DisplayableItem> {
        let (start, end) = self.visible_range();
        self.items[start..end].iter()
    }

    /// Iterate over visible lines only (skips Elided markers)
    pub fn iter_visible_lines(&self) -> impl Iterator<Item = &DiffLine> {
        self.iter_visible_items().filter_map(|item| {
            match item {
                DisplayableItem::Line(idx) => Some(&self.app.lines[*idx]),
                DisplayableItem::Elided(_) => None,
            }
        })
    }

    /// Find the next file header starting from the given display index
    pub fn find_next_file_header(&self, start: usize) -> Option<usize> {
        for (i, item) in self.items.iter().enumerate().skip(start + 1) {
            if let DisplayableItem::Line(idx) = item {
                if self.app.lines[*idx].source == LineSource::FileHeader {
                    return Some(i);
                }
            }
        }
        None
    }

    /// Find the previous file header before the given display index
    pub fn find_prev_file_header(&self, current: usize) -> Option<usize> {
        if current == 0 {
            return None;
        }

        // Check if current is a file header
        let current_is_header = matches!(
            self.items.get(current),
            Some(DisplayableItem::Line(idx)) if self.app.lines[*idx].source == LineSource::FileHeader
        );

        let search_start = if current_is_header {
            current.saturating_sub(1)
        } else {
            current
        };

        for i in (0..=search_start).rev() {
            if let DisplayableItem::Line(idx) = self.items[i] {
                if self.app.lines[idx].source == LineSource::FileHeader {
                    return Some(i);
                }
            }
        }
        None
    }

    /// Compute the maximum valid scroll offset
    fn compute_max_scroll(&self) -> usize {
        if self.items.is_empty() {
            return 0;
        }

        let wrap_heights = self.get_wrap_heights();
        let total_rows: usize = wrap_heights.iter().sum();

        if total_rows <= self.app.viewport_height {
            return 0;
        }

        // Work backwards from the end to find how many items fit in viewport
        let mut rows_from_end = 0;
        let mut items_from_end = 0;

        for height in wrap_heights.iter().rev() {
            if rows_from_end + height > self.app.viewport_height {
                break;
            }
            rows_from_end += height;
            items_from_end += 1;
        }

        self.items.len().saturating_sub(items_from_end)
    }

    /// Compute the visible range for current scroll position
    fn compute_visible_range(&self) -> (usize, usize) {
        if self.items.is_empty() {
            return (0, 0);
        }

        let start = self.app.scroll_offset.min(self.items.len());
        let wrap_heights = self.get_wrap_heights();

        // Calculate how many items fit in viewport, accounting for wrapping
        let mut screen_rows_used = 0;
        let mut end = start;

        while end < self.items.len() && screen_rows_used < self.app.viewport_height {
            screen_rows_used += wrap_heights[end];
            end += 1;
        }

        (start, end)
    }

    /// Get wrap heights for all items (lazily computed)
    fn get_wrap_heights(&self) -> &[usize] {
        self.wrap_heights.get_or_init(|| self.compute_wrap_heights())
    }

    /// Compute the screen height for each item (accounting for line wrapping)
    fn compute_wrap_heights(&self) -> Vec<usize> {
        self.items.iter().map(|item| {
            match item {
                DisplayableItem::Line(idx) => {
                    let line = &self.app.lines[*idx];
                    self.wrapped_line_height(line)
                }
                DisplayableItem::Elided(_) => 1, // Elided markers are always 1 row
            }
        }).collect()
    }

    /// Calculate how many screen rows a line will take when wrapped
    fn wrapped_line_height(&self, line: &DiffLine) -> usize {
        if self.app.content_width == 0 {
            return 1;
        }
        let content_len = line.content.len();
        if content_len <= self.app.content_width {
            1
        } else {
            (content_len + self.app.content_width - 1) / self.app.content_width
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ViewMode;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn create_test_app(lines: Vec<DiffLine>) -> App {
        App {
            repo_path: PathBuf::from("/tmp/test"),
            base_branch: "main".to_string(),
            merge_base: "abc123".to_string(),
            current_branch: Some("feature".to_string()),
            files: Vec::new(),
            lines,
            scroll_offset: 0,
            viewport_height: 10,
            error: None,
            show_help: false,
            view_mode: ViewMode::Full,
            selection: None,
            content_offset: (1, 1),
            line_num_width: 0,
            content_width: 80,
            conflict_warning: None,
            row_map: Vec::new(),
            collapsed_files: HashSet::new(),
            manually_toggled: HashSet::new(),
        }
    }

    fn base_line(content: &str) -> DiffLine {
        DiffLine::new(LineSource::Base, content.to_string(), ' ', None)
    }

    fn change_line(content: &str) -> DiffLine {
        DiffLine::new(LineSource::Committed, content.to_string(), '+', None)
    }

    #[test]
    fn test_displayable_item_as_line_index() {
        assert_eq!(DisplayableItem::Line(5).as_line_index(), Some(5));
        assert_eq!(DisplayableItem::Elided(10).as_line_index(), None);
    }

    #[test]
    fn test_displayable_item_is_elided() {
        assert!(!DisplayableItem::Line(5).is_elided());
        assert!(DisplayableItem::Elided(10).is_elided());
    }

    #[test]
    fn test_frame_context_full_mode_all_lines() {
        let lines = vec![
            DiffLine::file_header("test.rs"),
            base_line("line1"),
            change_line("line2"),
            base_line("line3"),
        ];
        let app = create_test_app(lines);
        let ctx = FrameContext::new(&app);

        assert_eq!(ctx.item_count(), 4);
        assert_eq!(ctx.line_count(), 4);

        // All items should be Line variants in Full mode
        for i in 0..4 {
            assert!(ctx.try_line(i).is_some());
        }
    }

    #[test]
    fn test_frame_context_max_scroll_empty() {
        let app = create_test_app(vec![]);
        let ctx = FrameContext::new(&app);
        assert_eq!(ctx.max_scroll(), 0);
    }

    #[test]
    fn test_frame_context_max_scroll_fits_viewport() {
        let lines: Vec<_> = (0..5).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = create_test_app(lines);
        app.viewport_height = 10;
        let ctx = FrameContext::new(&app);
        assert_eq!(ctx.max_scroll(), 0);
    }

    #[test]
    fn test_frame_context_max_scroll_scrollable() {
        let lines: Vec<_> = (0..20).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = create_test_app(lines);
        app.viewport_height = 10;
        let ctx = FrameContext::new(&app);
        assert_eq!(ctx.max_scroll(), 10);
    }

    #[test]
    fn test_frame_context_visible_range() {
        let lines: Vec<_> = (0..20).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = create_test_app(lines);
        app.viewport_height = 5;
        app.scroll_offset = 3;
        let ctx = FrameContext::new(&app);

        let (start, end) = ctx.visible_range();
        assert_eq!(start, 3);
        assert_eq!(end, 8);
    }

    #[test]
    fn test_frame_context_find_next_file_header() {
        let lines = vec![
            DiffLine::file_header("file1.rs"),
            base_line("line1"),
            base_line("line2"),
            DiffLine::file_header("file2.rs"),
            base_line("line3"),
        ];
        let app = create_test_app(lines);
        let ctx = FrameContext::new(&app);

        assert_eq!(ctx.find_next_file_header(0), Some(3));
        assert_eq!(ctx.find_next_file_header(3), None);
    }

    #[test]
    fn test_frame_context_find_prev_file_header() {
        let lines = vec![
            DiffLine::file_header("file1.rs"),
            base_line("line1"),
            base_line("line2"),
            DiffLine::file_header("file2.rs"),
            base_line("line3"),
        ];
        let app = create_test_app(lines);
        let ctx = FrameContext::new(&app);

        assert_eq!(ctx.find_prev_file_header(4), Some(3));
        assert_eq!(ctx.find_prev_file_header(3), Some(0));
        assert_eq!(ctx.find_prev_file_header(0), None);
    }

    #[test]
    fn test_frame_context_iter_visible_items() {
        let lines: Vec<_> = (0..10).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = create_test_app(lines);
        app.viewport_height = 3;
        app.scroll_offset = 2;
        let ctx = FrameContext::new(&app);

        let visible: Vec<_> = ctx.iter_visible_items().collect();
        assert_eq!(visible.len(), 3);
    }
}
