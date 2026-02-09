//! Per-frame context for efficient rendering and input handling.
//!
//! FrameContext computes derived state once per frame and shares it across
//! input handling and rendering, using indices instead of cloned lines
//! and lazy computation within the frame.

use std::cell::OnceCell;

use crate::diff::{DiffLine, LineSource};
use crate::ui::wrapping::{wrapped_line_height, ImageDimensions};

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
///
/// Note: This struct does NOT hold a reference to App to avoid borrow conflicts.
/// Methods that need access to app.lines take app as a parameter.
pub struct FrameContext {
    /// Eagerly computed: displayable items (cheap relative to cloning all lines)
    items: Vec<DisplayableItem>,

    /// Snapshot of app state at creation time
    viewport_height: usize,
    scroll_offset: usize,
    content_width: usize,

    /// Lazily computed: maximum valid scroll offset
    max_scroll: OnceCell<usize>,

    /// Lazily computed: wrapped line heights for each item
    wrap_heights: OnceCell<Vec<usize>>,

    /// Lazily computed: visible range (start, end) indices into items
    visible_range: OnceCell<(usize, usize)>,
}

impl FrameContext {
    /// Create a new frame context from the current app state
    pub fn new(app: &App) -> Self {
        let items = app.compute_displayable_items();
        Self::with_items(items, app)
    }

