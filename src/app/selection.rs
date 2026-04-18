use std::time::Instant;

use anyhow::Result;
use arboard::Clipboard;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{App, DisplayableItem};
use crate::diff::LineSource;
use crate::patch;
use crate::ui::{ScreenRowInfo, PREFIX_CHAR_WIDTH};

/// Multi-click detection window in milliseconds.
/// Shared between click detection and deferred copy timeout.
pub(crate) const MULTI_CLICK_MS: u128 = 500;

/// Get byte index at a display-width column boundary.
/// Returns `s.len()` if the column is at or past the end.
fn byte_at_display_col(s: &str, col: usize) -> usize {
    let mut w = 0;
    for (i, ch) in s.char_indices() {
        if w >= col {
            return i;
        }
        w += UnicodeWidthChar::width(ch).unwrap_or(0);
    }
    s.len()
}

/// Get substring by display-width column positions.
fn display_slice(s: &str, start: usize, end: usize) -> &str {
    let start_byte = byte_at_display_col(s, start);
    let end_byte = byte_at_display_col(s, end);
    &s[start_byte..end_byte]
}

/// Get substring from a display-width column to the end.
fn display_slice_from(s: &str, start: usize) -> &str {
    &s[byte_at_display_col(s, start)..]
}

/// Get substring from the start to a display-width column.
fn display_slice_to(s: &str, end: usize) -> &str {
    &s[..byte_at_display_col(s, end)]
}

/// Display width of a string (number of terminal columns).
fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Convert a display-width column to the corresponding character index.
/// Returns `chars.len()` if the column is past the end.
fn display_col_to_char_idx(chars: &[char], col: usize) -> usize {
    let mut w = 0;
    for (idx, &ch) in chars.iter().enumerate() {
        if w >= col {
            return idx;
        }
        w += UnicodeWidthChar::width(ch).unwrap_or(0);
    }
    chars.len()
}

/// Convert a character index to its display-width column.
fn char_idx_to_display_col(chars: &[char], idx: usize) -> usize {
    chars[..idx]
        .iter()
        .map(|c| UnicodeWidthChar::width(*c).unwrap_or(0))
        .sum()
}

/// Check if a character is a word character (alphanumeric or underscore)
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Check if a character is a symbol (non-word, non-whitespace)
fn is_symbol_char(c: char) -> bool {
    !is_word_char(c) && !c.is_whitespace()
}

