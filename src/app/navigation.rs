use crate::diff::{DiffLine, LineSource};
use crate::ui::wrapping::{wrapped_line_height, ImageDimensions};

use super::{App, DisplayableItem, FrameContext};

impl App {
    pub fn scroll_up(&mut self, n: usize) {
        let old_offset = self.scroll_offset;
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        if self.scroll_offset != old_offset {
            self.needs_inline_spans = true;
            self.clear_selection();
        }
    }

    pub fn scroll_down(&mut self, n: usize) {
        let old_offset = self.scroll_offset;
        self.scroll_offset = self.scroll_offset.saturating_add(n);
        self.clamp_scroll();
        if self.scroll_offset != old_offset {
            self.needs_inline_spans = true;
            self.clear_selection();
        }
    }

    pub fn next_file(&mut self) {
        let items = self.compute_displayable_items();
        if items.is_empty() {
            return;
        }

        for (i, item) in items.iter().enumerate().skip(self.scroll_offset + 1) {
            if let DisplayableItem::Line(idx) = item
                && self.lines[*idx].source == LineSource::FileHeader
            {
                self.scroll_offset = i;
                self.needs_inline_spans = true;
                self.clear_selection();
                return;
            }
        }
    }

    /// Navigate to next file using pre-computed FrameContext
    pub fn next_file_with_frame(&mut self, ctx: &FrameContext) {
        if ctx.item_count() == 0 {
            return;
        }

        if let Some(pos) = ctx.find_next_file_header(self, self.scroll_offset) {
            self.scroll_offset = pos;
            self.needs_inline_spans = true;
            self.clear_selection();
        }
    }

    pub fn prev_file(&mut self) {
        let items = self.compute_displayable_items();
        if items.is_empty() || self.scroll_offset == 0 {
            return;
        }

        let current_is_header = match items.get(self.scroll_offset) {
            Some(DisplayableItem::Line(idx)) => self.lines[*idx].source == LineSource::FileHeader,
            _ => false,
        };

        let search_start = if current_is_header {
            self.scroll_offset.saturating_sub(1)
        } else {
            self.scroll_offset
        };

        for i in (0..=search_start).rev() {
            if let DisplayableItem::Line(idx) = items[i]
                && self.lines[idx].source == LineSource::FileHeader
            {
                self.scroll_offset = i;
                self.needs_inline_spans = true;
                self.clear_selection();
                return;
            }
        }
    }

    /// Navigate to previous file using pre-computed FrameContext
    pub fn prev_file_with_frame(&mut self, ctx: &FrameContext) {
        if ctx.item_count() == 0 || self.scroll_offset == 0 {
            return;
        }

        if let Some(pos) = ctx.find_prev_file_header(self, self.scroll_offset) {
            self.scroll_offset = pos;
            self.needs_inline_spans = true;
            self.clear_selection();
        }
    }

    pub fn page_up(&mut self) {
        let page_size = self.viewport_height.saturating_sub(2);
        self.scroll_up(page_size);
    }

    pub fn page_down(&mut self) {
        let page_size = self.viewport_height.saturating_sub(2);
        self.scroll_down(page_size);
    }

    pub fn go_to_top(&mut self) {
        if self.scroll_offset != 0 {
            self.scroll_offset = 0;
            self.needs_inline_spans = true;
            self.clear_selection();
        }
    }

    pub fn go_to_bottom(&mut self) {
        let old_offset = self.scroll_offset;
        let items = self.compute_displayable_items();
        self.scroll_offset = self.max_scroll_for_items(&items);
        if self.scroll_offset != old_offset {
            self.needs_inline_spans = true;
            self.clear_selection();
        }
    }

    /// Go to bottom using pre-computed FrameContext
    pub fn go_to_bottom_with_frame(&mut self, ctx: &FrameContext) {
        let old_offset = self.scroll_offset;
        self.scroll_offset = ctx.max_scroll(self);
        if self.scroll_offset != old_offset {
            self.needs_inline_spans = true;
            self.clear_selection();
        }
    }

    /// Compute max scroll offset from displayable items (no cloning)
    fn max_scroll_for_items(&self, items: &[DisplayableItem]) -> usize {
        if items.is_empty() {
            return 0;
        }

        let total_rows: usize = items
            .iter()
            .map(|item| match item {
                DisplayableItem::Line(idx) => self.wrapped_line_height(&self.lines[*idx]),
                DisplayableItem::Elided(_) => 1,
            })
            .sum();

        if total_rows <= self.viewport_height {
            return 0;
        }

        // Work backwards to find how many items fit in viewport
        let mut rows_from_end = 0;
        let mut items_from_end = 0;

        for item in items.iter().rev() {
            let height = match item {
                DisplayableItem::Line(idx) => self.wrapped_line_height(&self.lines[*idx]),
                DisplayableItem::Elided(_) => 1,
            };
            if rows_from_end + height > self.viewport_height {
                break;
            }
            rows_from_end += height;
            items_from_end += 1;
        }

        items.len().saturating_sub(items_from_end)
    }