    /// Create a frame context with pre-computed items (avoids recomputing displayable items)
    pub fn with_items(items: Vec<DisplayableItem>, app: &App) -> Self {
        Self {
            items,
            viewport_height: app.viewport_height,
            scroll_offset: app.scroll_offset,
            content_width: app.content_width,
            max_scroll: OnceCell::new(),
            wrap_heights: OnceCell::new(),
            visible_range: OnceCell::new(),
        }
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
    pub fn line<'a>(&self, app: &'a App, display_idx: usize) -> &'a DiffLine {
        match self.items[display_idx] {
            DisplayableItem::Line(idx) => &app.lines[idx],
            DisplayableItem::Elided(_) => panic!("Called line() on Elided item at index {}", display_idx),
        }
    }

    /// Try to get line at display index (None if Elided)
    pub fn try_line<'a>(&self, app: &'a App, display_idx: usize) -> Option<&'a DiffLine> {
        match self.items[display_idx] {
            DisplayableItem::Line(idx) => Some(&app.lines[idx]),
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
    pub fn max_scroll(&self, app: &App) -> usize {
        *self.max_scroll.get_or_init(|| self.compute_max_scroll(app))
    }

    /// Get visible range as (start, end) indices into items (lazily computed)
    pub fn visible_range(&self, app: &App) -> (usize, usize) {
        *self.visible_range.get_or_init(|| self.compute_visible_range(app))
    }

    /// Iterate over all items
    pub fn iter_items(&self) -> impl Iterator<Item = &DisplayableItem> {
        self.items.iter()
    }

    /// Iterate over visible items (Lines and Elided markers)
    pub fn iter_visible_items<'a>(&'a self, app: &App) -> impl Iterator<Item = &'a DisplayableItem> {
        let (start, end) = self.visible_range(app);
        self.items[start..end].iter()
    }

    /// Find the next file header starting from the given display index
    pub fn find_next_file_header(&self, app: &App, start: usize) -> Option<usize> {
        for (i, item) in self.items.iter().enumerate().skip(start + 1) {
            if let DisplayableItem::Line(idx) = item
                && app.lines[*idx].source == LineSource::FileHeader
            {
                return Some(i);
            }
        }
        None
    }

    /// Find the previous file header before the given display index
    pub fn find_prev_file_header(&self, app: &App, current: usize) -> Option<usize> {
        if current == 0 {
            return None;
        }

        // Check if current is a file header
        let current_is_header = matches!(
            self.items.get(current),
            Some(DisplayableItem::Line(idx)) if app.lines[*idx].source == LineSource::FileHeader
        );

        let search_start = if current_is_header {
            current.saturating_sub(1)
        } else {
            current
        };

        for i in (0..=search_start).rev() {
            if let DisplayableItem::Line(idx) = self.items[i]
                && app.lines[idx].source == LineSource::FileHeader
            {
                return Some(i);
            }
        }
        None
    }

    /// Compute the maximum valid scroll offset
    fn compute_max_scroll(&self, app: &App) -> usize {
        if self.items.is_empty() {
            return 0;
        }

        let wrap_heights = self.get_wrap_heights(app);
        let total_rows: usize = wrap_heights.iter().sum();

        if total_rows <= self.viewport_height {
            return 0;
        }

        // Work backwards from the end to find how many items fit in viewport
        let mut rows_from_end = 0;
        let mut items_from_end = 0;

        for height in wrap_heights.iter().rev() {
            if rows_from_end + height > self.viewport_height {
                break;
            }
            rows_from_end += height;
            items_from_end += 1;
        }

        self.items.len().saturating_sub(items_from_end)
    }

    /// Compute the visible range for current scroll position
    fn compute_visible_range(&self, app: &App) -> (usize, usize) {
        if self.items.is_empty() {
            return (0, 0);
        }

        let start = self.scroll_offset.min(self.items.len());
        let wrap_heights = self.get_wrap_heights(app);

        let mut rows_used = 0;
        let mut end = start;

        for height in wrap_heights.iter().skip(start) {
            // Include items whose first row is visible, even if they extend beyond viewport
            if rows_used >= self.viewport_height && end > start {
                break;
            }
            rows_used += height;
            end += 1;
        }

        (start, end)
    }

    /// Get wrap heights for all items (lazily computed)
    fn get_wrap_heights(&self, app: &App) -> &[usize] {
        self.wrap_heights.get_or_init(|| self.compute_wrap_heights(app))
    }

    /// Compute the screen height for each item (accounting for line wrapping)
    fn compute_wrap_heights(&self, app: &App) -> Vec<usize> {
        self.items.iter().map(|item| {
            match item {
                DisplayableItem::Line(idx) => {
                    let line = &app.lines[*idx];
                    self.wrapped_line_height(line, app)
                }
                DisplayableItem::Elided(_) => 1, // Elided markers are always 1 row
            }
        }).collect()
    }

    /// Calculate how many screen rows a line will take when wrapped.
    /// Delegates to the shared `wrapped_line_height` function in `ui::wrapping`.
    fn wrapped_line_height(&self, line: &DiffLine, app: &App) -> usize {
        // Get image dimensions from cache if this is an image marker
        let image_dims: Option<ImageDimensions> = if line.is_image_marker() {
            line.file_path.as_ref().and_then(|path| {
                app.image_cache.peek(path).map(|state| {
                    let before = state
                        .before
                        .as_ref()
                        .map(|img| (img.original_width, img.original_height));
                    let after = state
                        .after
                        .as_ref()
                        .map(|img| (img.original_width, img.original_height));
                    (before, after)
                })
            })
        } else {
            None
        };

        wrapped_line_height(
            line,
            self.content_width,
            image_dims,
            app.panel_width,
            app.font_size,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{base_line, change_line, TestAppBuilder};

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
        let app = TestAppBuilder::new().with_lines(lines).build();
        let ctx = FrameContext::new(&app);

        assert_eq!(ctx.item_count(), 4);
        assert_eq!(ctx.line_count(), 4);

        // All items should be Line variants in Full mode
        for i in 0..4 {
            assert!(ctx.try_line(&app, i).is_some());
        }
    }

    #[test]
    fn test_frame_context_max_scroll_empty() {
        let app = TestAppBuilder::new().build();
        let ctx = FrameContext::new(&app);
        assert_eq!(ctx.max_scroll(&app), 0);
    }

    #[test]
    fn test_frame_context_max_scroll_fits_viewport() {
        let lines: Vec<_> = (0..5).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 10;
        let ctx = FrameContext::new(&app);
        assert_eq!(ctx.max_scroll(&app), 0);
    }

    #[test]
    fn test_frame_context_max_scroll_scrollable() {
        let lines: Vec<_> = (0..20).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 10;
        let ctx = FrameContext::new(&app);
        assert_eq!(ctx.max_scroll(&app), 10);
    }

    #[test]
    fn test_frame_context_visible_range() {
        let lines: Vec<_> = (0..20).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 5;
        app.scroll_offset = 3;
        let ctx = FrameContext::new(&app);

        let (start, end) = ctx.visible_range(&app);
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
        let app = TestAppBuilder::new().with_lines(lines).build();
        let ctx = FrameContext::new(&app);

        assert_eq!(ctx.find_next_file_header(&app, 0), Some(3));
        assert_eq!(ctx.find_next_file_header(&app, 3), None);
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
        let app = TestAppBuilder::new().with_lines(lines).build();
        let ctx = FrameContext::new(&app);

        assert_eq!(ctx.find_prev_file_header(&app, 4), Some(3));
        assert_eq!(ctx.find_prev_file_header(&app, 3), Some(0));
        assert_eq!(ctx.find_prev_file_header(&app, 0), None);
    }

    #[test]
    fn test_frame_context_iter_visible_items() {
        let lines: Vec<_> = (0..10).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 3;
        app.scroll_offset = 2;
        let ctx = FrameContext::new(&app);

        let visible: Vec<_> = ctx.iter_visible_items(&app).collect();
        assert_eq!(visible.len(), 3);
    }

    #[test]
    fn test_frame_context_uses_viewport_height_at_creation_time() {
        let lines: Vec<_> = (0..50).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();

        let ctx_with_default = FrameContext::new(&app);
        let (start, end) = ctx_with_default.visible_range(&app);
        assert_eq!(end - start, 10, "With default viewport_height=10, visible range should be 10");

        app.viewport_height = 40;
        let ctx_with_correct = FrameContext::new(&app);
        let (start, end) = ctx_with_correct.visible_range(&app);
        assert_eq!(end - start, 40, "With viewport_height=40, visible range should be 40");
    }

    #[test]
    fn test_visible_range_accounts_for_wrapped_lines() {
        let mut lines: Vec<_> = (0..10).map(|i| base_line(&format!("short{}", i))).collect();
        lines.push(base_line(&"x".repeat(200)));

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 5;
        app.content_width = 50;
        app.scroll_offset = 8;

        let ctx = FrameContext::new(&app);
        let (start, end) = ctx.visible_range(&app);

        assert_eq!(start, 8);
        assert!(end <= 11, "visible range should not exceed total items");

        // Visible range includes items whose first row is visible, even if they extend beyond.
        // This allows partial rendering of tall items at the viewport bottom.
        let wrap_heights = ctx.get_wrap_heights(&app);
        let rows_before_last: usize = wrap_heights[start..end.saturating_sub(1)].iter().sum();
        assert!(rows_before_last < app.viewport_height,
            "rows before last item ({}) should start within viewport ({})", rows_before_last, app.viewport_height);
    }

    #[test]
    fn test_visible_range_includes_at_least_one_item_when_taller_than_viewport() {
        let lines = vec![base_line(&"x".repeat(500))];

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 3;
        app.content_width = 50;
        app.scroll_offset = 0;

        let ctx = FrameContext::new(&app);
        let (start, end) = ctx.visible_range(&app);

        assert_eq!(start, 0);
        assert_eq!(end, 1, "should include at least one item even if taller than viewport");
    }

    #[test]
    fn test_visible_range_includes_partial_items_at_viewport_bottom() {
        // Create lines where the last visible item will only partially fit
        // 5 short lines (1 row each) + 1 tall line (4 rows)
        let mut lines: Vec<_> = (0..5).map(|i| base_line(&format!("short{}", i))).collect();
        lines.push(base_line(&"x".repeat(200))); // ~4 rows at width 50

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 6; // Can fit 5 short + 1 row of tall
        app.content_width = 50;
        app.scroll_offset = 0;

        let ctx = FrameContext::new(&app);
        let (start, end) = ctx.visible_range(&app);

        // Should include all 6 items: the tall item's first row starts within viewport
        assert_eq!(start, 0);
        assert_eq!(end, 6, "should include partial item at viewport bottom");

        // The total rows exceed viewport, proving we include a partial item
        let wrap_heights = ctx.get_wrap_heights(&app);
        let total_rows: usize = wrap_heights[start..end].iter().sum();
        assert!(total_rows > app.viewport_height,
            "total rows ({}) should exceed viewport ({}) due to partial item",
            total_rows, app.viewport_height);
    }

    #[test]
    fn test_visible_range_includes_partial_image_at_viewport_bottom() {
        use crate::image_diff::{CachedImage, ImageDiffState};
        use image::DynamicImage;

        // Create content: 2 short text lines + 1 image marker (16 rows tall)
        let lines = vec![
            DiffLine::file_header("test.png"),
            base_line("short line 1"),
            DiffLine::image_marker("test.png"),
        ];

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 5; // Only room for header (1) + text (1) + 3 rows of image
        app.content_width = 80;
        app.panel_width = 100;

        // Add image to cache - 192x192 results in ~16 row height
        let cached_image = CachedImage {
            display_image: DynamicImage::new_rgb8(192, 192),
            original_width: 192,
            original_height: 192,
            file_size: 1024,
            format_name: "PNG".to_string(),
            protocol: None,
        };
        app.image_cache.insert(
            "test.png".to_string(),
            ImageDiffState {
                before: Some(cached_image),
                after: None,
            },
        );

        let ctx = FrameContext::new(&app);
        let wrap_heights = ctx.get_wrap_heights(&app);

        // Verify the image marker has significant height (>5 rows)
        let image_height = wrap_heights[2];
        assert!(
            image_height > app.viewport_height,
            "image height ({}) should exceed viewport ({}) for this test to be meaningful",
            image_height, app.viewport_height
        );

        let (start, end) = ctx.visible_range(&app);

        // Should include all 3 items: header + text line + image marker
        // The image's first row starts at row 2 (within viewport of 5)
        assert_eq!(start, 0);
        assert_eq!(end, 3, "should include partial image marker at viewport bottom");

        // Total rows exceed viewport, proving partial image is included
        let total_rows: usize = wrap_heights[start..end].iter().sum();
        assert!(
            total_rows > app.viewport_height,
            "total rows ({}) should exceed viewport ({}) due to partial image",
            total_rows, app.viewport_height
        );
    }

    #[test]
    fn test_wrapped_line_height_accounts_for_mixed_inline_changes() {
        use crate::diff::InlineSpan;

        let mut line = base_line(&"x".repeat(100));
        line.old_content = Some("y".repeat(80));
        line.inline_spans = vec![
            InlineSpan { text: "deleted".to_string(), source: Some(LineSource::Unstaged), is_deletion: true },
            InlineSpan { text: "inserted".to_string(), source: Some(LineSource::Unstaged), is_deletion: false },
        ];

        let mut app = TestAppBuilder::new().with_lines(vec![line]).build();
        app.content_width = 50;

        let ctx = FrameContext::new(&app);
        let wrap_heights = ctx.get_wrap_heights(&app);

        assert_eq!(wrap_heights[0], 4, "mixed change: 80/50=2 del rows + 100/50=2 ins rows = 4 total");
    }

    #[test]
    fn test_initial_visible_range_fills_viewport_with_realistic_content_width() {
        // This test verifies that on initial render (scroll_offset=0), the visible_range
        // fills the viewport. A regression was caused when content_width was incorrectly
        // set to a small default value, causing wrap_height miscalculation.
        //
        // With content_width=50 (narrow), 100-char lines wrap to 2 rows,
        // so 10 such lines would consume 20 viewport rows.
        // With content_width=150 (wide), same lines fit in 1 row each,
        // so we should see 20 lines visible.

        // Create lines that are ~100 chars - long enough to wrap with narrow width
        let lines: Vec<_> = (0..30)
            .map(|i| base_line(&format!(
                "line {:02} with lots of extra content padding to make this line about one hundred characters long xx",
                i
            )))
            .collect();

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 20;
        app.scroll_offset = 0;

        // With narrow content_width (50), 100-char lines wrap to 2 rows each
        app.content_width = 50;
        let ctx_narrow = FrameContext::new(&app);
        let (start_narrow, end_narrow) = ctx_narrow.visible_range(&app);
        let visible_narrow = end_narrow - start_narrow;

        // With wide content_width (150), 100-char lines fit in 1 row each
        app.content_width = 150;
        let ctx_wide = FrameContext::new(&app);
        let (start_wide, end_wide) = ctx_wide.visible_range(&app);
        let visible_wide = end_wide - start_wide;

        // Wide content should show viewport_height (20) lines since each is 1 row
        assert_eq!(
            visible_wide, 20,
            "With wide content_width, should see viewport_height (20) lines"
        );

        // Narrow content should show ~10 lines (each takes 2 rows = 20 rows total)
        assert!(
            visible_narrow < visible_wide,
            "Narrow content_width ({} visible) should show fewer lines than wide ({} visible) due to wrapping",
            visible_narrow, visible_wide
        );
        assert!(
            visible_narrow <= 10,
            "With 50-char width and ~100-char lines, should see ~10 lines (2 rows each), got {}",
            visible_narrow
        );
    }

    #[test]
    fn test_initial_render_content_width_must_be_set_before_visible_range() {
        // This test catches a regression where content_width was left at its default
        // value (80) during the first render, causing incorrect wrap height calculations.
        //
        // The bug: On initial render, FrameContext was created with content_width=80,
        // but the actual terminal might be wider (e.g., 150). This caused lines to be
        // calculated as wrapping when they wouldn't, resulting in fewer visible items
        // and empty space at the bottom of the screen.
        //
        // This test verifies that using the DEFAULT content_width produces a different
        // (incorrect) visible_range than using the ACTUAL content_width. If this test
        // fails, it means the fix is working correctly.

        // Create lines that wrap with content_width=80 but not with content_width=150
        // Line length ~120 chars: wraps to 2 rows with width=80, 1 row with width=150
        let lines: Vec<_> = (0..30)
            .map(|i| base_line(&format!(
                "line {:02} - this line is intentionally long enough to wrap at 80 chars but fit on one line at 150 chars padding",
                i
            )))
            .collect();

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 20;
        app.scroll_offset = 0;

        // Simulate BUG: content_width left at default (80 from TestAppBuilder)
        // With ~120-char lines and width=80: each line wraps to 2 rows
        // 20 viewport rows / 2 rows per line = ~10 lines visible
        let default_width = app.content_width; // Should be 80 from TestAppBuilder
        assert_eq!(default_width, 80, "Test assumes default content_width is 80");

        let ctx_with_default = FrameContext::new(&app);
        let (_, end_default) = ctx_with_default.visible_range(&app);
        let visible_with_default = end_default;

        // Simulate FIX: content_width set to actual terminal width (150)
        // With ~120-char lines and width=150: each line fits in 1 row
        // 20 viewport rows / 1 row per line = 20 lines visible
        app.content_width = 150;
        let ctx_with_actual = FrameContext::new(&app);
        let (_, end_actual) = ctx_with_actual.visible_range(&app);
        let visible_with_actual = end_actual;

        // The default (buggy) calculation should show fewer items
        assert!(
            visible_with_default < visible_with_actual,
            "Bug: default content_width ({}) should show fewer items ({}) than actual width ({}) which shows {}",
            default_width, visible_with_default, 150, visible_with_actual
        );

        // With actual width, should fill viewport with 20 lines (1 row each)
        assert_eq!(
            visible_with_actual, 20,
            "With actual content_width=150, viewport should be filled with 20 lines"
        );

        // With default width, should only show ~10-12 lines due to wrapping
        assert!(
            visible_with_default <= 12,
            "With default content_width=80, should only see ~10-12 lines due to wrap, got {}",
            visible_with_default
        );
    }

    #[test]
    fn test_frame_context_uses_image_cache_for_height() {
        use crate::image_diff::{CachedImage, ImageDiffState};
        use image::DynamicImage;

        // Create 3 image files to match the bug scenario
        let lines = vec![
            DiffLine::file_header("image1.png"),
            DiffLine::image_marker("image1.png"),
            DiffLine::file_header("image2.png"),
            DiffLine::image_marker("image2.png"),
            DiffLine::file_header("image3.png"),
            DiffLine::image_marker("image3.png"),
        ];

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 60;  // Large enough for all items
        app.content_width = 80;
        app.panel_width = 100;

        // Add image data to cache for all 3 images (192x192)
        for i in 1..=3 {
            let cached_image = CachedImage {
                display_image: DynamicImage::new_rgb8(192, 192),
                original_width: 192,
                original_height: 192,
                file_size: 1024,
                format_name: "PNG".to_string(),
                protocol: None,
            };
            app.image_cache.insert(
                format!("image{}.png", i),
                ImageDiffState {
                    before: Some(cached_image),
                    after: None,
                },
            );
        }

        let ctx = FrameContext::new(&app);

        // Get the wrap heights via FrameContext
        let wrap_heights = ctx.get_wrap_heights(&app);

        // File headers should be 1 row each
        assert_eq!(wrap_heights[0], 1, "File header should be 1 row");
        assert_eq!(wrap_heights[2], 1, "File header should be 1 row");
        assert_eq!(wrap_heights[4], 1, "File header should be 1 row");

        // Image markers should use cache dimensions, not fallback
        // With 192x192 image, 8x16 font, panel_width=100:
        // Available width per panel: (100-4)/2 = 48 cells
        // Image in cells: 192/8 = 24 cells wide, 192/16 = 12 cells tall
        // No scaling needed (24 < 48), height = 12 + 4 (borders) = 16
        let expected_image_height = crate::ui::image_view::calculate_image_height_for_images(
            Some((192, 192)),
            None,
            100,
            (8, 16),
        ) as usize;

        assert_eq!(
            wrap_heights[1], expected_image_height,
            "Image 1 should use cache dimensions, not fallback"
        );
        assert_eq!(
            wrap_heights[3], expected_image_height,
            "Image 2 should use cache dimensions, not fallback"
        );
        assert_eq!(
            wrap_heights[5], expected_image_height,
            "Image 3 should use cache dimensions, not fallback"
        );

        // Verify visible range includes all 3 files at scroll_offset=0
        // Total rows: 3 headers (3) + 3 images (3 * 16 = 48) = 51 rows
        // With viewport_height=60, all 6 items should be visible
        let (start, end) = ctx.visible_range(&app);
        assert_eq!(start, 0);
        assert_eq!(
            end, 6,
            "All 6 items (3 headers + 3 images) should be visible with 60-row viewport"
        );
    }
}
