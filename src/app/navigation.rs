use crate::diff::LineSource;

use super::{App, DisplayableItem, FrameContext};

impl App {
    /// Invalidate cached view state after navigation changes.
    /// Call this after modifying scroll_offset to trigger re-computation
    /// of inline spans and clear any text selection.
    fn invalidate_view(&mut self) {
        self.view.needs_inline_spans = true;
        self.clear_selection();
    }

    fn current_abs_row(&self, ctx: &FrameContext) -> usize {
        ctx.to_abs_row(self, self.view.scroll_offset, self.view.sub_row)
    }

    fn set_scroll_abs(&mut self, ctx: &FrameContext, abs: usize) {
        let max_abs = ctx.total_rows(self).saturating_sub(self.view.viewport_height);
        let (item, sub) = ctx.from_abs_row(self, abs.min(max_abs));
        if item != self.view.scroll_offset || sub != self.view.sub_row {
            self.view.scroll_offset = item;
            self.view.sub_row = sub;
            self.invalidate_view();
        }
    }

    pub fn scroll_up(&mut self, n: usize) {
        let ctx = FrameContext::new(self);
        let abs = self.current_abs_row(&ctx).saturating_sub(n);
        self.set_scroll_abs(&ctx, abs);
    }

    pub fn scroll_down(&mut self, n: usize) {
        let ctx = FrameContext::new(self);
        let abs = self.current_abs_row(&ctx).saturating_add(n);
        self.set_scroll_abs(&ctx, abs);
    }

    /// Get the file path of the file currently at the top of the viewport.
    pub fn current_file_path(&self) -> Option<String> {
        let items = self.compute_displayable_items();
        if items.is_empty() {
            return None;
        }
        // Look backwards from scroll offset to find the most recent file header
        for i in (0..=self.view.scroll_offset.min(items.len() - 1)).rev() {
            if let DisplayableItem::Line(idx) = &items[i] {
                let line = &self.lines[*idx];
                if line.source == LineSource::FileHeader {
                    return line.file_path.clone();
                }
            }
        }
        // Fallback: first file
        self.files.first()
            .and_then(|f| f.lines.first())
            .and_then(|l| l.file_path.clone())
    }

    pub fn next_file(&mut self) {
        let items = self.compute_displayable_items();
        if items.is_empty() {
            return;
        }

        for (i, item) in items.iter().enumerate().skip(self.view.scroll_offset + 1) {
            if let DisplayableItem::Line(idx) = item
                && self.lines[*idx].source == LineSource::FileHeader
            {
                self.view.scroll_offset = i;
                self.view.sub_row = 0;
                self.invalidate_view();
                return;
            }
        }
    }

    /// Navigate to next file using pre-computed FrameContext
    pub fn next_file_with_frame(&mut self, ctx: &FrameContext) {
        if ctx.item_count() == 0 {
            return;
        }

        if let Some(pos) = ctx.find_next_file_header(self, self.view.scroll_offset) {
            self.view.scroll_offset = pos;
            self.view.sub_row = 0;
            self.invalidate_view();
        }
    }

    pub fn prev_file(&mut self) {
        let items = self.compute_displayable_items();
        if items.is_empty() || self.view.scroll_offset == 0 {
            return;
        }

        let current_is_header = match items.get(self.view.scroll_offset) {
            Some(DisplayableItem::Line(idx)) => self.lines[*idx].source == LineSource::FileHeader,
            _ => false,
        };

        let search_start = if current_is_header {
            self.view.scroll_offset.saturating_sub(1)
        } else {
            self.view.scroll_offset
        };

        for i in (0..=search_start).rev() {
            if let DisplayableItem::Line(idx) = items[i]
                && self.lines[idx].source == LineSource::FileHeader
            {
                self.view.scroll_offset = i;
                self.view.sub_row = 0;
                self.invalidate_view();
                return;
            }
        }
    }

    /// Navigate to previous file using pre-computed FrameContext
    pub fn prev_file_with_frame(&mut self, ctx: &FrameContext) {
        if ctx.item_count() == 0 || self.view.scroll_offset == 0 {
            return;
        }

        if let Some(pos) = ctx.find_prev_file_header(self, self.view.scroll_offset) {
            self.view.scroll_offset = pos;
            self.view.sub_row = 0;
            self.invalidate_view();
        }
    }

    pub fn page_up(&mut self) {
        let page_size = self.view.viewport_height.saturating_sub(2);
        self.scroll_up(page_size);
    }

    pub fn page_down(&mut self) {
        let page_size = self.view.viewport_height.saturating_sub(2);
        self.scroll_down(page_size);
    }

