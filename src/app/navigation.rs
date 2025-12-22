use crate::diff::{DiffLine, LineSource};

use super::{App, DisplayableItem, FrameContext};

impl App {
    pub fn scroll_up(&mut self, n: usize) {
        let old_offset = self.scroll_offset;
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        if self.scroll_offset != old_offset {
            self.needs_inline_spans = true;
        }
    }

    pub fn scroll_down(&mut self, n: usize) {
        let old_offset = self.scroll_offset;
        self.scroll_offset = self.scroll_offset.saturating_add(n);
        self.clamp_scroll();
        if self.scroll_offset != old_offset {
            self.needs_inline_spans = true;
        }
    }

    pub fn next_file(&mut self) {
        let items = self.compute_displayable_items();
        if items.is_empty() {
            return;
        }

        for (i, item) in items.iter().enumerate().skip(self.scroll_offset + 1) {
            if let DisplayableItem::Line(idx) = item {
                if self.lines[*idx].source == LineSource::FileHeader {
                    self.scroll_offset = i;
                    self.needs_inline_spans = true;
                    return;
                }
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
            if let DisplayableItem::Line(idx) = items[i] {
                if self.lines[idx].source == LineSource::FileHeader {
                    self.scroll_offset = i;
                    self.needs_inline_spans = true;
                    return;
                }
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
        }
    }

    pub fn go_to_bottom(&mut self) {
        let old_offset = self.scroll_offset;
        let items = self.compute_displayable_items();
        self.scroll_offset = self.max_scroll_for_items(&items);
        if self.scroll_offset != old_offset {
            self.needs_inline_spans = true;
        }
    }

    /// Go to bottom using pre-computed FrameContext
    pub fn go_to_bottom_with_frame(&mut self, ctx: &FrameContext) {
        let old_offset = self.scroll_offset;
        self.scroll_offset = ctx.max_scroll(self);
        if self.scroll_offset != old_offset {
            self.needs_inline_spans = true;
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
        }
    }

    /// Calculate how many screen rows a line will take when wrapped
    pub(super) fn wrapped_line_height(&self, line: &DiffLine) -> usize {
        if self.content_width == 0 {
            return 1;
        }
        let content_len = line.content.len();
        if content_len <= self.content_width {
            1
        } else {
            content_len.div_ceil(self.content_width)
        }
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
            needs_inline_spans: true,
        }
    }

    fn base_line(content: &str) -> DiffLine {
        DiffLine::new(LineSource::Base, content.to_string(), ' ', None)
    }

    #[test]
    fn test_next_file_jumps_to_header() {
        let lines = vec![
            DiffLine::file_header("file1.rs"),
            base_line("line1"),
            base_line("line2"),
            DiffLine::file_header("file2.rs"),
            base_line("line3"),
        ];
        let mut app = create_test_app(lines);
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
        let mut app = create_test_app(lines);
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
        let mut app = create_test_app(lines);
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
        let mut app = create_test_app(lines);
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
        let mut app = create_test_app(lines);
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
        let mut app = create_test_app(lines);
        app.scroll_offset = 0;

        app.prev_file();

        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_next_file_empty_lines() {
        let mut app = create_test_app(vec![]);
        app.next_file();
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_prev_file_empty_lines() {
        let mut app = create_test_app(vec![]);
        app.prev_file();
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_scroll_percentage_bounds() {
        let lines: Vec<DiffLine> = (0..50)
            .map(|i| base_line(&format!("line {}", i)))
            .collect();
        let mut app = create_test_app(lines);
        app.viewport_height = 10;

        // At top: 0%
        app.scroll_offset = 0;
        assert_eq!(app.scroll_percentage(), 0);

        // At bottom: 100%
        app.scroll_offset = 40; // 50 - 10 = 40
        assert_eq!(app.scroll_percentage(), 100);

        // Even if scroll_offset exceeds max
        app.scroll_offset = 100;
        assert!(
            app.scroll_percentage() <= 100,
            "scroll_percentage should never exceed 100, got {}",
            app.scroll_percentage()
        );

        // Empty lines: 100%
        let empty_app = create_test_app(vec![]);
        assert_eq!(empty_app.scroll_percentage(), 100);

        // Lines fit in viewport: 100%
        let small_lines: Vec<DiffLine> = (0..5)
            .map(|i| base_line(&format!("line {}", i)))
            .collect();
        let small_app = create_test_app(small_lines);
        assert_eq!(small_app.scroll_percentage(), 100);
    }

    #[test]
    fn test_max_scroll_for_items() {
        let lines: Vec<DiffLine> = (0..20)
            .map(|i| base_line(&format!("line {}", i)))
            .collect();
        let mut app = create_test_app(lines);
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
        let mut app = create_test_app(lines);
        app.viewport_height = 10;
        app.scroll_offset = 100; // Way past end

        app.clamp_scroll();

        assert_eq!(app.scroll_offset, 10); // Clamped to max
    }
}
