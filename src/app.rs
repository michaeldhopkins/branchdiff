use std::path::PathBuf;

use anyhow::{Context, Result};
use arboard::Clipboard;

use crate::diff::{compute_file_diff_v2, DiffLine, FileDiff, LineSource};
use crate::git;

/// Represents a position in the diff view (row, column)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub row: usize,
    pub col: usize,
}

/// Selection state for text copy
#[derive(Debug, Clone)]
pub struct Selection {
    pub start: Position,
    pub end: Position,
    pub active: bool,
}

/// Application state
pub struct App {
    /// Path to the git repository root
    pub repo_path: PathBuf,
    /// The base branch (main or master)
    pub base_branch: String,
    /// The merge-base commit
    pub merge_base: String,
    /// Current branch name (if any)
    pub current_branch: Option<String>,
    /// All file diffs
    pub files: Vec<FileDiff>,
    /// Flattened lines for display
    pub lines: Vec<DiffLine>,
    /// Current scroll offset
    pub scroll_offset: usize,
    /// Viewport height (set during rendering)
    pub viewport_height: usize,
    /// Error message to display (if any)
    pub error: Option<String>,
    /// Whether to show the help modal
    pub show_help: bool,
    /// Whether to show only context around changes (vs full file)
    pub context_only: bool,
    /// Current text selection (if any)
    pub selection: Option<Selection>,
    /// Content area offset (x, y) for coordinate mapping
    pub content_offset: (u16, u16),
    /// Width of line number column (for extracting content without line numbers)
    pub line_num_width: usize,
    /// Available width for content (used for wrapping calculation)
    pub content_width: usize,
}

impl App {
    /// Create a new App instance
    pub fn new(repo_path: PathBuf) -> Result<Self> {
        let base_branch = git::detect_base_branch(&repo_path)
            .unwrap_or_else(|_| "main".to_string());

        let merge_base = git::get_merge_base(&repo_path, &base_branch)
            .unwrap_or_default();

        let current_branch = git::get_current_branch(&repo_path)
            .unwrap_or(None);

        let mut app = Self {
            repo_path,
            base_branch,
            merge_base,
            current_branch,
            files: Vec::new(),
            lines: Vec::new(),
            scroll_offset: 0,
            viewport_height: 20, // Default, will be updated during render
            error: None,
            show_help: false,
            context_only: true, // Default to context-only view
            selection: None,
            content_offset: (1, 1), // Default border offset
            line_num_width: 0,
            content_width: 80, // Default, will be updated during render
        };

        app.refresh()?;
        Ok(app)
    }