    pub fn go_to_top(&mut self) {
        if self.view.scroll_offset != 0 || self.view.sub_row != 0 {
            self.view.scroll_offset = 0;
            self.view.sub_row = 0;
            self.invalidate_view();
        }
    }

    pub fn go_to_bottom(&mut self) {
        let ctx = FrameContext::new(self);
        self.go_to_bottom_with_frame(&ctx);
    }

    /// Go to bottom using pre-computed FrameContext
    pub fn go_to_bottom_with_frame(&mut self, ctx: &FrameContext) {
        let (item, sub) = ctx.max_scroll(self);
        if item != self.view.scroll_offset || sub != self.view.sub_row {
            self.view.scroll_offset = item;
            self.view.sub_row = sub;
            self.invalidate_view();
        }
    }

    /// Set viewport height (called during rendering)
    pub fn set_viewport_height(&mut self, height: usize) {
        if self.view.viewport_height != height {
            self.view.viewport_height = height;
            self.view.needs_inline_spans = true;
            self.clamp_scroll();
        }
    }

    pub(super) fn clamp_scroll(&mut self) {
        let ctx = FrameContext::new(self);
        self.clamp_scroll_with_frame(&ctx);
    }

    pub fn clamp_scroll_with_frame(&mut self, ctx: &FrameContext) {
        let (item, sub) = ctx.clamp(self, self.view.scroll_offset, self.view.sub_row);
        self.view.scroll_offset = item;
        self.view.sub_row = sub;
    }

    pub fn scroll_down_with_frame(&mut self, n: usize, ctx: &FrameContext) {
        let abs = self.current_abs_row(ctx).saturating_add(n);
        self.set_scroll_abs(ctx, abs);
    }

    pub fn scroll_up_with_frame(&mut self, n: usize, ctx: &FrameContext) {
        let abs = self.current_abs_row(ctx).saturating_sub(n);
        self.set_scroll_abs(ctx, abs);
    }

    pub fn scroll_percentage(&self) -> u16 {
        let ctx = FrameContext::new(self);
        self.scroll_percentage_with_frame(&ctx)
    }

