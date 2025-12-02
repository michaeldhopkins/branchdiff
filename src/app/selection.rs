use anyhow::Result;
use arboard::Clipboard;

use super::App;
use crate::ui::ScreenRowInfo;

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

            if start.row == end.row {
                // Single row selection
                let start_in_content = start.col.saturating_sub(prefix_len);
                let end_in_content = end.col.saturating_sub(prefix_len);
                if start_in_content < content.len() {
                    let actual_end = end_in_content.min(content.len());
                    if actual_end > start_in_content {
                        result.push_str(&content[start_in_content..actual_end]);
                    }
                }
            } else if screen_row == start.row {
                // First row of multi-row selection
                let start_in_content = start.col.saturating_sub(prefix_len);
                if start_in_content < content.len() {
                    result.push_str(&content[start_in_content..]);
                }
                result.push('\n');
            } else if screen_row == end.row {
                // Last row of multi-row selection
                let end_in_content = end.col.saturating_sub(prefix_len);
                let actual_end = end_in_content.min(content.len());
                result.push_str(&content[..actual_end]);
            } else {
                // Middle rows - take entire content
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
}
