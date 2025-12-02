use crate::diff::DiffLine;

use super::App;

impl App {
    /// Scroll up by n lines
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    /// Scroll down by n lines
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
        self.clamp_scroll();
    }

    /// Page up
    pub fn page_up(&mut self) {
        let page_size = self.viewport_height.saturating_sub(2);
        self.scroll_up(page_size);
    }

    /// Page down
    pub fn page_down(&mut self) {
        let page_size = self.viewport_height.saturating_sub(2);
        self.scroll_down(page_size);
    }

    /// Go to top
    pub fn go_to_top(&mut self) {
        self.scroll_offset = 0;
    }

    /// Go to bottom - find the scroll offset where the last logical line's bottom
    /// aligns with the bottom of the viewport
    pub fn go_to_bottom(&mut self) {
        let all_lines = self.displayable_lines();
        if all_lines.is_empty() {
            self.scroll_offset = 0;
            return;
        }

        // Calculate total screen rows if we showed all lines
        let total_rows: usize = all_lines.iter()
            .map(|l| self.wrapped_line_height(l))
            .sum();

        if total_rows <= self.viewport_height {
            self.scroll_offset = 0;
            return;
        }

        // Find the scroll offset where the viewport ends at the last line
        // Work backwards from the end to find how many logical lines fit in viewport
        let mut rows_from_end = 0;
        let mut lines_from_end = 0;

        for line in all_lines.iter().rev() {
            let line_height = self.wrapped_line_height(line);
            if rows_from_end + line_height > self.viewport_height {
                break;
            }
            rows_from_end += line_height;
            lines_from_end += 1;
        }

        self.scroll_offset = all_lines.len().saturating_sub(lines_from_end);
    }

    /// Set viewport height (called during rendering)
    pub fn set_viewport_height(&mut self, height: usize) {
        self.viewport_height = height;
        self.clamp_scroll();
    }

    /// Clamp scroll offset to valid range (accounting for line wrapping)
    pub(super) fn clamp_scroll(&mut self) {
        let all_lines = self.displayable_lines();
        if all_lines.is_empty() {
            self.scroll_offset = 0;
            return;
        }

        // Calculate max scroll: the scroll offset where last line ends at bottom
        let total_rows: usize = all_lines.iter()
            .map(|l| self.wrapped_line_height(l))
            .sum();

        if total_rows <= self.viewport_height {
            self.scroll_offset = 0;
            return;
        }

        // Find max scroll offset (same logic as go_to_bottom)
        let mut rows_from_end = 0;
        let mut lines_from_end = 0;

        for line in all_lines.iter().rev() {
            let line_height = self.wrapped_line_height(line);
            if rows_from_end + line_height > self.viewport_height {
                break;
            }
            rows_from_end += line_height;
            lines_from_end += 1;
        }

        let max_scroll = all_lines.len().saturating_sub(lines_from_end);
        self.scroll_offset = self.scroll_offset.min(max_scroll);
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
            // Ceiling division: how many rows needed to show all content
            (content_len + self.content_width - 1) / self.content_width
        }
    }

    /// Get visible lines for current scroll position, accounting for line wrapping
    pub fn visible_lines(&self) -> Vec<DiffLine> {
        let all_lines = self.displayable_lines();
        if all_lines.is_empty() {
            return Vec::new();
        }

        let start = self.scroll_offset.min(all_lines.len());

        // Calculate how many logical lines we can show, accounting for wrapping
        let mut screen_rows_used = 0;
        let mut end = start;

        while end < all_lines.len() && screen_rows_used < self.viewport_height {
            screen_rows_used += self.wrapped_line_height(&all_lines[end]);
            end += 1;
        }

        all_lines[start..end].to_vec()
    }

    /// Get scroll percentage for status bar
    pub fn scroll_percentage(&self) -> u16 {
        let line_count = self.displayable_line_count();
        if line_count == 0 || line_count <= self.viewport_height {
            100
        } else {
            let max_scroll = line_count - self.viewport_height;
            ((self.scroll_offset as f64 / max_scroll as f64) * 100.0) as u16
        }
    }
}