/// Find selection boundaries around a display-width column.
/// Returns `(start_col, end_col)` in display-width columns where `end_col` is exclusive.
///
/// Behavior:
/// - On a word character: select the word
/// - On a symbol character: select consecutive symbols
/// - On whitespace: select the first word/symbol to the right
/// - Past end of line: select entire line (handled by caller)
fn find_selection_boundaries(s: &str, col: usize) -> Option<(usize, usize)> {
    let chars: Vec<char> = s.chars().collect();
    let char_idx = display_col_to_char_idx(&chars, col);

    if char_idx >= chars.len() {
        return None;
    }

    let c = chars[char_idx];

    let (start_char, end_char) = if is_word_char(c) {
        find_word_boundaries_impl(&chars, char_idx)?
    } else if is_symbol_char(c) {
        find_symbol_boundaries_impl(&chars, char_idx)?
    } else {
        // Whitespace: find first non-whitespace to the right
        let mut scan = char_idx;
        while scan < chars.len() && chars[scan].is_whitespace() {
            scan += 1;
        }
        if scan >= chars.len() {
            return None;
        }
        if is_word_char(chars[scan]) {
            find_word_boundaries_impl(&chars, scan)?
        } else {
            find_symbol_boundaries_impl(&chars, scan)?
        }
    };

    Some((
        char_idx_to_display_col(&chars, start_char),
        char_idx_to_display_col(&chars, end_char),
    ))
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
    /// Calculate the prefix length for selection operations.
    /// This matches the prefix_width calculation in diff_view.rs.
    fn prefix_len(&self) -> usize {
        if self.view.line_num_width > 0 {
            self.view.line_num_width + 1 + PREFIX_CHAR_WIDTH
        } else {
            PREFIX_CHAR_WIDTH
        }
    }

    /// Set the row map (called during rendering)
    pub fn set_row_map(&mut self, row_map: Vec<ScreenRowInfo>) {
        self.view.row_map = row_map;
    }

    /// Start a selection at the given screen coordinates
    pub fn start_selection(&mut self, screen_x: u16, screen_y: u16) {
        self.view.word_selection_anchor = None;
        self.view.line_selection_anchor = None;
        if let Some(pos) = self.screen_to_content_position(screen_x, screen_y) {
            self.view.selection = Some(Selection {
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

        // Early return if no active selection
        let is_active = self.view.selection.as_ref().is_some_and(|s| s.active);
        if !is_active {
            return;
        }

        let prefix_len = self.prefix_len();

        // Line selection mode (triple-click drag)
        if let Some((anchor_start_row, anchor_end_row)) = self.view.line_selection_anchor {
            // Find the logical line boundaries at current position
            let (drag_start_row, drag_end_row) = self.find_logical_line_bounds(pos.row);

            // Extend selection to encompass both anchor line and current line
            let (new_start, new_end) = if pos.row < anchor_start_row {
                // Dragging before anchor
                let end_content_len = self.view.row_map.get(anchor_end_row)
                    .map(|r| display_width(&r.content))
                    .unwrap_or(0);
                (
                    Position { row: drag_start_row, col: prefix_len },
                    Position { row: anchor_end_row, col: end_content_len + prefix_len },
                )
            } else {
                // Dragging after or at anchor
                let end_content_len = self.view.row_map.get(drag_end_row)
                    .map(|r| display_width(&r.content))
                    .unwrap_or(0);
                (
                    Position { row: anchor_start_row, col: prefix_len },
                    Position { row: drag_end_row, col: end_content_len + prefix_len },
                )
            };

            if let Some(ref mut sel) = self.view.selection {
                sel.start = new_start;
                sel.end = new_end;
            }
            return;
        }

        // Word/symbol selection mode (double-click drag)
        if let Some((anchor_row, anchor_start, anchor_end)) = self.view.word_selection_anchor {
            // Get selection boundaries at current position
            let (drag_start, drag_end) = if pos.row < self.view.row_map.len() {
                let content = &self.view.row_map[pos.row].content;
                let content_col = pos.col.saturating_sub(prefix_len);
                find_selection_boundaries(content, content_col)
                    .map(|(s, e)| (s + prefix_len, e + prefix_len))
                    .unwrap_or((pos.col, pos.col))
            } else {
                (pos.col, pos.col)
            };

            // Extend selection to encompass both anchor word and current word
            let (new_start, new_end) =
                if pos.row < anchor_row || (pos.row == anchor_row && drag_start < anchor_start) {
                    // Dragging before anchor - selection goes from drag_start to anchor_end
                    (
                        Position { row: pos.row, col: drag_start },
                        Position { row: anchor_row, col: anchor_end },
                    )
                } else {
                    // Dragging after anchor - selection goes from anchor_start to drag_end
                    (
                        Position { row: anchor_row, col: anchor_start },
                        Position { row: pos.row, col: drag_end },
                    )
                };

            if let Some(ref mut sel) = self.view.selection {
                sel.start = new_start;
                sel.end = new_end;
            }
        } else {
            // Normal character-based selection
            if let Some(ref mut sel) = self.view.selection {
                sel.end = pos;
            }
        }
    }

    /// End selection (mouse released)
    pub fn end_selection(&mut self) {
        self.view.word_selection_anchor = None;
        self.view.line_selection_anchor = None;
        if let Some(ref mut sel) = self.view.selection {
            sel.active = false;
        }
    }

    /// End selection and auto-copy based on how the selection was made.
    ///
    /// - Drag (last_click cleared): copy immediately if non-empty
    /// - Multi-click (count >= 2): defer copy until multi-click window expires
    /// - Single click without drag: no copy (nothing meaningful selected)
    pub fn end_selection_with_auto_copy(&mut self) {
        let click_info = self.view.last_click;
        self.end_selection();

        match click_info {
            // Drag happened (UpdateSelection clears last_click).
            // Copy immediately if there's a real selection.
            None if self.has_non_empty_selection() => {
                let _ = self.copy_selection();
            }
            // Multi-click (double/triple). Defer copy to allow higher click counts.
            Some((_, _, _, count)) if count >= 2 && self.has_non_empty_selection() => {
                self.view.pending_copy = Some(Instant::now());
            }
            _ => {
                // No drag, or single click without drag, or empty selection.
            }
        }
    }

    /// Cancel any pending deferred copy (called when a new click arrives).
    pub fn cancel_pending_copy(&mut self) {
        self.view.pending_copy = None;
    }

    /// Execute pending copy if the multi-click window has expired.
    /// Returns true if a copy was executed (caller should trigger redraw).
    pub fn check_and_execute_pending_copy(&mut self) -> bool {
        if let Some(pending_time) = self.view.pending_copy
            && pending_time.elapsed().as_millis() >= MULTI_CLICK_MS
        {
            self.view.pending_copy = None;
            let _ = self.copy_selection();
            return true;
        }
        false
    }

    /// Check if there's a selection with actual content (not just a point click).
    pub fn has_non_empty_selection(&self) -> bool {
        self.view.selection.as_ref().is_some_and(|sel| {
            sel.start.row != sel.end.row || sel.start.col != sel.end.col
        })
    }

    /// Select the word at the given screen coordinates (for double-click)
    /// If clicking past end of line, selects the entire logical line (including wrapped segments)
    pub fn select_word_at(&mut self, screen_x: u16, screen_y: u16) {
        let pos = match self.screen_to_content_position(screen_x, screen_y) {
            Some(p) => p,
            None => return,
        };

        let is_status_bar = pos.row >= self.view.row_map.len();

        let (content, prefix_len) = match self.row_content(pos.row) {
            Some(r) => (r.0.to_string(), r.1),
            None => return,
        };

        let content_col = pos.col.saturating_sub(prefix_len);
        let content_len = display_width(&content);

        // Determine selection bounds
        if content_col >= content_len {
            if is_status_bar {
                // Select entire status bar line
                self.select_status_bar_line(pos.row, &content);
            } else {
                // Clicking past end of line - select entire logical line (including wrapped segments)
                self.select_logical_line(pos.row, prefix_len);
            }
        } else if let Some((start, end)) = find_selection_boundaries(&content, content_col) {
            let sel_start = start + prefix_len;
            let sel_end = end + prefix_len;

            // Set anchor for word-drag mode
            self.view.word_selection_anchor = Some((pos.row, sel_start, sel_end));

            self.view.selection = Some(Selection {
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

    /// Select the entire logical line at the given screen position (for triple-click)
    pub fn select_line_at(&mut self, screen_x: u16, screen_y: u16) {
        let pos = match self.screen_to_content_position(screen_x, screen_y) {
            Some(p) => p,
            None => return,
        };

        let is_status_bar = pos.row >= self.view.row_map.len();

        if is_status_bar {
            if let Some((content, _)) = self.row_content(pos.row) {
                let content = content.to_string();
                self.select_status_bar_line(pos.row, &content);
            }
            return;
        }

        let prefix_len = self.prefix_len();
        let (start_row, end_row) = self.find_logical_line_bounds(pos.row);

        // Set line selection anchor for line-based drag extension
        self.view.line_selection_anchor = Some((start_row, end_row));

        // Calculate end column for the last row
        let end_content_len = display_width(&self.view.row_map[end_row].content);

        self.view.selection = Some(Selection {
            start: Position { row: start_row, col: prefix_len },
            end: Position { row: end_row, col: end_content_len + prefix_len },
            active: true,
        });
    }

    /// Select an entire status bar line (no prefix, no continuation logic)
    fn select_status_bar_line(&mut self, row: usize, content: &str) {
        let content_len = display_width(content);

        self.view.line_selection_anchor = Some((row, row));
        self.view.word_selection_anchor = None;

        self.view.selection = Some(Selection {
            start: Position { row, col: 0 },
            end: Position { row, col: content_len },
            active: true,
        });
    }

    /// Find the start and end rows of the logical line containing the given screen row
    fn find_logical_line_bounds(&self, screen_row: usize) -> (usize, usize) {
        if screen_row >= self.view.row_map.len() {
            return (screen_row, screen_row);
        }

        // Find the start of the logical line (go backwards while is_continuation)
        let mut start_row = screen_row;
        while start_row > 0 && self.view.row_map[start_row].is_continuation {
            start_row -= 1;
        }

        // Find the end of the logical line (go forward while next row is_continuation)
        let mut end_row = screen_row;
        while end_row + 1 < self.view.row_map.len() && self.view.row_map[end_row + 1].is_continuation {
            end_row += 1;
        }

        (start_row, end_row)
    }

    /// Select an entire logical line, including all wrapped segments
    fn select_logical_line(&mut self, screen_row: usize, prefix_len: usize) {
        // Find the start of the logical line (go backwards while is_continuation)
        let mut start_row = screen_row;
        while start_row > 0 && self.view.row_map[start_row].is_continuation {
            start_row -= 1;
        }

        // Find the end of the logical line (go forward while next row is_continuation)
        let mut end_row = screen_row;
        while end_row + 1 < self.view.row_map.len() && self.view.row_map[end_row + 1].is_continuation {
            end_row += 1;
        }

        // Calculate end column for the last row
        let end_content_len = display_width(&self.view.row_map[end_row].content);

        // Set anchor spanning the entire logical line
        let sel_start = prefix_len;
        let sel_end = end_content_len + prefix_len;
        self.view.word_selection_anchor = Some((start_row, sel_start, sel_end));

        self.view.selection = Some(Selection {
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
        self.view.selection = None;
        self.view.word_selection_anchor = None;
        self.view.line_selection_anchor = None;
    }

    /// Check if there's an active selection
    pub fn has_selection(&self) -> bool {
        self.view.selection.is_some()
    }

    /// Convert screen coordinates to content position.
    /// Handles both the diff content area and the status bar area.
    fn screen_to_content_position(&self, screen_x: u16, screen_y: u16) -> Option<Position> {
        let (offset_x, offset_y) = self.view.content_offset;

        // Check status bar first (more specific bounds)
        let sb_y = self.view.status_bar_screen_y;
        let sb_lines = &self.view.status_bar_lines;
        if !sb_lines.is_empty()
            && sb_y > 0
            && screen_y >= sb_y
            && (screen_y - sb_y) < sb_lines.len() as u16
        {
            let virtual_row = self.view.row_map.len() + (screen_y - sb_y) as usize;
            return Some(Position {
                row: virtual_row,
                col: screen_x as usize,
            });
        }

        // Check if within diff content area
        if screen_x >= offset_x && screen_y >= offset_y {
            let content_x = (screen_x - offset_x) as usize;
            let content_y = (screen_y - offset_y) as usize;
            // Clamp to last actual content row. Without this, dragging into the
            // empty area below the diff pushes selection.end.row past every
            // wrapped row, making the renderer highlight rows the cursor never
            // touched (e.g. the entire wrapped paragraph above).
            let row = match self.view.row_map.len() {
                0 => content_y,
                len => content_y.min(len - 1),
            };
            return Some(Position {
                row,
                col: content_x,
            });
        }

        None
    }

    /// Get the text content and prefix length for a given row index.
    /// Returns None if the row is out of bounds for both row_map and status bar.
    fn row_content(&self, screen_row: usize) -> Option<(&str, usize)> {
        let row_map_len = self.view.row_map.len();
        if screen_row < row_map_len {
            Some((&self.view.row_map[screen_row].content, self.prefix_len()))
        } else {
            let sb_idx = screen_row - row_map_len;
            self.view.status_bar_lines.get(sb_idx).map(|s| (s.as_str(), 0))
        }
    }

    /// Check if the next row is a continuation of a wrapped line.
    fn is_next_row_continuation(&self, screen_row: usize) -> bool {
        let next = screen_row + 1;
        next < self.view.row_map.len()
            && self.view.row_map[next].is_continuation
    }

    /// Get selected text (content only, without line numbers or prefixes).
    /// Handles both diff content rows and status bar rows.
    pub fn get_selected_text(&self) -> Option<String> {
        let sel = self.view.selection.as_ref()?;

        // Normalize selection (start should be before end)
        let (start, end) = if sel.start.row < sel.end.row
            || (sel.start.row == sel.end.row && sel.start.col <= sel.end.col)
        {
            (sel.start, sel.end)
        } else {
            (sel.end, sel.start)
        };

        let mut result = String::new();

        for screen_row in start.row..=end.row {
            let (content, prefix_len) = match self.row_content(screen_row) {
                Some(r) => r,
                None => break,
            };

            let content_width = display_width(content);

            if start.row == end.row {
                // Single row selection
                let start_in_content = start.col.saturating_sub(prefix_len);
                let end_in_content = end.col.saturating_sub(prefix_len);
                if start_in_content < content_width {
                    let actual_end = end_in_content.min(content_width);
                    if actual_end > start_in_content {
                        result.push_str(display_slice(content, start_in_content, actual_end));
                    }
                }
            } else if screen_row == start.row {
                // First row of multi-row selection
                let start_in_content = start.col.saturating_sub(prefix_len);
                if start_in_content < content_width {
                    result.push_str(display_slice_from(content, start_in_content));
                }
                if !self.is_next_row_continuation(screen_row) {
                    result.push('\n');
                }
            } else if screen_row == end.row {
                // Last row of multi-row selection
                let end_in_content = end.col.saturating_sub(prefix_len);
                let actual_end = end_in_content.min(content_width);
                result.push_str(display_slice_to(content, actual_end));
            } else {
                // Middle rows - take entire content
                result.push_str(content);
                if !self.is_next_row_continuation(screen_row) {
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
            self.view.path_copied_at = Some(std::time::Instant::now());
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
        self.view.path_copied_at = Some(std::time::Instant::now());
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
        self.view.path_copied_at = Some(std::time::Instant::now());
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
                DisplayableItem::Message(msg) => {
                    result.push_str(&format!("{}\n", msg));
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
        if let Some(copied_at) = self.view.path_copied_at {
            copied_at.elapsed() < std::time::Duration::from_millis(800)
        } else {
            false
        }
    }

    /// Check if a screen position is on a file header, and return the file path if so
    pub fn get_file_header_at(&self, screen_x: u16, screen_y: u16) -> Option<String> {
        let (offset_x, offset_y) = self.view.content_offset;

        // Check if within content area
        if screen_x < offset_x || screen_y < offset_y {
            return None;
        }

        let content_y = (screen_y - offset_y) as usize;

        // Look up in row_map
        if content_y < self.view.row_map.len() {
            let row_info = &self.view.row_map[content_y];
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
        // With line_num_width=3, prefix_len = 3 + 1 + 4 = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.row_map = vec![
            make_row("line one", false),
            make_row("line two", false),
        ];
        app.view.selection = Some(Selection {
            start: Position { row: 0, col: 8 }, // prefix_len = 8
            end: Position { row: 1, col: 16 },  // "line two" is 8 chars, so 8 + 8 = 16
            active: false,
        });

        let text = app.get_selected_text().unwrap();
        assert_eq!(text, "line one\nline two");
    }

    #[test]
    fn test_get_selected_text_wrapped_line_no_extra_newlines() {
        // One logical line wrapped across two screen rows
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.row_map = vec![
            make_row("first part ", false), // Start of logical line (11 chars)
            make_row("second part", true),  // Continuation (wrapped) (11 chars)
        ];
        app.view.selection = Some(Selection {
            start: Position { row: 0, col: 8 },
            end: Position { row: 1, col: 19 },  // 8 + 11 = 19
            active: false,
        });

        let text = app.get_selected_text().unwrap();
        // Should NOT have newline between wrapped parts
        assert_eq!(text, "first part second part");
    }

    #[test]
    fn test_get_selected_text_mixed_wrapped_and_unwrapped() {
        // Two logical lines, first one wraps
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.row_map = vec![
            make_row("wrapped ", false),    // Line 1, part 1 (8 chars)
            make_row("line", true),         // Line 1, part 2 (4 chars)
            make_row("normal line", false), // Line 2 (11 chars)
        ];
        app.view.selection = Some(Selection {
            start: Position { row: 0, col: 8 },
            end: Position { row: 2, col: 19 },  // 8 + 11 = 19
            active: false,
        });

        let text = app.get_selected_text().unwrap();
        // Newline only between logical lines, not within wrapped line
        assert_eq!(text, "wrapped line\nnormal line");
    }

    #[test]
    fn test_get_selected_text_starting_on_continuation() {
        // Selection starts on a continuation row
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.row_map = vec![
            make_row("first ", false),       // Line 1, part 1
            make_row("second", true),        // Line 1, part 2 (6 chars)
            make_row("next line", false),    // Line 2 (9 chars)
        ];
        // Start selection on the continuation row
        app.view.selection = Some(Selection {
            start: Position { row: 1, col: 8 },
            end: Position { row: 2, col: 17 },  // 8 + 9 = 17
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
        // With line_num_width=3, prefix_len = 3 + 1 + 4 = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("hello world", false)];

        // Click on 'w' in "world" - content col 6, screen col = 6 + prefix_len(8) + offset(1) = 15
        app.select_word_at(15, 1);

        let sel = app.view.selection.as_ref().expect("Should have selection");
        // Word "world" at content cols 6-11, + prefix_len 8
        assert_eq!(sel.start.col, 14); // 6 + 8
        assert_eq!(sel.end.col, 19); // 11 + 8
        assert!(sel.active, "Should be active to allow word-drag");

        // Should have word anchor set for drag mode
        let anchor = app.view.word_selection_anchor.expect("Should have word anchor");
        assert_eq!(anchor, (0, 14, 19));
    }

    #[test]
    fn test_select_word_at_whitespace_selects_next_word() {
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("hello world", false)];

        // Click on space between words - content col 5, screen col = 5 + 8 + 1 = 14
        app.select_word_at(14, 1);

        // Should select "world" (the word to the right)
        let sel = app.view.selection.as_ref().expect("Should have selection");
        assert_eq!(sel.start.col, 14); // "world" at content col 6 + prefix 8
        assert_eq!(sel.end.col, 19); // ends at content col 11 + prefix 8
    }

    #[test]
    fn test_select_word_at_symbols() {
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("/// comment", false)];

        // Click on second slash - content col 1, screen col = 1 + 8 + 1 = 10
        app.select_word_at(10, 1);

        // Should select "///" (all three slashes)
        let sel = app.view.selection.as_ref().expect("Should have selection");
        assert_eq!(sel.start.col, 8); // starts at content col 0 + prefix 8
        assert_eq!(sel.end.col, 11); // ends at content col 3 + prefix 8
    }

    #[test]
    fn test_select_word_at_past_end_of_line() {
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("hello", false)]; // 5 chars

        // Click past end of line - content has 5 chars, click at col 10
        // screen_x = 10 + prefix(8) + offset(1) = 19
        app.select_word_at(19, 1);

        let sel = app.view.selection.as_ref().expect("Should select whole line");
        // Should select entire line: prefix_len to prefix_len + content_len
        assert_eq!(sel.start.col, 8); // prefix_len
        assert_eq!(sel.end.col, 13); // 5 + 8
    }

    #[test]
    fn drag_below_paragraph_into_empty_area_does_not_select_whole_paragraph() {
        // Suspected bug: when a 5-row wrapped paragraph is followed by empty
        // space (e.g. only 5 rows of content but a 30-row terminal), dragging
        // past the paragraph's last row sets selection.end.row to a very large
        // number. Then in get_line_selection_range, every wrapped row of the
        // paragraph falls inside [start.row, end.row], so the renderer
        // highlights all 5 rows.
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![
            make_row("paragraph row zero", false),
            make_row("paragraph row one", true),
            make_row("paragraph row two", true),
            make_row("paragraph row three", true),
            make_row("paragraph row four", true),
        ];

        // Click on the first content row (screen_y = 1 → pos.row = 0)
        app.start_selection(9, 1);

        // Drag down into empty space well past the last content row
        // (screen_y = 15 → pos.row = 14, much larger than row_map.len() - 1)
        app.update_selection(20, 15);

        let sel = app.view.selection.as_ref().expect("selection set");
        assert!(
            sel.end.row < app.view.row_map.len(),
            "drag past content should not push selection.end.row ({}) past the \
             last actual row in row_map ({}); without clamping, every wrapped row \
             of the paragraph appears selected",
            sel.end.row,
            app.view.row_map.len() - 1,
        );
    }

    #[test]
    fn test_word_drag_extends_by_words() {
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("one two three four", false)];

        // Double-click on "two" (content col 4-7)
        // screen_x for 't' in "two" = 4 + 8 + 1 = 13
        app.select_word_at(13, 1);

        let sel = app.view.selection.as_ref().unwrap();
        assert_eq!(sel.start.col, 12); // "two" starts at content col 4 + prefix 8
        assert_eq!(sel.end.col, 15); // "two" ends at content col 7 + prefix 8

        // Now drag to "four" (content col 14-18)
        // screen_x for 'f' in "four" = 14 + 8 + 1 = 23
        app.update_selection(23, 1);

        let sel = app.view.selection.as_ref().unwrap();
        // Should extend from "two" start to "four" end
        assert_eq!(sel.start.col, 12); // "two" starts at 4 + 8
        assert_eq!(sel.end.col, 26); // "four" ends at 18 + 8
    }

    #[test]
    fn test_word_drag_backwards() {
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("one two three four", false)];

        // Double-click on "three" (content col 8-13)
        // screen_x for 't' in "three" = 8 + 8 + 1 = 17
        app.select_word_at(17, 1);

        // Now drag backwards to "one" (content col 0-3)
        // screen_x for 'o' in "one" = 0 + 8 + 1 = 9
        app.update_selection(9, 1);

        let sel = app.view.selection.as_ref().unwrap();
        // Should extend from "one" start to "three" end
        assert_eq!(sel.start.col, 8); // "one" starts at 0 + 8
        assert_eq!(sel.end.col, 21); // "three" ends at 13 + 8
    }

    #[test]
    fn test_word_anchor_cleared_on_end_selection() {
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("hello world", false)];

        // Click on 'w' in "world" - screen_x = 6 + 8 + 1 = 15
        app.select_word_at(15, 1);
        assert!(app.view.word_selection_anchor.is_some());

        app.end_selection();
        assert!(app.view.word_selection_anchor.is_none());
    }

    #[test]
    fn test_word_anchor_cleared_on_start_selection() {
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("hello world", false)];

        // Click on 'w' in "world" - screen_x = 6 + 8 + 1 = 15
        app.select_word_at(15, 1);
        assert!(app.view.word_selection_anchor.is_some());

        // Normal click should clear word anchor
        app.start_selection(9, 1);  // 0 + 8 + 1 = 9
        assert!(app.view.word_selection_anchor.is_none());
    }

    #[test]
    fn test_select_word_at_empty_line() {
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("", false)]; // Empty line

        // Click on empty line (any position)
        app.select_word_at(12, 1);

        // Should create an empty selection (start == end at prefix boundary)
        let sel = app.view.selection.as_ref().expect("Should have selection");
        assert_eq!(sel.start.col, 8); // prefix_len
        assert_eq!(sel.end.col, 8); // prefix_len + 0 chars
    }

    #[test]
    fn test_word_drag_across_rows() {
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![
            make_row("first line", false),
            make_row("second line", false),
        ];

        // Double-click on "first" (content col 0-5)
        app.select_word_at(9, 1); // screen_x = 0 + 8 + 1 = 9

        let sel = app.view.selection.as_ref().unwrap();
        assert_eq!(sel.start.row, 0);
        assert_eq!(sel.end.row, 0);

        // Drag to "second" on row 1 (content col 0-6)
        app.update_selection(9, 2); // screen_y = 1 + 1 = 2

        let sel = app.view.selection.as_ref().unwrap();
        // Should extend from "first" start (row 0) to "second" end (row 1)
        assert_eq!(sel.start.row, 0);
        assert_eq!(sel.start.col, 8); // "first" at col 0 + prefix 8
        assert_eq!(sel.end.row, 1);
        assert_eq!(sel.end.col, 14); // "second" ends at col 6 + prefix 8
    }

    #[test]
    fn test_word_drag_to_whitespace_selects_next_word() {
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("one   two", false)]; // Multiple spaces

        // Double-click on "one"
        app.select_word_at(9, 1); // col 0 + prefix 8 + offset 1 = 9

        // Drag to whitespace (col 4, between words)
        // screen_x = 4 + 8 + 1 = 13
        app.update_selection(13, 1);

        let sel = app.view.selection.as_ref().unwrap();
        // Whitespace selects word to the right ("two" at cols 6-9)
        // So selection extends from "one" start to "two" end
        assert_eq!(sel.start.col, 8); // "one" starts at 0 + 8
        assert_eq!(sel.end.col, 17); // "two" ends at content col 9 + prefix 8
    }

    #[test]
    fn test_word_drag_to_trailing_whitespace() {
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("word   ", false)]; // Trailing spaces

        // Double-click on "word"
        app.select_word_at(9, 1);  // 0 + 8 + 1 = 9

        // Drag to trailing whitespace (col 5)
        // screen_x = 5 + 8 + 1 = 14
        app.update_selection(14, 1);

        let sel = app.view.selection.as_ref().unwrap();
        // No word to the right, so falls back to cursor position
        assert_eq!(sel.start.col, 8); // "word" starts at 0 + 8
        assert_eq!(sel.end.col, 13); // cursor at 5 + 8
    }

    #[test]
    fn test_select_past_eol_on_wrapped_line_selects_entire_logical_line() {
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        // Simulate a wrapped line: first row is start, second is continuation
        app.view.row_map = vec![
            make_row("first part ", false),      // Start of logical line (11 chars)
            make_row("second part", true),       // Continuation (wrapped) (11 chars)
            make_row("next line", false),        // Different logical line
        ];

        // Click past end of the wrapped segment (row 1)
        // Content is "second part" (11 chars), click at col 15
        // screen_x = 15 + 8 + 1 = 24
        app.select_word_at(24, 2); // screen_y = 1 + 1 = 2 (row 1)

        let sel = app.view.selection.as_ref().expect("Should have selection");
        // Should select from start of logical line (row 0) to end of last segment (row 1)
        assert_eq!(sel.start.row, 0);
        assert_eq!(sel.start.col, 8); // prefix_len
        assert_eq!(sel.end.row, 1);
        assert_eq!(sel.end.col, 19); // "second part" is 11 chars + prefix 8
    }

    #[test]
    fn test_select_past_eol_on_first_segment_of_wrapped_line() {
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        // Simulate a wrapped line spanning 3 rows
        app.view.row_map = vec![
            make_row("part one ", false),   // Start (9 chars)
            make_row("part two ", true),    // Continuation (9 chars)
            make_row("part three", true),   // Continuation (10 chars)
        ];

        // Click past end of the first segment (row 0)
        // screen_x = 12 + 8 + 1 = 21
        app.select_word_at(21, 1); // screen_y = 0 + 1 = 1 (row 0)

        let sel = app.view.selection.as_ref().expect("Should have selection");
        // Should select entire logical line from row 0 to row 2
        assert_eq!(sel.start.row, 0);
        assert_eq!(sel.start.col, 8);
        assert_eq!(sel.end.row, 2);
        assert_eq!(sel.end.col, 18); // "part three" is 10 chars + prefix 8
    }

    #[test]
    fn test_select_line_at_basic() {
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("hello world", false)]; // 11 chars

        // Triple-click anywhere on the line - screen_x = 5 + 8 + 1 = 14
        app.select_line_at(14, 1);

        let sel = app.view.selection.as_ref().expect("Should have selection");
        assert_eq!(sel.start.row, 0);
        assert_eq!(sel.end.row, 0);
        assert_eq!(sel.start.col, 8); // prefix_len
        assert_eq!(sel.end.col, 19); // 11 + 8
        assert!(sel.active, "Should be active for line-drag");

        // Line selection anchor should be set
        let anchor = app.view.line_selection_anchor.expect("Should have line anchor");
        assert_eq!(anchor, (0, 0)); // start_row, end_row
    }

    #[test]
    fn test_select_line_at_wrapped_line() {
        // With line_num_width=3, prefix_len = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![
            make_row("first part ", false),  // 11 chars
            make_row("second part", true),   // 11 chars (continuation)
        ];

        // Triple-click on the continuation row
        app.select_line_at(14, 2); // screen_y = 1 + 1 = 2 (row 1)

        let sel = app.view.selection.as_ref().expect("Should have selection");
        // Should select entire logical line (both rows)
        assert_eq!(sel.start.row, 0);
        assert_eq!(sel.end.row, 1);
        assert_eq!(sel.start.col, 8); // prefix_len
        assert_eq!(sel.end.col, 19); // 11 + 8

        // Line selection anchor spans entire logical line
        let anchor = app.view.line_selection_anchor.expect("Should have line anchor");
        assert_eq!(anchor, (0, 1)); // start_row, end_row
    }

    #[test]
    fn test_prefix_len_with_zero_line_num_width() {
        // When line_num_width=0, prefix_len = 0 + PREFIX_CHAR_WIDTH = 4
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 0;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("hello", false)]; // 5 chars

        // Triple-click - screen_x = 2 + 4 + 1 = 7
        app.select_line_at(7, 1);

        let sel = app.view.selection.as_ref().expect("Should have selection");
        assert_eq!(sel.start.col, 4); // prefix_len = 4 when line_num_width = 0
        assert_eq!(sel.end.col, 9); // 5 + 4
    }

    #[test]
    fn test_select_word_at_with_zero_line_num_width() {
        // When line_num_width=0, prefix_len = 4
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 0;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("hello world", false)];

        // Click on 'w' in "world" - content col 6, screen col = 6 + 4 + 1 = 11
        app.select_word_at(11, 1);

        let sel = app.view.selection.as_ref().expect("Should have selection");
        assert_eq!(sel.start.col, 10); // 6 + 4
        assert_eq!(sel.end.col, 15); // 11 + 4
    }

    // --- Auto-copy tests ---

    #[test]
    fn test_has_non_empty_selection_point() {
        let mut app = TestAppBuilder::new().build();
        app.view.selection = Some(Selection {
            start: Position { row: 0, col: 5 },
            end: Position { row: 0, col: 5 },
            active: false,
        });
        assert!(!app.has_non_empty_selection());
    }

    #[test]
    fn test_has_non_empty_selection_range() {
        let mut app = TestAppBuilder::new().build();
        app.view.selection = Some(Selection {
            start: Position { row: 0, col: 5 },
            end: Position { row: 0, col: 10 },
            active: false,
        });
        assert!(app.has_non_empty_selection());
    }

    #[test]
    fn test_has_non_empty_selection_none() {
        let app = TestAppBuilder::new().build();
        assert!(!app.has_non_empty_selection());
    }

    #[test]
    fn test_end_selection_after_drag_clears_selection() {
        // Simulate drag: set non-empty selection, clear last_click (drag clears it)
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("hello world", false)];
        app.view.selection = Some(Selection {
            start: Position { row: 0, col: 8 },
            end: Position { row: 0, col: 14 },
            active: true,
        });
        app.view.last_click = None; // Drag clears last_click

        app.end_selection_with_auto_copy();

        // copy_selection clears the selection on success
        // In test env clipboard may fail, but selection should still be processed
        // Check that pending_copy was NOT set (drag = immediate copy)
        assert!(app.view.pending_copy.is_none());
    }

    #[test]
    fn test_end_selection_after_double_click_sets_pending_copy() {
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("hello world", false)];
        app.view.selection = Some(Selection {
            start: Position { row: 0, col: 8 },
            end: Position { row: 0, col: 13 },
            active: true,
        });
        app.view.last_click = Some((Instant::now(), 15, 1, 2)); // double-click

        app.end_selection_with_auto_copy();

        // Should defer copy for multi-click
        assert!(app.view.pending_copy.is_some());
        // Selection should still exist (not yet copied)
        assert!(app.view.selection.is_some());
    }

    #[test]
    fn test_end_selection_single_click_no_copy() {
        let mut app = TestAppBuilder::new().build();
        app.view.selection = Some(Selection {
            start: Position { row: 0, col: 5 },
            end: Position { row: 0, col: 5 },
            active: true,
        });
        app.view.last_click = Some((Instant::now(), 5, 0, 1)); // single click

        app.end_selection_with_auto_copy();

        assert!(app.view.pending_copy.is_none());
    }

    #[test]
    fn test_cancel_pending_copy() {
        let mut app = TestAppBuilder::new().build();
        app.view.pending_copy = Some(Instant::now());

        app.cancel_pending_copy();

        assert!(app.view.pending_copy.is_none());
    }

    #[test]
    fn test_check_and_execute_pending_copy_too_soon() {
        let mut app = TestAppBuilder::new().build();
        app.view.pending_copy = Some(Instant::now());
        app.view.row_map = vec![make_row("hello", false)];
        app.view.selection = Some(Selection {
            start: Position { row: 0, col: 0 },
            end: Position { row: 0, col: 5 },
            active: false,
        });

        // Should NOT execute yet (just created)
        let executed = app.check_and_execute_pending_copy();
        assert!(!executed);
        assert!(app.view.pending_copy.is_some());
    }

    #[test]
    fn test_check_and_execute_pending_copy_after_timeout() {
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 0;
        app.view.content_offset = (0, 0);
        // Set pending_copy to 600ms ago
        app.view.pending_copy = Some(Instant::now() - std::time::Duration::from_millis(600));
        app.view.row_map = vec![make_row("hello", false)];
        app.view.selection = Some(Selection {
            start: Position { row: 0, col: 4 },
            end: Position { row: 0, col: 9 },
            active: false,
        });

        let executed = app.check_and_execute_pending_copy();
        assert!(executed);
        assert!(app.view.pending_copy.is_none());
    }

    // --- Status bar selection tests ---

    #[test]
    fn test_screen_to_content_position_status_bar() {
        let mut app = TestAppBuilder::new().build();
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("line1", false), make_row("line2", false)];
        app.view.status_bar_lines = vec!["status line".to_string()];
        app.view.status_bar_screen_y = 10;

        // Click in status bar area (screen_y=10)
        let pos = app.screen_to_content_position(5, 10);
        assert!(pos.is_some());
        let pos = pos.unwrap();
        // Virtual row = row_map.len() + (10 - 10) = 2
        assert_eq!(pos.row, 2);
        assert_eq!(pos.col, 5);
    }

    #[test]
    fn test_screen_to_content_position_outside_all_areas() {
        let mut app = TestAppBuilder::new().build();
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("line1", false)];
        app.view.status_bar_lines = vec!["status".to_string()];
        app.view.status_bar_screen_y = 10;

        // Click above content area and not in status bar
        let pos = app.screen_to_content_position(0, 0);
        assert!(pos.is_none());
    }

    #[test]
    fn test_get_selected_text_from_status_bar() {
        let mut app = TestAppBuilder::new().build();
        app.view.content_offset = (1, 1);
        app.view.line_num_width = 0;
        app.view.row_map = vec![make_row("diff content", false)];
        app.view.status_bar_lines = vec!["repo | feat vs main".to_string()];
        app.view.status_bar_screen_y = 10;

        // Select "feat" from status bar (virtual row 1, cols 7..11, no prefix)
        app.view.selection = Some(Selection {
            start: Position { row: 1, col: 7 },
            end: Position { row: 1, col: 11 },
            active: false,
        });

        let text = app.get_selected_text();
        assert_eq!(text, Some("feat".to_string()));
    }

    #[test]
    fn test_double_click_status_bar_selects_word() {
        let mut app = TestAppBuilder::new().build();
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("diff line", false)];
        app.view.status_bar_lines = vec!["repo | feature vs main".to_string()];
        app.view.status_bar_screen_y = 5;

        // Double-click on "feature" (starts at col 7, status bar screen_y=5)
        // screen_to_content_position maps (10, 5) → virtual row 1, col 10
        // "feature" spans cols 7..14 in the status bar text
        app.select_word_at(10, 5);

        let sel = app.view.selection.as_ref().expect("Should have selection");
        assert_eq!(sel.start.row, 1); // virtual row (row_map.len() + 0)
        assert_eq!(sel.start.col, 7); // "feature" starts at col 7 (no prefix)
        assert_eq!(sel.end.col, 14);  // "feature" ends at col 14

        let text = app.get_selected_text();
        assert_eq!(text, Some("feature".to_string()));
    }

    #[test]
    fn test_triple_click_status_bar_selects_line() {
        let mut app = TestAppBuilder::new().build();
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("diff line", false)];
        app.view.status_bar_lines = vec!["repo | feat vs main".to_string()];
        app.view.status_bar_screen_y = 5;

        // Triple-click anywhere on the status bar line
        app.select_line_at(10, 5);

        let sel = app.view.selection.as_ref().expect("Should have selection");
        assert_eq!(sel.start.row, 1); // virtual row
        assert_eq!(sel.start.col, 0); // line select starts at 0 (no prefix)
        assert_eq!(sel.end.col, 19);  // "repo | feat vs main" = 19 chars

        let text = app.get_selected_text();
        assert_eq!(text, Some("repo | feat vs main".to_string()));
    }

    #[test]
    fn test_double_click_status_bar_past_end_selects_line() {
        let mut app = TestAppBuilder::new().build();
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![make_row("diff line", false)];
        app.view.status_bar_lines = vec!["short".to_string()];
        app.view.status_bar_screen_y = 5;

        // Double-click past end of status bar content
        app.select_word_at(50, 5);

        let sel = app.view.selection.as_ref().expect("Should have selection");
        // Should select entire line since click was past content
        assert_eq!(sel.start.col, 0);
        assert_eq!(sel.end.col, 5); // "short" = 5 chars
    }

    // ===== Display-width (CJK / wide character) tests =====
    //
    // These tests verify that selection coordinates use display-width columns
    // (not byte offsets or char counts). CJK characters are 1 char, 3 bytes,
    // but 2 display columns each.

    #[test]
    fn get_selected_text_cjk_single_row() {
        // "hi你好" = 2 ASCII + 2 CJK = 6 display columns, 4 chars, 8 bytes
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3; // prefix_len = 8
        app.view.row_map = vec![make_row("hi你好", false)];

        // Select display columns 2-4 within content = "你" (2 display cols)
        app.view.selection = Some(Selection {
            start: Position { row: 0, col: 10 }, // 8 + 2
            end: Position { row: 0, col: 12 },   // 8 + 4
            active: false,
        });

        let text = app.get_selected_text().unwrap();
        assert_eq!(text, "你", "should select one CJK char at display cols 2-4");
    }

    #[test]
    fn get_selected_text_cjk_from_start() {
        // "你好world" = 2 CJK + 5 ASCII = 4+5 = 9 display columns
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.row_map = vec![make_row("你好world", false)];

        // Select first 4 display columns = "你好"
        app.view.selection = Some(Selection {
            start: Position { row: 0, col: 8 },
            end: Position { row: 0, col: 12 },  // 8 + 4
            active: false,
        });

        let text = app.get_selected_text().unwrap();
        assert_eq!(text, "你好");
    }

    #[test]
    fn get_selected_text_cjk_multirow() {
        // Two rows: one with CJK, one ASCII
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.row_map = vec![
            make_row("你好world", false),   // 9 display cols
            make_row("hello", false),        // 5 display cols
        ];

        // Select from CJK row (display col 4 = after "你好") to end of second row
        app.view.selection = Some(Selection {
            start: Position { row: 0, col: 12 }, // 8 + 4 (after "你好")
            end: Position { row: 1, col: 13 },   // 8 + 5
            active: false,
        });

        let text = app.get_selected_text().unwrap();
        assert_eq!(text, "world\nhello");
    }

    #[test]
    fn find_selection_boundaries_cjk_word() {
        // "你好 world" — CJK chars are NOT word chars (not alphanumeric),
        // so they're symbols. Double-clicking on "你" at display col 0
        // should select consecutive CJK symbols "你好".
        let result = find_selection_boundaries("你好 world", 0);
        assert_eq!(result, Some((0, 4)), "CJK symbols 你好 span display cols 0-4");
    }

    #[test]
    fn find_selection_boundaries_ascii_after_cjk() {
        // "你好 world" — clicking on 'w' at display col 5
        // (你=0-1, 好=2-3, space=4, w=5)
        let result = find_selection_boundaries("你好 world", 5);
        assert_eq!(result, Some((5, 10)), "word 'world' spans display cols 5-10");
    }

    #[test]
    fn select_line_at_cjk_content_uses_display_width() {
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![
            make_row("你好世界", false),  // 8 display columns
        ];

        // Triple-click to select the line
        app.select_line_at(5, 1);

        let sel = app.view.selection.as_ref().expect("should have selection");
        // End col should be prefix_len(8) + display_width("你好世界")(8) = 16
        assert_eq!(sel.end.col, 16, "end col should use display width, not char count (4)");
    }
}