    /// Scroll percentage, in absolute screen rows so it advances within one
    /// long wrapped line rather than jumping a whole item at a time.
    pub fn scroll_percentage_with_frame(&self, ctx: &FrameContext) -> u16 {
        let max_abs = ctx.total_rows(self).saturating_sub(self.view.viewport_height);
        if max_abs == 0 {
            100
        } else {
            let cur = self.current_abs_row(ctx);
            let pct = ((cur as f64 / max_abs as f64) * 100.0) as u16;
            pct.min(100)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::DiffLine;
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
        app.view.scroll_offset = 0;

        app.next_file();

        assert_eq!(app.view.scroll_offset, 3);
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
        app.view.scroll_offset = 1;

        app.next_file();

        assert_eq!(app.view.scroll_offset, 3);
    }

    #[test]
    fn test_next_file_no_more_files() {
        let lines = vec![
            DiffLine::file_header("file1.rs"),
            base_line("line1"),
        ];
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view.scroll_offset = 0;

        app.next_file();

        assert_eq!(app.view.scroll_offset, 0);
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
        app.view.scroll_offset = 4;

        app.prev_file();

        assert_eq!(app.view.scroll_offset, 3);
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
        app.view.scroll_offset = 3;

        app.prev_file();

        assert_eq!(app.view.scroll_offset, 0);
    }

    #[test]
    fn test_prev_file_at_first_file() {
        let lines = vec![
            DiffLine::file_header("file1.rs"),
            base_line("line1"),
        ];
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view.scroll_offset = 0;

        app.prev_file();

        assert_eq!(app.view.scroll_offset, 0);
    }

    #[test]
    fn test_next_file_empty_lines() {
        let mut app = TestAppBuilder::new().build();
        app.next_file();
        assert_eq!(app.view.scroll_offset, 0);
    }

    #[test]
    fn test_prev_file_empty_lines() {
        let mut app = TestAppBuilder::new().build();
        app.prev_file();
        assert_eq!(app.view.scroll_offset, 0);
    }

    #[test]
    fn test_scroll_percentage_at_top_returns_zero() {
        let lines: Vec<DiffLine> = (0..50).map(|i| base_line(&format!("line {}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view.viewport_height = 10;
        app.view.scroll_offset = 0;

        assert_eq!(app.scroll_percentage(), 0);
    }

    #[test]
    fn test_scroll_percentage_at_bottom_returns_100() {
        let lines: Vec<DiffLine> = (0..50).map(|i| base_line(&format!("line {}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view.viewport_height = 10;
        app.view.scroll_offset = 40; // 50 lines - 10 viewport = 40 max scroll

        assert_eq!(app.scroll_percentage(), 100);
    }

    #[test]
    fn test_scroll_percentage_capped_at_100() {
        let lines: Vec<DiffLine> = (0..50).map(|i| base_line(&format!("line {}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view.viewport_height = 10;
        app.view.scroll_offset = 100; // Beyond max scroll

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
        app.view.viewport_height = 10;

        let ctx = FrameContext::new(&app);
        assert_eq!(ctx.max_scroll(&app), (10, 0));
    }

    #[test]
    fn test_clamp_scroll() {
        let lines: Vec<DiffLine> = (0..20)
            .map(|i| base_line(&format!("line {}", i)))
            .collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view.viewport_height = 10;
        app.view.scroll_offset = 100; // Way past end

        app.clamp_scroll();

        assert_eq!(app.view.scroll_offset, 10); // Clamped to max
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
        app.view.viewport_height = 40;
        app.view.content_width = 80;
        app.view.panel_width = 100;

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

        // total_rows = header (1 row) + image height
        let ctx = FrameContext::new(&app);
        let height = ctx.total_rows(&app) - 1;

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
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view.content_width = 80;
        app.view.panel_width = 100;

        // header (1 row) + image fallback (1 row)
        let ctx = FrameContext::new(&app);
        assert_eq!(
            ctx.total_rows(&app),
            2,
            "Image marker without cache data should be 1 row"
        );
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
        app.view.viewport_height = 15;
        app.view.content_width = 80;
        app.view.panel_width = 100;

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

        let ctx = FrameContext::new(&app);
        assert!(
            ctx.max_scroll(&app) > (0, 0),
            "max_scroll should account for tall image markers"
        );
    }

    /// Build an app whose items wrap to the given row heights (content width 10).
    fn heights_app(heights: &[usize], viewport: usize) -> App {
        let lines: Vec<_> = heights
            .iter()
            .map(|&h| base_line(&"x".repeat((h * 10).saturating_sub(5).max(1))))
            .collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view.content_width = 10;
        app.view.viewport_height = viewport;
        app
    }

    fn pos(app: &App) -> (usize, usize) {
        (app.view.scroll_offset, app.view.sub_row)
    }

    #[test]
    fn test_scroll_down_crosses_item_boundary() {
        let mut app = heights_app(&[1, 3, 1, 1, 1, 1, 1], 3);
        app.scroll_down(1);
        assert_eq!(pos(&app), (1, 0), "abs row 1 = start of the 3-row item");
        app.scroll_down(2);
        assert_eq!(pos(&app), (1, 2), "still inside the 3-row item");
        app.scroll_down(1);
        assert_eq!(pos(&app), (2, 0), "crossed into the next item");
    }

    #[test]
    fn test_scroll_up_underflows_to_previous_item_last_row() {
        let mut app = heights_app(&[1, 3, 1, 1, 1, 1, 1], 3);
        app.view.scroll_offset = 2;
        app.view.sub_row = 0;
        app.scroll_up(1);
        assert_eq!(pos(&app), (1, 2), "underflow lands on the previous item's last row");
    }

    #[test]
    fn test_resize_reclamps_sub_row() {
        let mut app = heights_app(&[1, 10], 4);
        let (i, s) = FrameContext::new(&app).max_scroll(&app);
        app.view.scroll_offset = i;
        app.view.sub_row = s;
        assert_eq!(pos(&app), (1, 6));

        app.set_viewport_height(11);
        assert_eq!(pos(&app), (0, 0));
    }

    #[test]
    fn current_file_path_empty_diff_is_none() {
        // Regression: an empty diff must not panic indexing items[0].
        let app = TestAppBuilder::new().build();
        assert_eq!(app.current_file_path(), None);
    }

    #[test]
    fn test_width_change_reclamps_sub_row() {
        let mut app = heights_app(&[10], 4);
        let (i, s) = FrameContext::new(&app).max_scroll(&app);
        app.view.scroll_offset = i;
        app.view.sub_row = s;
        assert!(app.view.sub_row > 0, "precondition: scrolled into the line");

        // Widening re-wraps the line shorter; the stale sub_row must clamp back.
        app.set_content_layout(1, 1, 0, 200, 210);
        assert_eq!(pos(&app), (0, 0));
    }

    #[test]
    fn test_scroll_percentage_advances_within_one_long_line() {
        let mut app = heights_app(&[1, 100], 10);
        app.view.scroll_offset = 1;
        app.view.sub_row = 0;
        let near_top = app.scroll_percentage();
        app.view.sub_row = 45;
        let mid = app.scroll_percentage();
        assert!(
            mid > near_top,
            "percentage advances while scrolling through one long wrapped line ({near_top} -> {mid})"
        );
    }
}