    /// Set viewport height (called during rendering)
    pub fn set_viewport_height(&mut self, height: usize) {
        if self.viewport_height != height {
            self.viewport_height = height;
            self.needs_inline_spans = true;
            self.clamp_scroll();
        }
    }

    /// Clamp scroll offset to valid range
    pub(super) fn clamp_scroll(&mut self) {
        let items = self.compute_displayable_items();
        let max_scroll = self.max_scroll_for_items(&items);
        self.scroll_offset = self.scroll_offset.min(max_scroll);
    }

    /// Clamp scroll offset using pre-computed FrameContext
    pub fn clamp_scroll_with_frame(&mut self, ctx: &FrameContext) {
        let max_scroll = ctx.max_scroll(self);
        self.scroll_offset = self.scroll_offset.min(max_scroll);
    }

    /// Scroll down using pre-computed FrameContext
    pub fn scroll_down_with_frame(&mut self, n: usize, ctx: &FrameContext) {
        let old_offset = self.scroll_offset;
        self.scroll_offset = self.scroll_offset.saturating_add(n);
        self.clamp_scroll_with_frame(ctx);
        if self.scroll_offset != old_offset {
            self.needs_inline_spans = true;
            self.clear_selection();
        }
    }

    /// Calculate how many screen rows a line will take when wrapped.
    /// Delegates to the shared `wrapped_line_height` function in `ui::wrapping`.
    pub(super) fn wrapped_line_height(&self, line: &DiffLine) -> usize {
        // Get image dimensions from cache if this is an image marker
        let image_dims: Option<ImageDimensions> = if line.is_image_marker() {
            line.file_path.as_ref().and_then(|path| {
                self.image_cache.peek(path).map(|state| {
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
            self.panel_width,
            self.font_size,
        )
    }

    pub fn scroll_percentage(&self) -> u16 {
        let items = self.compute_displayable_items();
        let item_count = items.len();
        if item_count == 0 || item_count <= self.viewport_height {
            100
        } else {
            let max_scroll = self.max_scroll_for_items(&items);
            if max_scroll == 0 {
                return 100;
            }
            let pct = ((self.scroll_offset as f64 / max_scroll as f64) * 100.0) as u16;
            pct.min(100)
        }
    }

    /// Compute scroll percentage using pre-computed FrameContext
    pub fn scroll_percentage_with_frame(&self, ctx: &FrameContext) -> u16 {
        let item_count = ctx.item_count();
        if item_count == 0 || item_count <= self.viewport_height {
            100
        } else {
            let max_scroll = ctx.max_scroll(self);
            if max_scroll == 0 {
                return 100;
            }
            let pct = ((self.scroll_offset as f64 / max_scroll as f64) * 100.0) as u16;
            pct.min(100)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{base_line, TestAppBuilder};

    #[test]
    fn test_next_file_jumps_to_header() {
        let lines = vec![
            DiffLine::file_header("file1.rs"),
            base_line("line1"),
            base_line("line2"),
            DiffLine::file_header("file2.rs"),
            base_line("line3"),
        ];
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.scroll_offset = 0;

        app.next_file();

        assert_eq!(app.scroll_offset, 3);
    }

    #[test]
    fn test_next_file_from_middle_of_file() {
        let lines = vec![
            DiffLine::file_header("file1.rs"),
            base_line("line1"),
            base_line("line2"),
            DiffLine::file_header("file2.rs"),
            base_line("line3"),
        ];
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.scroll_offset = 1;

        app.next_file();

        assert_eq!(app.scroll_offset, 3);
    }

    #[test]
    fn test_next_file_no_more_files() {
        let lines = vec![
            DiffLine::file_header("file1.rs"),
            base_line("line1"),
        ];
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.scroll_offset = 0;

        app.next_file();

        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_prev_file_jumps_back() {
        let lines = vec![
            DiffLine::file_header("file1.rs"),
            base_line("line1"),
            base_line("line2"),
            DiffLine::file_header("file2.rs"),
            base_line("line3"),
        ];
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.scroll_offset = 4;

        app.prev_file();

        assert_eq!(app.scroll_offset, 3);
    }

    #[test]
    fn test_prev_file_from_header_goes_to_previous() {
        let lines = vec![
            DiffLine::file_header("file1.rs"),
            base_line("line1"),
            base_line("line2"),
            DiffLine::file_header("file2.rs"),
            base_line("line3"),
        ];
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.scroll_offset = 3;

        app.prev_file();

        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_prev_file_at_first_file() {
        let lines = vec![
            DiffLine::file_header("file1.rs"),
            base_line("line1"),
        ];
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.scroll_offset = 0;

        app.prev_file();

        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_next_file_empty_lines() {
        let mut app = TestAppBuilder::new().build();
        app.next_file();
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_prev_file_empty_lines() {
        let mut app = TestAppBuilder::new().build();
        app.prev_file();
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_scroll_percentage_at_top_returns_zero() {
        let lines: Vec<DiffLine> = (0..50).map(|i| base_line(&format!("line {}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 10;
        app.scroll_offset = 0;

        assert_eq!(app.scroll_percentage(), 0);
    }

    #[test]
    fn test_scroll_percentage_at_bottom_returns_100() {
        let lines: Vec<DiffLine> = (0..50).map(|i| base_line(&format!("line {}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 10;
        app.scroll_offset = 40; // 50 lines - 10 viewport = 40 max scroll

        assert_eq!(app.scroll_percentage(), 100);
    }

    #[test]
    fn test_scroll_percentage_capped_at_100() {
        let lines: Vec<DiffLine> = (0..50).map(|i| base_line(&format!("line {}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 10;
        app.scroll_offset = 100; // Beyond max scroll

        assert!(app.scroll_percentage() <= 100);
    }

    #[test]
    fn test_scroll_percentage_empty_content_returns_100() {
        let app = TestAppBuilder::new().build();

        assert_eq!(app.scroll_percentage(), 100);
    }

    #[test]
    fn test_scroll_percentage_content_fits_viewport_returns_100() {
        let lines: Vec<DiffLine> = (0..5).map(|i| base_line(&format!("line {}", i))).collect();
        let app = TestAppBuilder::new().with_lines(lines).build();
        // viewport_height defaults to 10, so 5 lines fit

        assert_eq!(app.scroll_percentage(), 100);
    }

    #[test]
    fn test_max_scroll_for_items() {
        let lines: Vec<DiffLine> = (0..20)
            .map(|i| base_line(&format!("line {}", i)))
            .collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 10;

        let items = app.compute_displayable_items();
        let max_scroll = app.max_scroll_for_items(&items);

        assert_eq!(max_scroll, 10); // 20 lines - 10 viewport = 10
    }

    #[test]
    fn test_clamp_scroll() {
        let lines: Vec<DiffLine> = (0..20)
            .map(|i| base_line(&format!("line {}", i)))
            .collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 10;
        app.scroll_offset = 100; // Way past end

        app.clamp_scroll();

        assert_eq!(app.scroll_offset, 10); // Clamped to max
    }

    #[test]
    fn test_wrapped_line_height_for_image_marker() {
        use crate::image_diff::{CachedImage, ImageDiffState};
        use image::DynamicImage;

        let lines = vec![
            DiffLine::file_header("test.png"),
            DiffLine::image_marker("test.png"),
        ];
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 40;
        app.content_width = 80;
        app.panel_width = 100;

        // Add image data to cache with known dimensions
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

        // Image markers should have dynamic height based on actual image dimensions
        let image_line = &app.lines[1];
        let height = app.wrapped_line_height(image_line);

        // Height is calculated from actual image dimensions, not viewport percentage
        let expected = crate::ui::image_view::calculate_image_height_for_images(
            Some((192, 192)),
            None,
            100, // panel_width
            (8, 16), // default font size
        ) as usize;
        assert_eq!(height, expected);
        assert!(height > 1, "Image marker should be taller than 1 row");
    }

    #[test]
    fn test_wrapped_line_height_for_image_marker_no_cache() {
        // When image is not in cache, height should be 1 (fallback)
        let lines = vec![
            DiffLine::file_header("test.png"),
            DiffLine::image_marker("test.png"),
        ];
        let app = TestAppBuilder::new().with_lines(lines).build();

        let image_line = &app.lines[1];
        let height = app.wrapped_line_height(image_line);

        assert_eq!(height, 1, "Image marker without cache data should be 1 row");
    }

    #[test]
    fn test_max_scroll_accounts_for_image_markers() {
        use crate::image_diff::{CachedImage, ImageDiffState};
        use image::DynamicImage;

        let lines = vec![
            DiffLine::file_header("image1.png"),
            DiffLine::image_marker("image1.png"),
            DiffLine::file_header("image2.png"),
            DiffLine::image_marker("image2.png"),
            DiffLine::file_header("image3.png"),
            DiffLine::image_marker("image3.png"),
        ];
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 15;
        app.content_width = 80;
        app.panel_width = 100;

        // Add image data for all three images
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

        let items = app.compute_displayable_items();
        let max_scroll = app.max_scroll_for_items(&items);

        // With image data in cache, markers have real heights based on dimensions
        // 3 file headers (3 rows) + 3 image markers with real height
        // Total should exceed viewport of 15, so max_scroll > 0
        assert!(
            max_scroll > 0,
            "max_scroll should account for tall image markers"
        );
    }
}
