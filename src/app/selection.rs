use anyhow::Result;
use arboard::Clipboard;

use super::{App, DisplayableItem};
use crate::diff::LineSource;
use crate::ui::ScreenRowInfo;

/// Get substring by character positions (not byte positions)
fn char_slice(s: &str, start: usize, end: usize) -> &str {
    let mut char_indices = s.char_indices();
    let start_byte = char_indices.nth(start).map(|(i, _)| i).unwrap_or(s.len());
    let end_byte = if end <= start {
        start_byte
    } else {
        char_indices
            .nth(end - start - 1)
            .map(|(i, _)| i)
            .unwrap_or(s.len())
    };
    &s[start_byte..end_byte]
}

/// Get substring from character position to end
fn char_slice_from(s: &str, start: usize) -> &str {
    let start_byte = s.char_indices().nth(start).map(|(i, _)| i).unwrap_or(s.len());
    &s[start_byte..]
}

/// Get substring from start to character position
fn char_slice_to(s: &str, end: usize) -> &str {
    let end_byte = s.char_indices().nth(end).map(|(i, _)| i).unwrap_or(s.len());
    &s[..end_byte]
}

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

impl App {
    /// Set the row map (called during rendering)
    pub fn set_row_map(&mut self, row_map: Vec<ScreenRowInfo>) {
        self.row_map = row_map;
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
        if let Some(ref mut sel) = self.selection
            && sel.active
            && let Some(p) = pos
        {
            sel.end = p;
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

    /// Check if there's an active selection
    pub fn has_selection(&self) -> bool {
        self.selection.is_some()
    }

    /// Convert screen coordinates to content position
    /// Now uses row_map to correctly handle wrapped lines and split inline diffs
    fn screen_to_content_position(&self, screen_x: u16, screen_y: u16) -> Option<Position> {
        let (offset_x, offset_y) = self.content_offset;

        // Check if within content area
        if screen_x < offset_x || screen_y < offset_y {
            return None;
        }

        let content_x = (screen_x - offset_x) as usize;
        let content_y = (screen_y - offset_y) as usize;

        // Use row_map to find the correct screen row
        // row_map is indexed by screen row, so content_y is the index
        // The row field in Position now refers to screen row, not logical line
        // This allows selection to work correctly with wrapped/split lines
        Some(Position {
            row: content_y,
            col: content_x,
        })
    }

    /// Get selected text (content only, without line numbers or prefixes)
    /// Now uses row_map to correctly handle wrapped lines and split inline diffs
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

        // Selection row/col now refer to screen rows, not logical lines
        // Use row_map to get the actual content for each screen row
        let mut result = String::new();

        // Calculate the prefix length to skip (line number + prefix char + spaces)
        // Format: "{line_num:>width} {prefix} {content}"
        let prefix_len = self.line_num_width + 3; // width + space + prefix + space

        for screen_row in start.row..=end.row {
            if screen_row >= self.row_map.len() {
                break;
            }

            let row_info = &self.row_map[screen_row];

            // Get content from the row_map (already has the correct content for this screen row)
            let content = &row_info.content;

            let char_count = content.chars().count();

            if start.row == end.row {
                // Single row selection
                let start_in_content = start.col.saturating_sub(prefix_len);
                let end_in_content = end.col.saturating_sub(prefix_len);
                if start_in_content < char_count {
                    let actual_end = end_in_content.min(char_count);
                    if actual_end > start_in_content {
                        result.push_str(char_slice(content, start_in_content, actual_end));
                    }
                }
            } else if screen_row == start.row {
                // First row of multi-row selection
                let start_in_content = start.col.saturating_sub(prefix_len);
                if start_in_content < char_count {
                    result.push_str(char_slice_from(content, start_in_content));
                }
                // Only add newline if next row is a new logical line (not a wrapped continuation)
                if screen_row + 1 < self.row_map.len()
                    && !self.row_map[screen_row + 1].is_continuation
                {
                    result.push('\n');
                }
            } else if screen_row == end.row {
                // Last row of multi-row selection
                let end_in_content = end.col.saturating_sub(prefix_len);
                let actual_end = end_in_content.min(char_count);
                result.push_str(char_slice_to(content, actual_end));
            } else {
                // Middle rows - take entire content
                result.push_str(content);
                // Only add newline if next row is a new logical line (not a wrapped continuation)
                if screen_row + 1 < self.row_map.len()
                    && !self.row_map[screen_row + 1].is_continuation
                {
                    result.push('\n');
                }
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

    /// Copy current file path to clipboard
    pub fn copy_current_path(&mut self) -> Result<bool> {
        if let Some(path) = self.current_file() {
            let mut clipboard = Clipboard::new()
                .map_err(|e| anyhow::anyhow!("Failed to access clipboard: {}", e))?;
            clipboard.set_text(path)
                .map_err(|e| anyhow::anyhow!("Failed to copy to clipboard: {}", e))?;
            self.path_copied_at = Some(std::time::Instant::now());
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Copy entire diff to clipboard (respects view mode and collapsed files)
    pub fn copy_diff(&mut self) -> Result<bool> {
        let text = self.format_diff_for_copy();
        if text.is_empty() {
            return Ok(false);
        }

        let mut clipboard = Clipboard::new()
            .map_err(|e| anyhow::anyhow!("Failed to access clipboard: {}", e))?;
        clipboard.set_text(text)
            .map_err(|e| anyhow::anyhow!("Failed to copy to clipboard: {}", e))?;
        self.path_copied_at = Some(std::time::Instant::now());
        Ok(true)
    }

    /// Format the diff for copying to clipboard
    pub(crate) fn format_diff_for_copy(&self) -> String {
        let items = self.compute_displayable_items();
        if items.is_empty() {
            return String::new();
        }

        // Calculate max line number width
        let max_line_num = items
            .iter()
            .filter_map(|item| {
                if let DisplayableItem::Line(idx) = item {
                    self.lines[*idx].line_number
                } else {
                    None
                }
            })
            .max()
            .unwrap_or(0);
        let line_num_width = if max_line_num > 0 {
            max_line_num.to_string().len()
        } else {
            0
        };

        let mut result = String::new();

        for item in &items {
            match item {
                DisplayableItem::Elided(count) => {
                    let padding = if line_num_width > 0 {
                        " ".repeat(line_num_width + 1)
                    } else {
                        String::new()
                    };
                    result.push_str(&format!("{}... {} lines hidden ...\n", padding, count));
                }
                DisplayableItem::Line(idx) => {
                    let line = &self.lines[*idx];

                    // Format line number
                    let line_num_str = if let Some(num) = line.line_number {
                        format!("{:>width$} ", num, width = line_num_width)
                    } else if line_num_width > 0 {
                        " ".repeat(line_num_width + 1)
                    } else {
                        String::new()
                    };

                    if line.source == LineSource::FileHeader {
                        result.push_str(&format!("{}── {} ──\n", line_num_str, line.content));
                    } else {
                        result.push_str(&format!(
                            "{}{} {}\n",
                            line_num_str, line.prefix, line.content
                        ));
                    }
                }
            }
        }

        result
    }

    /// Check if the "copied" flash should be shown (within 800ms of copy)
    pub fn should_show_copied_flash(&self) -> bool {
        if let Some(copied_at) = self.path_copied_at {
            copied_at.elapsed() < std::time::Duration::from_millis(800)
        } else {
            false
        }
    }

    /// Check if a screen position is on a file header, and return the file path if so
    pub fn get_file_header_at(&self, screen_x: u16, screen_y: u16) -> Option<String> {
        let (offset_x, offset_y) = self.content_offset;

        // Check if within content area
        if screen_x < offset_x || screen_y < offset_y {
            return None;
        }

        let content_y = (screen_y - offset_y) as usize;

        // Look up in row_map
        if content_y < self.row_map.len() {
            let row_info = &self.row_map[content_y];
            if row_info.is_file_header {
                return row_info.file_path.clone();
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestAppBuilder;
    use crate::ui::ScreenRowInfo;

    fn make_row(content: &str, is_continuation: bool) -> ScreenRowInfo {
        ScreenRowInfo {
            content: content.to_string(),
            is_file_header: false,
            file_path: None,
            is_continuation,
        }
    }

    #[test]
    fn test_get_selected_text_unwrapped_lines() {
        // Two separate logical lines (no wrapping)
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.row_map = vec![
            make_row("line one", false),
            make_row("line two", false),
        ];
        app.selection = Some(Selection {
            start: Position { row: 0, col: 6 }, // After prefix "123 + "
            end: Position { row: 1, col: 14 },  // Include "line two"
            active: false,
        });

        let text = app.get_selected_text().unwrap();
        assert_eq!(text, "line one\nline two");
    }

    #[test]
    fn test_get_selected_text_wrapped_line_no_extra_newlines() {
        // One logical line wrapped across two screen rows
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.row_map = vec![
            make_row("first part ", false), // Start of logical line
            make_row("second part", true),  // Continuation (wrapped)
        ];
        app.selection = Some(Selection {
            start: Position { row: 0, col: 6 },
            end: Position { row: 1, col: 17 },
            active: false,
        });

        let text = app.get_selected_text().unwrap();
        // Should NOT have newline between wrapped parts
        assert_eq!(text, "first part second part");
    }

    #[test]
    fn test_get_selected_text_mixed_wrapped_and_unwrapped() {
        // Two logical lines, first one wraps
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.row_map = vec![
            make_row("wrapped ", false),    // Line 1, part 1
            make_row("line", true),         // Line 1, part 2 (continuation)
            make_row("normal line", false), // Line 2 (new logical line)
        ];
        app.selection = Some(Selection {
            start: Position { row: 0, col: 6 },
            end: Position { row: 2, col: 17 },
            active: false,
        });

        let text = app.get_selected_text().unwrap();
        // Newline only between logical lines, not within wrapped line
        assert_eq!(text, "wrapped line\nnormal line");
    }

    #[test]
    fn test_get_selected_text_starting_on_continuation() {
        // Selection starts on a continuation row
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.row_map = vec![
            make_row("first ", false),       // Line 1, part 1
            make_row("second", true),        // Line 1, part 2 (continuation)
            make_row("next line", false),    // Line 2 (new logical line)
        ];
        // Start selection on the continuation row
        app.selection = Some(Selection {
            start: Position { row: 1, col: 6 },
            end: Position { row: 2, col: 15 },
            active: false,
        });

        let text = app.get_selected_text().unwrap();
        // Should get content from continuation + newline + next line
        assert_eq!(text, "second\nnext line");
    }
}