    /// Refresh all diffs from git
    pub fn refresh(&mut self) -> Result<()> {
        self.error = None;

        // Update merge-base (might have changed if branch was rebased)
        self.merge_base = git::get_merge_base(&self.repo_path, &self.base_branch)
            .unwrap_or_default();

        // Get all changed files
        let changed_files = git::get_all_changed_files(&self.repo_path, &self.merge_base)
            .context("Failed to get changed files")?;

        self.files.clear();
        self.lines.clear();

        for file in changed_files {
            // Skip binary files
            if git::is_binary_file(&self.repo_path, &file.path) {
                self.lines.push(DiffLine::file_header(&file.path));
                self.lines.push(DiffLine::new(
                    LineSource::Base,
                    "[binary file]".to_string(),
                    ' ',
                    None,
                ));
                continue;
            }

            // Get content at each state
            let base_content = if self.merge_base.is_empty() {
                None
            } else {
                git::get_file_at_ref(&self.repo_path, &file.path, &self.merge_base)
                    .ok()
                    .flatten()
            };

            let head_content = git::get_file_at_ref(&self.repo_path, &file.path, "HEAD")
                .ok()
                .flatten();

            // Index content: use empty string as ref for staged content
            let index_content = git::get_file_at_ref(&self.repo_path, &file.path, "")
                .ok()
                .flatten();

            let working_content = git::get_working_tree_file(&self.repo_path, &file.path)
                .ok()
                .flatten();

            // Compute the diff
            let file_diff = compute_file_diff_v2(
                &file.path,
                base_content.as_deref(),
                head_content.as_deref(),
                index_content.as_deref(),
                working_content.as_deref(),
            );

            // Add to flattened lines
            for line in &file_diff.lines {
                self.lines.push(line.clone());
            }

            // Add empty line between files
            self.lines.push(DiffLine::new(LineSource::Base, String::new(), ' ', None));

            self.files.push(file_diff);
        }

        // Ensure scroll offset is valid
        self.clamp_scroll();

        Ok(())
    }

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
    fn clamp_scroll(&mut self) {
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

    /// Get the count of displayable lines (respects context_only mode)
    fn displayable_line_count(&self) -> usize {
        if !self.context_only {
            self.lines.len()
        } else {
            self.build_context_lines().len()
        }
    }

    /// Compute which original line indices are visible in context mode
    fn compute_context_visibility(&self) -> Vec<bool> {
        const CONTEXT_LINES: usize = 5;

        // First pass: mark which lines are "interesting" (changes or headers)
        // A line is interesting if:
        // 1. Its source indicates a change (Committed, Staged, Unstaged, Deleted*, FileHeader)
        // 2. OR it has inline spans (meaning it's a merged modification line)
        let interesting: Vec<bool> = self.lines.iter()
            .map(|line| {
                // Lines with inline spans are always interesting (they show modifications)
                if !line.inline_spans.is_empty() {
                    return true;
                }
                matches!(line.source,
                    LineSource::Committed |
                    LineSource::Staged |
                    LineSource::Unstaged |
                    LineSource::DeletedBase |
                    LineSource::DeletedCommitted |
                    LineSource::DeletedStaged |
                    LineSource::FileHeader
                )
            })
            .collect();

        // Second pass: mark lines within CONTEXT_LINES of interesting lines
        let mut show = vec![false; self.lines.len()];
        for (i, &is_interesting) in interesting.iter().enumerate() {
            if is_interesting {
                let start = i.saturating_sub(CONTEXT_LINES);
                let end = (i + CONTEXT_LINES + 1).min(self.lines.len());
                for j in start..end {
                    show[j] = true;
                }
            }
        }
        show
    }

    /// Build filtered lines with elided markers for context-only mode
    /// Returns (filtered_lines, mapping from filtered index to original index)
    fn build_context_lines_with_mapping(&self) -> (Vec<DiffLine>, Vec<Option<usize>>) {
        let show = self.compute_context_visibility();

        // Build result with elided markers between gaps
        let mut result = Vec::new();
        let mut index_map = Vec::new(); // Maps filtered index -> original index (None for elided)
        let mut last_shown: Option<usize> = None;

        for (i, line) in self.lines.iter().enumerate() {
            if show[i] {
                // Check if there's a gap since last shown line
                if let Some(last) = last_shown {
                    let gap = i - last - 1;
                    if gap > 0 {
                        result.push(DiffLine::elided(gap));
                        index_map.push(None); // Elided marker has no original index
                    }
                }
                result.push(line.clone());
                index_map.push(Some(i));
                last_shown = Some(i);
            }
        }

        (result, index_map)
    }

    /// Build filtered lines with elided markers for context-only mode
    fn build_context_lines(&self) -> Vec<DiffLine> {
        self.build_context_lines_with_mapping().0
    }

    /// Get all displayable lines (filtered if context_only is true)
    pub fn displayable_lines(&self) -> Vec<DiffLine> {
        if !self.context_only {
            return self.lines.clone();
        }
        self.build_context_lines()
    }

    /// Calculate how many screen rows a line will take when wrapped
    fn wrapped_line_height(&self, line: &DiffLine) -> usize {
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

    /// Get status text
    pub fn status_text(&self) -> String {
        let branch_info = match &self.current_branch {
            Some(b) => format!("{} vs {}", b, self.base_branch),
            None => format!("HEAD vs {}", self.base_branch),
        };

        let file_count = self.files.len();
        let line_count = self.displayable_line_count();
        let mode = if self.context_only { " [context]" } else { "" };

        format!(
            "{} | {} file{} | {} line{}{} | {}%",
            branch_info,
            file_count,
            if file_count == 1 { "" } else { "s" },
            line_count,
            if line_count == 1 { "" } else { "s" },
            mode,
            self.scroll_percentage()
        )
    }

    /// Toggle help modal visibility
    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    /// Toggle context-only view, anchoring on the middle visible line
    pub fn toggle_context_only(&mut self) {
        if self.lines.is_empty() {
            self.context_only = !self.context_only;
            return;
        }

        // Find the original line index of the middle visible line
        let middle_offset = self.viewport_height / 2;
        let anchor_original_idx = self.get_original_index_at_offset(middle_offset);

        // Toggle the mode
        self.context_only = !self.context_only;

        // Find where this line (or closest) appears in the new mode
        if let Some(anchor_idx) = anchor_original_idx {
            let new_position = self.find_position_for_original_index(anchor_idx);
            // Set scroll so this line appears in the middle
            self.scroll_offset = new_position.saturating_sub(middle_offset);
        }

        self.clamp_scroll();
    }

    /// Get the original line index for a line at the given offset from scroll position
    fn get_original_index_at_offset(&self, offset: usize) -> Option<usize> {
        let target_pos = self.scroll_offset + offset;

        if self.context_only {
            // In context mode, use the mapping to find original index
            let (_, index_map) = self.build_context_lines_with_mapping();
            if target_pos < index_map.len() {
                // If we land on an elided marker, find the closest real line
                if let Some(idx) = index_map[target_pos] {
                    return Some(idx);
                }
                // Search nearby for a real line
                for delta in 1..index_map.len() {
                    if target_pos >= delta {
                        if let Some(Some(idx)) = index_map.get(target_pos - delta) {
                            return Some(*idx);
                        }
                    }
                    if let Some(Some(idx)) = index_map.get(target_pos + delta) {
                        return Some(*idx);
                    }
                }
            }
            // Fallback: return last visible original index
            index_map.iter().rev().find_map(|x| *x)
        } else {
            // In full mode, position directly maps to original index
            if target_pos < self.lines.len() {
                Some(target_pos)
            } else if !self.lines.is_empty() {
                Some(self.lines.len() - 1)
            } else {
                None
            }
        }
    }

    /// Find the position in current mode for an original line index
    /// If the line isn't visible (elided), finds the closest visible line
    fn find_position_for_original_index(&self, original_idx: usize) -> usize {
        if !self.context_only {
            // In full mode, position equals original index
            original_idx.min(self.lines.len().saturating_sub(1))
        } else {
            // In context mode, search the mapping
            let (_, index_map) = self.build_context_lines_with_mapping();
            let visibility = self.compute_context_visibility();

            // Check if this exact line is visible
            if original_idx < visibility.len() && visibility[original_idx] {
                // Find its position in the filtered list
                for (pos, mapped_idx) in index_map.iter().enumerate() {
                    if *mapped_idx == Some(original_idx) {
                        return pos;
                    }
                }
            }

            // Line is elided - find closest visible line
            let mut best_pos = 0;
            let mut best_distance = usize::MAX;

            for (pos, mapped_idx) in index_map.iter().enumerate() {
                if let Some(idx) = mapped_idx {
                    let distance = if *idx > original_idx {
                        *idx - original_idx
                    } else {
                        original_idx - *idx
                    };
                    if distance < best_distance {
                        best_distance = distance;
                        best_pos = pos;
                    }
                }
            }

            best_pos
        }
    }

    /// Get the file path of the first visible line
    pub fn current_file(&self) -> Option<String> {
        self.visible_lines()
            .into_iter()
            .find_map(|line| line.file_path)
    }

    /// Set content area layout info (called during rendering)
    pub fn set_content_layout(&mut self, offset_x: u16, offset_y: u16, line_num_width: usize, content_width: usize) {
        self.content_offset = (offset_x, offset_y);
        self.line_num_width = line_num_width;
        self.content_width = content_width;
    }

    /// Start a selection at the given screen coordinates
    pub fn start_selection(&mut self, screen_x: u16, screen_y: u16) {
        if let Some(pos) = self.screen_to_content_position(screen_x, screen_y) {
            self.selection = Some(Selection {
                start: pos,
                end: pos,
                active: true,
            });
        }
    }

    /// Update selection end point during drag
    pub fn update_selection(&mut self, screen_x: u16, screen_y: u16) {
        // Get position first to avoid borrow conflict
        let pos = self.screen_to_content_position(screen_x, screen_y);
        if let Some(ref mut sel) = self.selection {
            if sel.active {
                if let Some(p) = pos {
                    sel.end = p;
                }
            }
        }
    }

    /// End selection (mouse released)
    pub fn end_selection(&mut self) {
        if let Some(ref mut sel) = self.selection {
            sel.active = false;
        }
    }

    /// Clear current selection
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Convert screen coordinates to content position
    fn screen_to_content_position(&self, screen_x: u16, screen_y: u16) -> Option<Position> {
        let (offset_x, offset_y) = self.content_offset;

        // Check if within content area
        if screen_x < offset_x || screen_y < offset_y {
            return None;
        }

        let content_x = (screen_x - offset_x) as usize;
        let content_y = (screen_y - offset_y) as usize;

        // Convert to absolute line position
        let line_idx = self.scroll_offset + content_y;

        Some(Position {
            row: line_idx,
            col: content_x,
        })
    }

    /// Get selected text (content only, without line numbers or prefixes)
    pub fn get_selected_text(&self) -> Option<String> {
        let sel = self.selection.as_ref()?;

        // Normalize selection (start should be before end)
        let (start, end) = if sel.start.row < sel.end.row
            || (sel.start.row == sel.end.row && sel.start.col <= sel.end.col)
        {
            (sel.start, sel.end)
        } else {
            (sel.end, sel.start)
        };

        let all_lines = self.displayable_lines();
        let mut result = String::new();

        // Calculate the prefix length to skip (line number + prefix char + spaces)
        // Format: "{line_num:>width} {prefix} {content}"
        let prefix_len = self.line_num_width + 3; // width + space + prefix + space

        for row in start.row..=end.row {
            if row >= all_lines.len() {
                break;
            }

            let line = &all_lines[row];

            // Skip file headers and elided markers for copying
            if line.source == LineSource::FileHeader || line.source == LineSource::Elided {
                continue;
            }

            // Get content only (skip line number and prefix)
            let content = &line.content;

            if start.row == end.row {
                // Single line selection
                let start_in_content = start.col.saturating_sub(prefix_len);
                let end_in_content = end.col.saturating_sub(prefix_len);
                if start_in_content < content.len() {
                    let actual_end = end_in_content.min(content.len());
                    if actual_end > start_in_content {
                        result.push_str(&content[start_in_content..actual_end]);
                    }
                }
            } else if row == start.row {
                // First line of multi-line selection
                let start_in_content = start.col.saturating_sub(prefix_len);
                if start_in_content < content.len() {
                    result.push_str(&content[start_in_content..]);
                }
                result.push('\n');
            } else if row == end.row {
                // Last line of multi-line selection
                let end_in_content = end.col.saturating_sub(prefix_len);
                let actual_end = end_in_content.min(content.len());
                result.push_str(&content[..actual_end]);
            } else {
                // Middle lines - take entire content
                result.push_str(content);
                result.push('\n');
            }
        }

        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }

    /// Copy selected text to clipboard
    pub fn copy_selection(&mut self) -> Result<bool> {
        if let Some(text) = self.get_selected_text() {
            let mut clipboard = Clipboard::new()
                .map_err(|e| anyhow::anyhow!("Failed to access clipboard: {}", e))?;
            clipboard.set_text(text)
                .map_err(|e| anyhow::anyhow!("Failed to copy to clipboard: {}", e))?;
            self.clear_selection();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Check if a line is within the current selection
    pub fn is_line_selected(&self, line_idx: usize) -> bool {
        if let Some(ref sel) = self.selection {
            let (start_row, end_row) = if sel.start.row <= sel.end.row {
                (sel.start.row, sel.end.row)
            } else {
                (sel.end.row, sel.start.row)
            };
            line_idx >= start_row && line_idx <= end_row
        } else {
            false
        }
    }

    /// Get selection range for a specific line (returns column range if selected)
    pub fn get_line_selection_range(&self, line_idx: usize) -> Option<(usize, usize)> {
        let sel = self.selection.as_ref()?;

        let (start, end) = if sel.start.row < sel.end.row
            || (sel.start.row == sel.end.row && sel.start.col <= sel.end.col)
        {
            (sel.start, sel.end)
        } else {
            (sel.end, sel.start)
        };

        if line_idx < start.row || line_idx > end.row {
            return None;
        }

        if start.row == end.row {
            Some((start.col, end.col))
        } else if line_idx == start.row {
            Some((start.col, usize::MAX))
        } else if line_idx == end.row {
            Some((0, end.col))
        } else {
            Some((0, usize::MAX))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a test app with synthetic lines
    fn create_test_app(lines: Vec<DiffLine>) -> App {
        App {
            repo_path: std::path::PathBuf::from("/tmp/test"),
            base_branch: "main".to_string(),
            merge_base: "abc123".to_string(),
            current_branch: Some("feature".to_string()),
            files: Vec::new(),
            lines,
            scroll_offset: 0,
            viewport_height: 10,
            error: None,
            show_help: false,
            context_only: false,
            selection: None,
            content_offset: (1, 1),
            line_num_width: 0,
            content_width: 80,  // Default test width
        }
    }

    /// Helper to create a base (context) line
    fn base_line(content: &str) -> DiffLine {
        DiffLine::new(LineSource::Base, content.to_string(), ' ', None)
    }

    /// Helper to create an unstaged (change) line
    fn change_line(content: &str) -> DiffLine {
        DiffLine::new(LineSource::Unstaged, content.to_string(), '+', None)
    }

    #[test]
    fn test_toggle_context_empty_lines() {
        let mut app = create_test_app(Vec::new());
        app.toggle_context_only();
        assert!(app.context_only);
        assert_eq!(app.scroll_offset, 0);
        app.toggle_context_only();
        assert!(!app.context_only);
    }

    #[test]
    fn test_toggle_context_few_lines() {
        // Fewer lines than viewport - scroll should stay at 0
        let lines = vec![
            base_line("line1"),
            change_line("changed"),
            base_line("line3"),
        ];
        let mut app = create_test_app(lines);
        app.viewport_height = 10;

        app.toggle_context_only();
        assert!(app.context_only);
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_toggle_context_anchors_on_middle_line() {
        // Create 30 lines: 10 base, 1 change, 19 base
        // In context mode, only lines around the change are shown
        let mut lines = Vec::new();
        for i in 0..10 {
            lines.push(base_line(&format!("before{}", i)));
        }
        lines.push(change_line("THE CHANGE"));
        for i in 0..19 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = create_test_app(lines);
        app.viewport_height = 10;

        // Scroll to middle of file (around line 15)
        app.scroll_offset = 10;

        // The middle of viewport is at offset 5, so line 15 in original
        // Toggle to context mode
        app.toggle_context_only();

        // Should still be showing content near line 15
        // The change is at original index 10, context shows 5 lines around it
        // So visible in context: indices 5-15 of original (lines before5..after4)
        assert!(app.context_only);
        // Scroll should be adjusted to keep similar content visible
    }

    #[test]
    fn test_toggle_context_when_middle_is_elided() {
        // Create lines where the middle will be elided in context mode
        // 50 base lines, then 1 change at the end
        let mut lines = Vec::new();
        for i in 0..50 {
            lines.push(base_line(&format!("base{}", i)));
        }
        lines.push(change_line("change at end"));

        let mut app = create_test_app(lines);
        app.viewport_height = 10;

        // Scroll to line 20 (far from the change at 50)
        app.scroll_offset = 20;

        // Toggle to context mode - line 25 (middle) will be elided
        app.toggle_context_only();

        // Should find closest visible line and anchor there
        assert!(app.context_only);
        // The only visible content is around line 50, so scroll should jump there
    }

    #[test]
    fn test_toggle_context_round_trip_near_change() {
        // Toggling twice should return to approximately the same position
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(base_line(&format!("before{}", i)));
        }
        lines.push(change_line("THE CHANGE"));
        for i in 0..20 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = create_test_app(lines);
        app.viewport_height = 10;

        // Position so the change is visible (change is at index 20)
        app.scroll_offset = 16; // Middle at 21, close to change
        let original_scroll = app.scroll_offset;

        // Toggle twice
        app.toggle_context_only();
        app.toggle_context_only();

        // Should be close to original position (may not be exact due to elided lines)
        assert!(!app.context_only);
        // Allow some tolerance since exact positioning depends on context
        let diff = if app.scroll_offset > original_scroll {
            app.scroll_offset - original_scroll
        } else {
            original_scroll - app.scroll_offset
        };
        assert!(diff <= 5, "Round trip scroll difference too large: {}", diff);
    }

    #[test]
    fn test_toggle_context_at_top() {
        let mut lines = Vec::new();
        lines.push(change_line("change at top"));
        for i in 0..30 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = create_test_app(lines);
        app.viewport_height = 10;
        app.scroll_offset = 0;

        app.toggle_context_only();

        // Should stay near top since change is at top
        assert!(app.context_only);
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_toggle_context_at_bottom() {
        let mut lines = Vec::new();
        for i in 0..30 {
            lines.push(base_line(&format!("before{}", i)));
        }
        lines.push(change_line("change at bottom"));

        let mut app = create_test_app(lines);
        app.viewport_height = 10;

        // Scroll to bottom
        app.go_to_bottom();

        app.toggle_context_only();

        // Should stay near bottom content
        assert!(app.context_only);
    }

    #[test]
    fn test_find_position_for_visible_line() {
        let mut lines = Vec::new();
        for i in 0..5 {
            lines.push(base_line(&format!("before{}", i)));
        }
        lines.push(change_line("change"));
        for i in 0..5 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = create_test_app(lines);
        app.context_only = true;

        // The change is at original index 5
        // In context mode with 5 lines of context, indices 0-10 are visible
        let pos = app.find_position_for_original_index(5);

        // Position should be valid and map back to the change
        let (_, index_map) = app.build_context_lines_with_mapping();
        assert!(pos < index_map.len());
        assert_eq!(index_map[pos], Some(5));
    }

    #[test]
    fn test_find_position_for_elided_line() {
        // Create scenario where some lines are elided
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(base_line(&format!("start{}", i)));
        }
        lines.push(change_line("change"));
        for i in 0..20 {
            lines.push(base_line(&format!("end{}", i)));
        }

        let mut app = create_test_app(lines);
        app.context_only = true;

        // Original index 0 is far from change at 20, so it's elided
        // Should find closest visible line
        let pos = app.find_position_for_original_index(0);

        // Should return a valid position
        let (filtered, _) = app.build_context_lines_with_mapping();
        assert!(pos < filtered.len());
    }

    #[test]
    fn test_context_view_shows_lines_with_inline_spans() {
        // REGRESSION TEST: Lines with inline spans (merged modifications) should be
        // visible in context view, even if their source is Base.
        //
        // A merged modification line has source=Base but contains inline_spans
        // showing what changed. These should be treated as "interesting" lines.
        use crate::diff::InlineSpan;

        let mut lines = Vec::new();

        // Add many base lines before
        for i in 0..20 {
            lines.push(base_line(&format!("before{}", i)));
        }

        // Add a line with inline spans (merged modification)
        // This simulates: "commercial_renewal.name" -> "bond.name"
        let mut merged_line = DiffLine::new(
            LineSource::Base,  // Source is Base for merged lines
            "bond.name".to_string(),
            ' ',
            Some(21),
        );
        merged_line.inline_spans = vec![
            InlineSpan {
                text: "commercial_renewal".to_string(),
                source: Some(LineSource::DeletedBase),
                is_deletion: true,
            },
            InlineSpan {
                text: "bond".to_string(),
                source: Some(LineSource::Committed),
                is_deletion: false,
            },
            InlineSpan {
                text: ".name".to_string(),
                source: None,
                is_deletion: false,
            },
        ];
        lines.push(merged_line);

        // Add many base lines after
        for i in 0..20 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = create_test_app(lines);
        app.context_only = true;

        // Get the filtered lines in context mode
        let filtered = app.build_context_lines();

        // The line with inline spans should be visible
        let has_merged_line = filtered.iter().any(|l| l.content == "bond.name");
        assert!(has_merged_line,
            "Line with inline spans should be visible in context view. \
             Filtered lines: {:?}",
            filtered.iter().map(|l| &l.content).collect::<Vec<_>>());

        // Should also have context lines around it (not just the change)
        assert!(filtered.len() > 1,
            "Should have context lines around the merged line");
    }

    #[test]
    fn test_context_view_shows_trailing_base_lines_after_change() {
        // REGRESSION TEST: Trailing base lines after a change should be visible
        // in context view as trailing context.
        //
        // Scenario:
        // - Many base lines before
        // - A committed change (addition)
        // - 2 base lines after (end, end)
        //
        // The trailing base lines should appear as context.

        let mut lines = Vec::new();

        // Add many base lines before
        for i in 0..20 {
            lines.push(base_line(&format!("before{}", i)));
        }

        // Add a committed change
        lines.push(DiffLine::new(
            LineSource::Committed,
            "new_line".to_string(),
            '+',
            Some(21),
        ));

        // Add trailing base lines (these should show as context)
        lines.push(base_line("end"));
        lines.push(base_line("end"));

        let mut app = create_test_app(lines);
        app.context_only = true;

        let filtered = app.build_context_lines();

        eprintln!("\n=== Context mode trailing lines test ===");
        eprintln!("Filtered lines ({}):", filtered.len());
        for (i, line) in filtered.iter().enumerate() {
            eprintln!("  [{}] {} {:?} '{}'", i, line.prefix, line.source, line.content);
        }

        // The change should be visible
        let has_change = filtered.iter().any(|l| l.content == "new_line");
        assert!(has_change, "The change should be visible");

        // The trailing "end" lines should be visible as context
        let trailing_ends = filtered.iter()
            .filter(|l| l.content == "end" && l.source == LineSource::Base)
            .count();
        assert_eq!(trailing_ends, 2,
            "Both trailing 'end' lines should be visible. Found {} of 2. \
             Filtered: {:?}",
            trailing_ends,
            filtered.iter().map(|l| &l.content).collect::<Vec<_>>());
    }

    #[test]
    fn test_context_mode_last_file_trailing_lines() {
        // REGRESSION TEST: This simulates the exact scenario from the bug:
        // Multiple files, and the LAST file has trailing lines after additions.
        //
        // File structure (simulated):
        // - File 1: some content (file header + lines)
        // - Empty separator line
        // - File 2 (last): base lines, then committed additions, then base trailing lines
        //
        // In context mode, the trailing base lines of the last file should be visible.

        let mut lines = Vec::new();

        // ===== FILE 1 =====
        lines.push(DiffLine::file_header("file1.rb"));
        for i in 0..10 {
            lines.push(base_line(&format!("file1_line{}", i)));
        }
        // One change in file 1
        lines.push(change_line("file1_change"));
        for i in 0..10 {
            lines.push(base_line(&format!("file1_after{}", i)));
        }
        // Empty separator between files
        lines.push(base_line(""));

        // ===== FILE 2 (last file) =====
        lines.push(DiffLine::file_header("file2.rb"));
        // Many base lines
        for i in 0..50 {
            lines.push(base_line(&format!("file2_base{}", i)));
        }
        // Block of additions at position ~50
        lines.push(DiffLine::new(LineSource::Committed, "added_line_1".to_string(), '+', Some(51)));
        lines.push(DiffLine::new(LineSource::Committed, "added_line_2".to_string(), '+', Some(52)));
        lines.push(DiffLine::new(LineSource::Committed, "added_line_3".to_string(), '+', Some(53)));
        lines.push(DiffLine::new(LineSource::Committed, "  end".to_string(), '+', Some(54)));  // The "+ end" from bug
        // Trailing base lines
        lines.push(base_line("end"));   // These are the missing lines
        lines.push(base_line("end"));

        let mut app = create_test_app(lines);
        app.context_only = true;

        let filtered = app.build_context_lines();

        eprintln!("\n=== Multi-file trailing context test ===");
        eprintln!("Total lines: {}, Filtered: {}", app.lines.len(), filtered.len());
        eprintln!("Last 15 filtered lines:");
        for (i, line) in filtered.iter().rev().take(15).collect::<Vec<_>>().into_iter().rev().enumerate() {
            let idx = filtered.len().saturating_sub(15) + i;
            eprintln!("  [{}] {} {:?} '{}'", idx, line.prefix, line.source, line.content);
        }

        // The "+ end" line should be visible (it's Committed)
        let has_added_end = filtered.iter().any(|l| l.content == "  end" && l.source == LineSource::Committed);
        assert!(has_added_end, "The '+ end' addition should be visible");

        // The trailing base "end" lines should be visible as context
        let trailing_base_ends = filtered.iter()
            .filter(|l| l.content == "end" && l.source == LineSource::Base)
            .count();
        assert_eq!(trailing_base_ends, 2,
            "Both trailing base 'end' lines should be visible as context. Found {}",
            trailing_base_ends);
    }

    #[test]
    fn test_context_mode_scroll_to_bottom_shows_trailing() {
        // Test that scrolling to the bottom in context mode shows trailing lines

        let mut lines = Vec::new();

        // File header
        lines.push(DiffLine::file_header("test.rb"));

        // Many base lines at the start
        for i in 0..100 {
            lines.push(base_line(&format!("base_line_{}", i)));
        }

        // Some committed changes near the end
        for i in 0..5 {
            lines.push(DiffLine::new(
                LineSource::Committed,
                format!("added_{}", i),
                '+',
                Some(101 + i),
            ));
        }

        // Trailing base lines (like "end" "end")
        lines.push(base_line("trailing_1"));
        lines.push(base_line("trailing_2"));
        lines.push(base_line("trailing_3"));

        let mut app = create_test_app(lines);
        app.context_only = true;
        app.viewport_height = 20;

        // Scroll to bottom
        app.go_to_bottom();

        let visible = app.visible_lines();

        eprintln!("\n=== Scroll to bottom test ===");
        eprintln!("scroll_offset: {}", app.scroll_offset);
        eprintln!("Visible lines ({}):", visible.len());
        for (i, line) in visible.iter().enumerate() {
            eprintln!("  [{}] {} {:?} '{}'", i, line.prefix, line.source, line.content);
        }

        // The last visible line should be trailing_3
        let last_visible = visible.last().unwrap();
        assert_eq!(last_visible.content, "trailing_3",
            "Last visible line should be 'trailing_3', got '{}'", last_visible.content);

        // All three trailing lines should be visible
        let has_trailing_1 = visible.iter().any(|l| l.content == "trailing_1");
        let has_trailing_2 = visible.iter().any(|l| l.content == "trailing_2");
        let has_trailing_3 = visible.iter().any(|l| l.content == "trailing_3");

        assert!(has_trailing_1, "trailing_1 should be visible when scrolled to bottom");
        assert!(has_trailing_2, "trailing_2 should be visible when scrolled to bottom");
        assert!(has_trailing_3, "trailing_3 should be visible when scrolled to bottom");
    }

    #[test]
    fn test_context_mode_large_file_scroll_to_bottom() {
        // Test with multiple change regions so context mode has more lines than viewport

        let mut lines = Vec::new();

        // File header
        lines.push(DiffLine::file_header("test.rb"));

        // Create several change regions spread throughout the file
        // Each region: base lines, then changes, then more base lines

        // Region 1 at the start
        for i in 0..10 { lines.push(base_line(&format!("region1_base_{}", i))); }
        for i in 0..3 { lines.push(change_line(&format!("region1_change_{}", i))); }

        // Large gap of base lines
        for i in 0..50 { lines.push(base_line(&format!("gap1_base_{}", i))); }

        // Region 2 in the middle
        for i in 0..3 { lines.push(change_line(&format!("region2_change_{}", i))); }
        for i in 0..20 { lines.push(base_line(&format!("region2_after_{}", i))); }

        // Large gap of base lines
        for i in 0..50 { lines.push(base_line(&format!("gap2_base_{}", i))); }

        // Region 3 near the end (the one we care about)
        for i in 0..5 { lines.push(change_line(&format!("region3_change_{}", i))); }

        // Trailing base lines
        lines.push(base_line("final_end_1"));
        lines.push(base_line("final_end_2"));

        let mut app = create_test_app(lines);
        app.context_only = true;
        app.viewport_height = 15; // Small viewport so we need to scroll

        let all_displayable = app.displayable_lines();
        eprintln!("\n=== Large file scroll test ===");
        eprintln!("Total displayable lines in context mode: {}", all_displayable.len());
        eprintln!("Viewport height: {}", app.viewport_height);

        // Print all displayable lines
        eprintln!("All displayable lines:");
        for (i, line) in all_displayable.iter().enumerate() {
            eprintln!("  [{}] {} {:?} '{}'", i, line.prefix, line.source, line.content);
        }

        // Scroll to bottom
        app.go_to_bottom();
        eprintln!("\nAfter go_to_bottom:");
        eprintln!("  scroll_offset: {}", app.scroll_offset);

        let visible = app.visible_lines();
        eprintln!("Visible lines after scroll ({}):", visible.len());
        for (i, line) in visible.iter().enumerate() {
            eprintln!("  [{}] {} {:?} '{}'", i, line.prefix, line.source, line.content);
        }

        // The trailing lines should be visible when scrolled to bottom
        let has_final_1 = visible.iter().any(|l| l.content == "final_end_1");
        let has_final_2 = visible.iter().any(|l| l.content == "final_end_2");

        assert!(has_final_1, "final_end_1 should be visible at bottom");
        assert!(has_final_2, "final_end_2 should be visible at bottom");

        // Also check that the last line in displayable_lines is final_end_2
        let last_displayable = all_displayable.last().unwrap();
        assert_eq!(last_displayable.content, "final_end_2",
            "Last displayable line should be final_end_2, got '{}'", last_displayable.content);
    }

    #[test]
    fn test_bug_scenario_multiple_files_last_file_trailing() {
        // This test simulates the EXACT scenario from the bug:
        // - 4 files total
        // - The LAST file has additions near the end
        // - The additions end with a Committed "end"
        // - Then 2 more Base "end" lines
        //
        // In context mode, after pressing G (go to bottom), we should see
        // ALL lines including the trailing Base "end" lines.

        use crate::diff::{DiffLine, LineSource};

        let mut lines = Vec::new();

        // ===== FILES 1-3 (with changes, to simulate "4 files") =====
        for file_num in 1..=3 {
            lines.push(DiffLine::file_header(&format!("file{}.rb", file_num)));
            for i in 0..20 {
                lines.push(DiffLine::new(LineSource::Base, format!("file{}_line{}", file_num, i), ' ', Some(i+1)));
            }
            // A change in each file
            lines.push(DiffLine::new(LineSource::Committed, format!("file{}_change", file_num), '+', Some(21)));
            for i in 0..10 {
                lines.push(DiffLine::new(LineSource::Base, format!("file{}_after{}", file_num, i), ' ', Some(22+i)));
            }
            // Separator
            lines.push(DiffLine::new(LineSource::Base, "".to_string(), ' ', None));
        }

        // ===== FILE 4 (the one with trailing context issues) =====
        lines.push(DiffLine::file_header("premium_due_notice_spec.rb"));

        // Many base lines (simulating lines 1-101)
        for i in 1..=101 {
            lines.push(DiffLine::new(
                LineSource::Base,
                format!("    it {{ spec line {} }}", i),
                ' ',
                Some(i),
            ));
        }

        // The added test block (lines 102-105)
        lines.push(DiffLine::new(LineSource::Committed, "".to_string(), '+', Some(102)));  // empty line
        lines.push(DiffLine::new(LineSource::Committed, "    it \"calculates total_due\" do".to_string(), '+', Some(103)));
        lines.push(DiffLine::new(LineSource::Committed, "      expect(letter.send(:total_due)).to eq(...)".to_string(), '+', Some(104)));
        lines.push(DiffLine::new(LineSource::Committed, "    end".to_string(), '+', Some(105)));  // THIS IS THE + end

        // Trailing base lines (lines 106-107) - THESE ARE MISSING IN THE BUG
        lines.push(DiffLine::new(LineSource::Base, "  end".to_string(), ' ', Some(106)));
        lines.push(DiffLine::new(LineSource::Base, "end".to_string(), ' ', Some(107)));

        let mut app = create_test_app(lines.clone());
        app.context_only = true;
        app.viewport_height = 20;

        // Get ALL displayable lines
        let all_displayable = app.displayable_lines();

        eprintln!("\n=== Bug scenario multi-file test ===");
        eprintln!("Total original lines: {}", lines.len());
        eprintln!("Total displayable in context mode: {}", all_displayable.len());

        // Print the LAST 20 displayable lines
        eprintln!("\nLast 20 displayable lines:");
        let start_idx = all_displayable.len().saturating_sub(20);
        for (i, line) in all_displayable.iter().skip(start_idx).enumerate() {
            eprintln!("  [{}] {} {:?} '{}'", start_idx + i, line.prefix, line.source, line.content);
        }

        // Now scroll to bottom
        app.go_to_bottom();
        let visible = app.visible_lines();

        eprintln!("\nAfter go_to_bottom (scroll_offset={}):", app.scroll_offset);
        eprintln!("Visible lines:");
        for (i, line) in visible.iter().enumerate() {
            eprintln!("  [{}] {} {:?} '{}'", i, line.prefix, line.source, line.content);
        }

        // CRITICAL ASSERTIONS:
        // 1. The "    end" (Committed) should be in displayable_lines
        let has_committed_end = all_displayable.iter().any(|l| l.content == "    end" && l.source == LineSource::Committed);
        assert!(has_committed_end, "Should have Committed '    end' in displayable lines");

        // 2. The "  end" (Base) should be in displayable_lines
        let has_base_end_indented = all_displayable.iter().any(|l| l.content == "  end" && l.source == LineSource::Base);
        assert!(has_base_end_indented, "Should have Base '  end' in displayable lines");

        // 3. The "end" (Base) should be in displayable_lines
        let has_base_end = all_displayable.iter().any(|l| l.content == "end" && l.source == LineSource::Base);
        assert!(has_base_end, "Should have Base 'end' in displayable lines");

        // 4. When scrolled to bottom, the last visible line should be "end" (Base)
        let last_visible = visible.last().unwrap();
        assert_eq!(last_visible.content, "end", "Last visible should be 'end'");
        assert_eq!(last_visible.source, LineSource::Base, "Last visible should be Base");
    }

    #[test]
    #[ignore]  // Run with: cargo test test_real_mbc_repo -- --ignored --nocapture
    fn test_real_mbc_repo() {
        // Integration test against real MBC repo
        let repo_path = std::path::PathBuf::from("/Users/michaelhopkins/projects/merchantsbonding/mbc");
        if !repo_path.exists() {
            eprintln!("Skipping: MBC repo not found at {:?}", repo_path);
            return;
        }

        let repo_root = match crate::git::get_repo_root(&repo_path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Skipping: Not a git repo: {:?}", e);
                return;
            }
        };

        let app = match App::new(repo_root) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("Skipping: Failed to create app: {:?}", e);
                return;
            }
        };

        eprintln!("\n=== REAL MBC REPO DEBUG ===");
        eprintln!("Total RAW lines: {}", app.lines.len());
        eprintln!("Context only (default): {}", app.context_only);

        // Find lines related to premium_due_notice_spec.rb
        eprintln!("\nSearching for premium_due_notice lines...");
        let mut found_file = false;
        let mut in_file = false;
        let mut file_lines = Vec::new();

        for (i, line) in app.lines.iter().enumerate() {
            if line.source == LineSource::FileHeader && line.content.contains("premium_due_notice") {
                found_file = true;
                in_file = true;
                eprintln!("Found file at line {}: {:?}", i, line.content);
            }
            if in_file {
                file_lines.push((i, line.clone()));
                if line.source == LineSource::FileHeader && !line.content.contains("premium_due_notice") {
                    in_file = false;
                }
            }
        }

        if found_file {
            eprintln!("\nLines in/around premium_due_notice_spec.rb ({} total):", file_lines.len());
            let start = file_lines.len().saturating_sub(20);
            for (orig_idx, line) in file_lines.iter().skip(start) {
                eprintln!("  raw[{}] {} {:?} num={:?} '{}'",
                    orig_idx, line.prefix, line.source, line.line_number,
                    if line.content.len() > 60 { &line.content[..60] } else { &line.content });
            }
        } else {
            eprintln!("premium_due_notice_spec.rb NOT found in diff!");
        }

        // Now check displayable lines
        let displayable = app.displayable_lines();
        eprintln!("\nTotal DISPLAYABLE lines: {}", displayable.len());

        eprintln!("\nLast 20 DISPLAYABLE lines:");
        let start = displayable.len().saturating_sub(20);
        for (i, line) in displayable.iter().skip(start).enumerate() {
            eprintln!("  disp[{}] {} {:?} num={:?} '{}'",
                start + i, line.prefix, line.source, line.line_number,
                if line.content.len() > 60 { &line.content[..60] } else { &line.content });
        }

        // The test: last line should be "end"
        let last = displayable.last().expect("Should have displayable lines");
        eprintln!("\nLast displayable line: {:?} '{}'", last.source, last.content);

        // Simulate viewport and scrolling
        let mut app_mut = app;
        app_mut.viewport_height = 20; // Simulated viewport

        eprintln!("\nScroll test with viewport_height=20:");
        eprintln!("  displayable_line_count: {}", displayable.len());
        app_mut.go_to_bottom();
        eprintln!("  After go_to_bottom, scroll_offset: {}", app_mut.scroll_offset);

        let visible = app_mut.visible_lines();
        eprintln!("  visible_lines count: {}", visible.len());
        eprintln!("  Visible lines:");
        for (i, line) in visible.iter().enumerate() {
            eprintln!("    [{}] {} {:?} num={:?} '{}'",
                i, line.prefix, line.source, line.line_number,
                if line.content.len() > 50 { &line.content[..50] } else { &line.content });
        }

        eprintln!("\n=== END REAL MBC REPO DEBUG ===");
    }
}
