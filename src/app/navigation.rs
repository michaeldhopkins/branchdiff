use crate::diff::{DiffLine, LineSource};

use super::App;

impl App {
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
        self.clamp_scroll();
    }

    pub fn next_file(&mut self) {
        let lines = self.displayable_lines();
        if lines.is_empty() {
            return;
        }

        for (i, line) in lines.iter().enumerate().skip(self.scroll_offset + 1) {
            if line.source == LineSource::FileHeader {
                self.scroll_offset = i;
                return;
            }
        }
    }

    pub fn prev_file(&mut self) {
        let lines = self.displayable_lines();
        if lines.is_empty() || self.scroll_offset == 0 {
            return;
        }

        let current_is_header = lines
            .get(self.scroll_offset)
            .map(|l| l.source == LineSource::FileHeader)
            .unwrap_or(false);

        let search_start = if current_is_header {
            self.scroll_offset.saturating_sub(1)
        } else {
            self.scroll_offset
        };

        for i in (0..=search_start).rev() {
            if lines[i].source == LineSource::FileHeader {
                self.scroll_offset = i;
                return;
            }
        }
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

    pub fn scroll_percentage(&self) -> u16 {
        let line_count = self.displayable_line_count();
        if line_count == 0 || line_count <= self.viewport_height {
            100
        } else {
            let max_scroll = line_count.saturating_sub(self.viewport_height);
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
        // Create app with 50 lines, viewport of 10
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

        // Even if scroll_offset exceeds max (shouldn't happen but must not show >100%)
        app.scroll_offset = 100;
        assert!(app.scroll_percentage() <= 100,
            "scroll_percentage should never exceed 100, got {}",
            app.scroll_percentage());

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
}
