use anyhow::Result;
use arboard::Clipboard;

use super::{App, DisplayableItem};
use crate::diff::LineSource;
use crate::patch;
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

/// Check if a character is a word character (alphanumeric or underscore)
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Check if a character is a symbol (non-word, non-whitespace)
fn is_symbol_char(c: char) -> bool {
    !is_word_char(c) && !c.is_whitespace()
}

/// Find selection boundaries around a given column position.
/// Returns (start_col, end_col) where end_col is exclusive.
///
/// Behavior:
/// - On a word character: select the word
/// - On a symbol character: select consecutive symbols
/// - On whitespace: select the first word/symbol to the right
/// - Past end of line: select entire line (handled by caller)
fn find_selection_boundaries(s: &str, col: usize) -> Option<(usize, usize)> {
    let chars: Vec<char> = s.chars().collect();

    if col >= chars.len() {
        return None;
    }

    let c = chars[col];

    if is_word_char(c) {
        // Select word
        find_word_boundaries_impl(&chars, col)
    } else if is_symbol_char(c) {
        // Select consecutive symbols
        find_symbol_boundaries_impl(&chars, col)
    } else {
        // Whitespace: find first non-whitespace to the right
        let mut start = col;
        while start < chars.len() && chars[start].is_whitespace() {
            start += 1;
        }
        if start >= chars.len() {
            return None;
        }
        if is_word_char(chars[start]) {
            find_word_boundaries_impl(&chars, start)
        } else {
            find_symbol_boundaries_impl(&chars, start)
        }
    }
}

fn find_word_boundaries_impl(chars: &[char], col: usize) -> Option<(usize, usize)> {
    if col >= chars.len() || !is_word_char(chars[col]) {
        return None;
    }

    let mut start = col;
    while start > 0 && is_word_char(chars[start - 1]) {
        start -= 1;
    }

    let mut end = col;
    while end < chars.len() && is_word_char(chars[end]) {
        end += 1;
    }

    Some((start, end))
}

fn find_symbol_boundaries_impl(chars: &[char], col: usize) -> Option<(usize, usize)> {
    if col >= chars.len() || !is_symbol_char(chars[col]) {
        return None;
    }

    let mut start = col;
    while start > 0 && is_symbol_char(chars[start - 1]) {
        start -= 1;
    }

    let mut end = col;
    while end < chars.len() && is_symbol_char(chars[end]) {
        end += 1;
    }

    Some((start, end))
}

/// Backward-compatible wrapper for tests that specifically test word boundaries
#[cfg(test)]
fn find_word_boundaries(s: &str, col: usize) -> Option<(usize, usize)> {
    let chars: Vec<char> = s.chars().collect();
    find_word_boundaries_impl(&chars, col)
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
        self.word_selection_anchor = None; // Clear word mode
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
        let pos = match self.screen_to_content_position(screen_x, screen_y) {
            Some(p) => p,
            None => return,
        };

        let Some(ref mut sel) = self.selection else {
            return;
        };
        if !sel.active {
            return;
        }

        // If in word/symbol selection mode, snap to boundaries
        if let Some((anchor_row, anchor_start, anchor_end)) = self.word_selection_anchor {
            let prefix_len = self.line_num_width + 3;

            // Get selection boundaries at current position
            let (drag_start, drag_end) = if pos.row < self.row_map.len() {
                let content = &self.row_map[pos.row].content;
                let content_col = pos.col.saturating_sub(prefix_len);
                find_selection_boundaries(content, content_col)
                    .map(|(s, e)| (s + prefix_len, e + prefix_len))
                    .unwrap_or((pos.col, pos.col))
            } else {
                (pos.col, pos.col)
            };

            // Extend selection to encompass both anchor word and current word
            if pos.row < anchor_row || (pos.row == anchor_row && drag_start < anchor_start) {
                // Dragging before anchor - selection goes from drag_start to anchor_end
                sel.start = Position {
                    row: pos.row,
                    col: drag_start,
                };
                sel.end = Position {
                    row: anchor_row,
                    col: anchor_end,
                };
            } else {
                // Dragging after anchor - selection goes from anchor_start to drag_end
                sel.start = Position {
                    row: anchor_row,
                    col: anchor_start,
                };
                sel.end = Position {
                    row: pos.row,
                    col: drag_end,
                };
            }
        } else {
            // Normal character-based selection
            sel.end = pos;
        }
    }

    /// End selection (mouse released)
    pub fn end_selection(&mut self) {
        self.word_selection_anchor = None; // Clear word mode
        if let Some(ref mut sel) = self.selection {
            sel.active = false;
        }
    }

    /// Select the word at the given screen coordinates (for double-click)
    /// If clicking past end of line, selects the entire logical line (including wrapped segments)
    pub fn select_word_at(&mut self, screen_x: u16, screen_y: u16) {
        let pos = match self.screen_to_content_position(screen_x, screen_y) {
            Some(p) => p,
            None => return,
        };

        if pos.row >= self.row_map.len() {
            return;
        }

        let content = &self.row_map[pos.row].content;
        let prefix_len = self.line_num_width + 3; // line_num + space + prefix + space
        let content_col = pos.col.saturating_sub(prefix_len);
        let content_len = content.chars().count();

        // Determine selection bounds
        if content_col >= content_len {
            // Clicking past end of line - select entire logical line (including wrapped segments)
            self.select_logical_line(pos.row, prefix_len);
        } else if let Some((start, end)) = find_selection_boundaries(content, content_col) {
            let sel_start = start + prefix_len;
            let sel_end = end + prefix_len;

            // Set anchor for word-drag mode
            self.word_selection_anchor = Some((pos.row, sel_start, sel_end));

            self.selection = Some(Selection {
                start: Position {
                    row: pos.row,
                    col: sel_start,
                },
                end: Position {
                    row: pos.row,
                    col: sel_end,
                },
                active: true, // Allow dragging to extend
            });
        }
        // else: nothing to select (shouldn't happen with current logic)
    }

    /// Select an entire logical line, including all wrapped segments
    fn select_logical_line(&mut self, screen_row: usize, prefix_len: usize) {
        // Find the start of the logical line (go backwards while is_continuation)
        let mut start_row = screen_row;
        while start_row > 0 && self.row_map[start_row].is_continuation {
            start_row -= 1;
        }

        // Find the end of the logical line (go forward while next row is_continuation)
        let mut end_row = screen_row;
        while end_row + 1 < self.row_map.len() && self.row_map[end_row + 1].is_continuation {
            end_row += 1;
        }

        // Calculate end column for the last row
        let end_content_len = self.row_map[end_row].content.chars().count();

        // Set anchor spanning the entire logical line
        let sel_start = prefix_len;
        let sel_end = end_content_len + prefix_len;
        self.word_selection_anchor = Some((start_row, sel_start, sel_end));

        self.selection = Some(Selection {
            start: Position {
                row: start_row,
                col: sel_start,
            },
            end: Position {
                row: end_row,
                col: sel_end,
            },
            active: true,
        });
    }

    /// Clear current selection
    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.word_selection_anchor = None;
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

    /// Copy git patch format to clipboard (for use with `git apply`)
    pub fn copy_patch(&mut self) -> Result<bool> {
        let text = patch::generate_patch(&self.lines);
        if text.is_empty() {
            return Ok(false);
        }

        let mut clipboard = Clipboard::new()
            .map_err(|e| anyhow::anyhow!("Failed to access clipboard: {}", e))?;
        clipboard
            .set_text(text)
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

    #[test]
    fn test_find_word_boundaries_simple() {
        assert_eq!(find_word_boundaries("hello world", 0), Some((0, 5)));
        assert_eq!(find_word_boundaries("hello world", 2), Some((0, 5)));
        assert_eq!(find_word_boundaries("hello world", 4), Some((0, 5)));
        assert_eq!(find_word_boundaries("hello world", 6), Some((6, 11)));
        assert_eq!(find_word_boundaries("hello world", 10), Some((6, 11)));
    }

    #[test]
    fn test_find_word_boundaries_with_underscores() {
        assert_eq!(find_word_boundaries("foo_bar_baz", 0), Some((0, 11)));
        assert_eq!(find_word_boundaries("foo_bar_baz", 4), Some((0, 11)));
        assert_eq!(find_word_boundaries("snake_case_name", 6), Some((0, 15)));
    }

    #[test]
    fn test_find_word_boundaries_not_on_word() {
        // find_word_boundaries only finds words, not symbols or whitespace
        assert_eq!(find_word_boundaries("hello world", 5), None); // space
        assert_eq!(find_word_boundaries("foo.bar", 3), None); // dot
        assert_eq!(find_word_boundaries("a + b", 2), None); // plus
    }

    #[test]
    fn test_find_selection_boundaries_symbols() {
        // Triple slash should be selected as a group
        assert_eq!(find_selection_boundaries("/// comment", 0), Some((0, 3)));
        assert_eq!(find_selection_boundaries("/// comment", 1), Some((0, 3)));
        assert_eq!(find_selection_boundaries("/// comment", 2), Some((0, 3)));

        // Double colon
        assert_eq!(find_selection_boundaries("std::vec", 3), Some((3, 5)));
        assert_eq!(find_selection_boundaries("std::vec", 4), Some((3, 5)));

        // Arrow operator
        assert_eq!(find_selection_boundaries("foo->bar", 3), Some((3, 5)));

        // Mixed symbols
        assert_eq!(find_selection_boundaries("a := b", 2), Some((2, 4)));
    }

    #[test]
    fn test_find_selection_boundaries_whitespace_selects_next() {
        // Whitespace should select the word to the right
        assert_eq!(find_selection_boundaries("hello world", 5), Some((6, 11)));
        assert_eq!(find_selection_boundaries("  word", 0), Some((2, 6)));
        assert_eq!(find_selection_boundaries("  word", 1), Some((2, 6)));

        // Whitespace before symbols should select the symbols
        assert_eq!(find_selection_boundaries("foo /// bar", 4), Some((4, 7)));

        // Trailing whitespace should return None
        assert_eq!(find_selection_boundaries("word  ", 5), None);
    }

    #[test]
    fn test_find_selection_boundaries_words() {
        // Words still work as before
        assert_eq!(find_selection_boundaries("hello world", 0), Some((0, 5)));
        assert_eq!(find_selection_boundaries("hello world", 6), Some((6, 11)));
        assert_eq!(find_selection_boundaries("foo_bar", 3), Some((0, 7)));
    }

    #[test]
    fn test_find_word_boundaries_at_edges() {
        assert_eq!(find_word_boundaries("word", 0), Some((0, 4)));
        assert_eq!(find_word_boundaries("word", 3), Some((0, 4)));
        assert_eq!(find_word_boundaries("  word  ", 2), Some((2, 6)));
    }

    #[test]
    fn test_find_word_boundaries_empty_and_out_of_bounds() {
        assert_eq!(find_word_boundaries("", 0), None);
        assert_eq!(find_word_boundaries("hello", 10), None);
    }

    #[test]
    fn test_select_word_at_basic() {
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.content_offset = (1, 1);
        app.row_map = vec![make_row("hello world", false)];

        // Click on 'w' in "world" - content col 6, screen col = 6 + prefix_len(6) + offset(1) = 13
        // prefix_len = line_num_width(3) + 3 = 6
        app.select_word_at(13, 1);

        let sel = app.selection.as_ref().expect("Should have selection");
        // Word "world" at content cols 6-11, screen cols 12-17 (+ prefix_len 6)
        assert_eq!(sel.start.col, 12); // 6 + 6
        assert_eq!(sel.end.col, 17); // 11 + 6
        assert!(sel.active, "Should be active to allow word-drag");

        // Should have word anchor set for drag mode
        let anchor = app.word_selection_anchor.expect("Should have word anchor");
        assert_eq!(anchor, (0, 12, 17));
    }

    #[test]
    fn test_select_word_at_whitespace_selects_next_word() {
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.content_offset = (1, 1);
        app.row_map = vec![make_row("hello world", false)];

        // Click on space between words - content col 5, screen col = 5 + 6 + 1 = 12
        app.select_word_at(12, 1);

        // Should select "world" (the word to the right)
        let sel = app.selection.as_ref().expect("Should have selection");
        assert_eq!(sel.start.col, 12); // "world" at content col 6 + prefix 6
        assert_eq!(sel.end.col, 17); // ends at content col 11 + prefix 6
    }

    #[test]
    fn test_select_word_at_symbols() {
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.content_offset = (1, 1);
        app.row_map = vec![make_row("/// comment", false)];

        // Click on second slash - content col 1, screen col = 1 + 6 + 1 = 8
        app.select_word_at(8, 1);

        // Should select "///" (all three slashes)
        let sel = app.selection.as_ref().expect("Should have selection");
        assert_eq!(sel.start.col, 6); // starts at content col 0 + prefix 6
        assert_eq!(sel.end.col, 9); // ends at content col 3 + prefix 6
    }

    #[test]
    fn test_select_word_at_past_end_of_line() {
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.content_offset = (1, 1);
        app.row_map = vec![make_row("hello", false)]; // 5 chars

        // Click past end of line - content has 5 chars, click at col 10
        // screen_x = 10 + prefix(6) + offset(1) = 17
        app.select_word_at(17, 1);

        let sel = app.selection.as_ref().expect("Should select whole line");
        // Should select entire line: prefix_len to prefix_len + content_len
        assert_eq!(sel.start.col, 6); // prefix_len
        assert_eq!(sel.end.col, 11); // 5 + 6
    }

    #[test]
    fn test_word_drag_extends_by_words() {
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.content_offset = (1, 1);
        app.row_map = vec![make_row("one two three four", false)];

        // Double-click on "two" (content col 4-7)
        // screen_x for 't' in "two" = 4 + 6 + 1 = 11
        app.select_word_at(11, 1);

        let sel = app.selection.as_ref().unwrap();
        assert_eq!(sel.start.col, 10); // "two" starts at content col 4 + prefix 6
        assert_eq!(sel.end.col, 13); // "two" ends at content col 7 + prefix 6

        // Now drag to "four" (content col 14-18)
        // screen_x for 'f' in "four" = 14 + 6 + 1 = 21
        app.update_selection(21, 1);

        let sel = app.selection.as_ref().unwrap();
        // Should extend from "two" start to "four" end
        assert_eq!(sel.start.col, 10); // "two" starts at 4 + 6
        assert_eq!(sel.end.col, 24); // "four" ends at 18 + 6
    }

    #[test]
    fn test_word_drag_backwards() {
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.content_offset = (1, 1);
        app.row_map = vec![make_row("one two three four", false)];

        // Double-click on "three" (content col 8-13)
        // screen_x for 't' in "three" = 8 + 6 + 1 = 15
        app.select_word_at(15, 1);

        // Now drag backwards to "one" (content col 0-3)
        // screen_x for 'o' in "one" = 0 + 6 + 1 = 7
        app.update_selection(7, 1);

        let sel = app.selection.as_ref().unwrap();
        // Should extend from "one" start to "three" end
        assert_eq!(sel.start.col, 6); // "one" starts at 0 + 6
        assert_eq!(sel.end.col, 19); // "three" ends at 13 + 6
    }

    #[test]
    fn test_word_anchor_cleared_on_end_selection() {
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.content_offset = (1, 1);
        app.row_map = vec![make_row("hello world", false)];

        app.select_word_at(13, 1);
        assert!(app.word_selection_anchor.is_some());

        app.end_selection();
        assert!(app.word_selection_anchor.is_none());
    }

    #[test]
    fn test_word_anchor_cleared_on_start_selection() {
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.content_offset = (1, 1);
        app.row_map = vec![make_row("hello world", false)];

        app.select_word_at(13, 1);
        assert!(app.word_selection_anchor.is_some());

        // Normal click should clear word anchor
        app.start_selection(7, 1);
        assert!(app.word_selection_anchor.is_none());
    }

    #[test]
    fn test_select_word_at_empty_line() {
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.content_offset = (1, 1);
        app.row_map = vec![make_row("", false)]; // Empty line

        // Click on empty line (any position)
        app.select_word_at(10, 1);

        // Should create an empty selection (start == end at prefix boundary)
        let sel = app.selection.as_ref().expect("Should have selection");
        assert_eq!(sel.start.col, 6); // prefix_len
        assert_eq!(sel.end.col, 6); // prefix_len + 0 chars
    }

    #[test]
    fn test_word_drag_across_rows() {
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.content_offset = (1, 1);
        app.row_map = vec![
            make_row("first line", false),
            make_row("second line", false),
        ];

        // Double-click on "first" (content col 0-5)
        app.select_word_at(7, 1); // screen_x = 0 + 6 + 1 = 7

        let sel = app.selection.as_ref().unwrap();
        assert_eq!(sel.start.row, 0);
        assert_eq!(sel.end.row, 0);

        // Drag to "second" on row 1 (content col 0-6)
        app.update_selection(7, 2); // screen_y = 1 + 1 = 2

        let sel = app.selection.as_ref().unwrap();
        // Should extend from "first" start (row 0) to "second" end (row 1)
        assert_eq!(sel.start.row, 0);
        assert_eq!(sel.start.col, 6); // "first" at col 0 + prefix 6
        assert_eq!(sel.end.row, 1);
        assert_eq!(sel.end.col, 12); // "second" ends at col 6 + prefix 6
    }

    #[test]
    fn test_word_drag_to_whitespace_selects_next_word() {
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.content_offset = (1, 1);
        app.row_map = vec![make_row("one   two", false)]; // Multiple spaces

        // Double-click on "one"
        app.select_word_at(7, 1); // col 0 + prefix 6 + offset 1

        // Drag to whitespace (col 4, between words)
        // screen_x = 4 + 6 + 1 = 11
        app.update_selection(11, 1);

        let sel = app.selection.as_ref().unwrap();
        // Whitespace selects word to the right ("two" at cols 6-9)
        // So selection extends from "one" start to "two" end
        assert_eq!(sel.start.col, 6); // "one" starts at 0 + 6
        assert_eq!(sel.end.col, 15); // "two" ends at content col 9 + prefix 6
    }

    #[test]
    fn test_word_drag_to_trailing_whitespace() {
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.content_offset = (1, 1);
        app.row_map = vec![make_row("word   ", false)]; // Trailing spaces

        // Double-click on "word"
        app.select_word_at(7, 1);

        // Drag to trailing whitespace (col 5)
        // screen_x = 5 + 6 + 1 = 12
        app.update_selection(12, 1);

        let sel = app.selection.as_ref().unwrap();
        // No word to the right, so falls back to cursor position
        assert_eq!(sel.start.col, 6); // "word" starts at 0 + 6
        assert_eq!(sel.end.col, 11); // cursor at 5 + 6
    }

    #[test]
    fn test_select_past_eol_on_wrapped_line_selects_entire_logical_line() {
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.content_offset = (1, 1);
        // Simulate a wrapped line: first row is start, second is continuation
        app.row_map = vec![
            make_row("first part ", false),      // Start of logical line
            make_row("second part", true),       // Continuation (wrapped)
            make_row("next line", false),        // Different logical line
        ];

        // Click past end of the wrapped segment (row 1)
        // Content is "second part" (11 chars), click at col 15
        // screen_x = 15 + 6 + 1 = 22
        app.select_word_at(22, 2); // screen_y = 1 + 1 = 2 (row 1)

        let sel = app.selection.as_ref().expect("Should have selection");
        // Should select from start of logical line (row 0) to end of last segment (row 1)
        assert_eq!(sel.start.row, 0);
        assert_eq!(sel.start.col, 6); // prefix_len
        assert_eq!(sel.end.row, 1);
        assert_eq!(sel.end.col, 17); // "second part" is 11 chars + prefix 6
    }

    #[test]
    fn test_select_past_eol_on_first_segment_of_wrapped_line() {
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.content_offset = (1, 1);
        // Simulate a wrapped line spanning 3 rows
        app.row_map = vec![
            make_row("part one ", false),   // Start
            make_row("part two ", true),    // Continuation
            make_row("part three", true),   // Continuation
        ];

        // Click past end of the first segment (row 0)
        // screen_x = 12 + 6 + 1 = 19
        app.select_word_at(19, 1); // screen_y = 0 + 1 = 1 (row 0)

        let sel = app.selection.as_ref().expect("Should have selection");
        // Should select entire logical line from row 0 to row 2
        assert_eq!(sel.start.row, 0);
        assert_eq!(sel.start.col, 6);
        assert_eq!(sel.end.row, 2);
        assert_eq!(sel.end.col, 16); // "part three" is 10 chars + prefix 6
    }
}
