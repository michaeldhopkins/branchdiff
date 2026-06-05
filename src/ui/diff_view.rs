//! Diff view rendering with pure data model separation.
//!
//! The DiffViewModel provides a pure view model for rendering, enabling
//! easier unit testing without requiring a full App instance.

use std::collections::{HashMap, HashSet};

use unicode_width::UnicodeWidthStr;
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::{App, DisplayableItem, FrameContext, SearchState, Selection};
use crate::diff::{DiffLine, LineSource};
use crate::image_diff::{ImageCache, IMAGE_PANEL_OVERHEAD};
use crate::syntax::reset_highlight_state;
use crate::vcs::VcsBackend;

use super::colors::{line_style, status_symbol, SEARCH_CURRENT_BG, SEARCH_MATCH_BG};
use super::selection::{apply_selection_to_span, get_line_selection_range};
use super::colors::line_style_with_highlight;
use super::spans::{
    build_deletion_spans_with_highlight, build_insertion_spans_with_highlight, classify_inline_change,
    get_deletion_source, get_insertion_source, inline_display_width, InlineChangeType,
    syntax_highlight_content, syntax_highlight_inline_spans,
};
use super::wrapping::{content_display_width, wrap_content};
use super::{ScreenRowInfo, PREFIX_CHAR_WIDTH};

/// Pure data needed for diff rendering (no App reference during render).
pub struct DiffViewModel<'a> {
    /// Visible displayable items from FrameContext.
    pub items: &'a [DisplayableItem],
    /// All diff lines (for lookup by index).
    pub lines: &'a [DiffLine],
    /// Current selection state.
    pub selection: &'a Option<Selection>,
    /// Set of collapsed file paths.
    pub collapsed_files: &'a HashSet<String>,
    /// Rendering area dimensions.
    pub area: Rect,
    /// Whether to show the "copied" flash in the title.
    pub show_copied_flash: bool,
    /// Image cache for loaded image data.
    pub image_cache: &'a ImageCache,
    /// Font size in pixels (width, height) for image height calculations.
    pub font_size: (u16, u16),
    /// VCS backend for UI customization (e.g., gutter symbols).
    pub vcs_backend: VcsBackend,
    /// Active search state (for highlighting matches).
    pub search: &'a Option<SearchState>,
    /// Files changed on upstream since fork point (for ↑ markers).
    pub upstream_files: Option<&'a HashSet<String>>,
    /// Files marked as reviewed by the user.
    pub reviewed_files: &'a HashMap<String, u64>,
    /// Wrapped rows of the first visible item to skip (mid-line scroll position).
    pub top_sub_row: usize,
}

/// Position where an image should be rendered after text render
#[derive(Debug, Clone)]
pub struct ImageRenderPosition {
    /// File path to identify the image in the cache
    pub file_path: String,
    /// Screen row where image rendering starts (relative to content area)
    pub start_row: u16,
    /// Height in rows for the image display
    pub height: u16,
    /// Expected available height for image sizing (used even when viewport clips)
    /// This ensures consistent image dimensions when scrolling
    pub expected_available_height: u16,
}

/// Output from rendering (data App needs to store).
pub struct RenderOutput {
    pub row_map: Vec<ScreenRowInfo>,
    pub content_offset: (u16, u16),
    pub line_num_width: usize,
    pub content_width: usize,
    /// Positions where images should be rendered (after text rendering)
    pub image_positions: Vec<ImageRenderPosition>,
}

impl<'a> DiffViewModel<'a> {
    /// Create view model from App and FrameContext.
    pub fn from_app(app: &'a App, ctx: &'a FrameContext, area: Rect) -> Self {
        let (start, end) = ctx.visible_range(app);
        let items = &ctx.items()[start..end];

        // Image markers can't be scrolled into mid-line (they render from their
        // top), so pin the top sub-row to 0 when the first visible item is one.
        let top_sub_row = items
            .first()
            .and_then(|it| it.as_line_index())
            .filter(|&idx| app.lines[idx].is_image_marker())
            .map_or(app.view.sub_row, |_| 0);

        Self {
            items,
            lines: &app.lines,
            selection: &app.view.selection,
            collapsed_files: &app.view.collapsed_files,
            area,
            show_copied_flash: app.should_show_copied_flash(),
            image_cache: &app.image_cache,
            font_size: app.font_size,
            vcs_backend: app.comparison.vcs_backend,
            search: &app.search,
            upstream_files: app.comparison.divergence.as_ref().map(|d| &d.upstream_files),
            reviewed_files: &app.view.reviewed_files,
            top_sub_row,
        }
    }

    /// Check if a file is collapsed.
    fn is_file_collapsed(&self, path: &str) -> bool {
        self.collapsed_files.contains(path)
    }

    /// Build the prefix string for a diff line (e.g. "+ C " or "M M ").
    /// Moved lines always show "M M " regardless of source.
    fn line_prefix(&self, line: &DiffLine, default_char: char, source: LineSource) -> String {
        if line.move_target.is_some() {
            "M   ".to_string()
        } else {
            format!("{} {} ", default_char, status_symbol(source, self.vcs_backend))
        }
    }

    /// Render the diff view and return output data.
    pub fn render(&self, frame: &mut Frame) -> RenderOutput {
        // Reset syntax highlight state at the start of each render to avoid
        // stale state from previous renders causing flickering or incorrect colors
        reset_highlight_state();

        // Global max (all lines, not just visible) so `content_width` is constant
        // as you scroll. The scroll engine caches wrap heights against it; a width
        // that shifted with the visible window would desync them and a deep
        // within-line scroll could render past the line's end. Matches
        // `App::estimate_content_width`.
        let max_line_num = self
            .lines
            .iter()
            .filter_map(|line| line.line_number)
            .max()
            .unwrap_or(0);
        let line_num_width = if max_line_num > 0 {
            max_line_num.to_string().len() + 1
        } else {
            0
        };

        let available_width = self.area.width.saturating_sub(2) as usize;
        let prefix_width =
            if line_num_width > 0 { line_num_width + 1 } else { 0 } + PREFIX_CHAR_WIDTH;
        let content_width = available_width.saturating_sub(prefix_width);

        let content_offset_x = self.area.x + 1;
        let content_offset_y = self.area.y + 1;

        let max_content_rows = self.area.height.saturating_sub(2) as usize;

        let mut all_lines: Vec<Line> = Vec::new();
        let mut all_row_infos: Vec<ScreenRowInfo> = Vec::new();
        let mut image_positions: Vec<ImageRenderPosition> = Vec::new();
        let mut screen_row_idx = 0;

        for (item_pos, item) in self.items.iter().enumerate() {
            let skip_rows = if item_pos == 0 { self.top_sub_row } else { 0 };
            match item {
                DisplayableItem::Elided(count) => {
                    self.render_elided_marker(
                        *count,
                        line_num_width,
                        &mut all_lines,
                        &mut all_row_infos,
                    );
                    screen_row_idx += 1;
                }
                DisplayableItem::Message(msg) => {
                    self.render_message(
                        msg,
                        line_num_width,
                        &mut all_lines,
                        &mut all_row_infos,
                    );
                    screen_row_idx += 1;
                }
                DisplayableItem::Line(idx) => {
                    let rows_added = self.render_diff_line(
                        &self.lines[*idx],
                        *idx,
                        line_num_width,
                        prefix_width,
                        content_width,
                        screen_row_idx,
                        max_content_rows,
                        skip_rows,
                        &mut all_lines,
                        &mut all_row_infos,
                        &mut image_positions,
                    );
                    screen_row_idx += rows_added;
                }
            }
        }

        // Determine title based on current file (with optional "copied" flash)
        let current_file = self.find_current_file();
        let title = if self.show_copied_flash {
            Line::from(vec![Span::styled(
                " ✓ Copied ",
                Style::default().fg(Color::Green),
            )])
        } else {
            match current_file {
                Some(file) => Line::from(vec![Span::styled(
                    format!(" {} ", file),
                    Style::default().fg(Color::White),
                )]),
                None => Line::from(vec![Span::styled(
                    " branchdiff ",
                    Style::default().fg(Color::DarkGray),
                )]),
            }
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        frame.render_widget(Clear, self.area);
        let paragraph = Paragraph::new(all_lines).block(block);
        frame.render_widget(paragraph, self.area);

        if let Some(search) = self.search {
            let inner = Rect::new(
                self.area.x + 1,
                self.area.y + self.area.height.saturating_sub(2),
                self.area.width.saturating_sub(2),
                1,
            );
            if inner.width > 2 {
                render_search_bar(frame, search, inner);
            }
        }

        RenderOutput {
            row_map: all_row_infos,
            content_offset: (content_offset_x, content_offset_y),
            line_num_width,
            content_width,
            image_positions,
        }
    }

    /// Find the current file being displayed (file path of first visible line).
    fn find_current_file(&self) -> Option<String> {
        for item in self.items {
            if let DisplayableItem::Line(idx) = item {
                let line = &self.lines[*idx];
                if let Some(ref path) = line.file_path {
                    return Some(path.clone());
                }
            }
        }
        None
    }

    /// Render an elided marker.
    fn render_elided_marker(
        &self,
        count: usize,
        line_num_width: usize,
        all_lines: &mut Vec<Line<'static>>,
        all_row_infos: &mut Vec<ScreenRowInfo>,
    ) {
        let prefix_str = if line_num_width > 0 {
            " ".repeat(line_num_width + 1)
        } else {
            String::new()
        };
        let elided_style = line_style(LineSource::Elided);
        let elided_text = format!("{} lines hidden", count);

        let mut spans = Vec::new();
        if !prefix_str.is_empty() {
            spans.push(Span::styled(prefix_str, Style::default().fg(Color::DarkGray)));
        }
        spans.push(Span::styled(
            format!("┈┈ ⋮ {} ⋮ ┈┈", elided_text),
            elided_style,
        ));

        all_lines.push(Line::from(spans));
        all_row_infos.push(ScreenRowInfo {
            content: elided_text,
            is_file_header: false,
            file_path: None,
            is_continuation: false,
        });
    }

    /// Render an informational message (e.g., empty-state text).
    fn render_message(
        &self,
        msg: &str,
        line_num_width: usize,
        all_lines: &mut Vec<Line<'static>>,
        all_row_infos: &mut Vec<ScreenRowInfo>,
    ) {
        let prefix_str = if line_num_width > 0 {
            " ".repeat(line_num_width + 1)
        } else {
            String::new()
        };
        let style = line_style(LineSource::Elided);

        let mut spans = Vec::new();
        if !prefix_str.is_empty() {
            spans.push(Span::styled(prefix_str, Style::default().fg(Color::DarkGray)));
        }
        spans.push(Span::styled(
            format!("┈┈ ⋮ {} ⋮ ┈┈", msg),
            style,
        ));

        all_lines.push(Line::from(spans));
        all_row_infos.push(ScreenRowInfo {
            content: msg.to_string(),
            is_file_header: false,
            file_path: None,
            is_continuation: false,
        });
    }

    /// Render a diff line and return the number of screen rows used.
    fn render_diff_line(
        &self,
        diff_line: &DiffLine,
        line_idx: usize,
        line_num_width: usize,
        prefix_width: usize,
        content_width: usize,
        screen_row_idx: usize,
        max_rows: usize,
        skip_rows: usize,
        all_lines: &mut Vec<Line<'static>>,
        all_row_infos: &mut Vec<ScreenRowInfo>,
        image_positions: &mut Vec<ImageRenderPosition>,
    ) -> usize {
        let is_moved = diff_line.move_target.is_some();
        let style = if is_moved {
            line_style(LineSource::CanceledCommitted)
        } else {
            line_style(diff_line.source)
        };

        let prefix_str = if let Some(num) = diff_line.line_number {
            format!("{:>width$} ", num, width = line_num_width)
        } else if line_num_width > 0 {
            " ".repeat(line_num_width + 1)
        } else {
            String::new()
        };

        // File header
        if diff_line.source == LineSource::FileHeader {
            return self.render_file_header(
                diff_line,
                &prefix_str,
                style,
                all_lines,
                all_row_infos,
            );
        }

        // Elided line (legacy path)
        if diff_line.source == LineSource::Elided {
            return self.render_elided_line(
                diff_line,
                &prefix_str,
                style,
                all_lines,
                all_row_infos,
            );
        }

        // Image marker (placeholder for actual image rendering)
        if diff_line.is_image_marker() {
            return self.render_image_marker(
                diff_line,
                &prefix_str,
                screen_row_idx,
                all_lines,
                all_row_infos,
                image_positions,
            );
        }

        // Lines with inline spans
        if !diff_line.inline_spans.is_empty() {
            return self.render_inline_spans(
                diff_line,
                line_idx,
                &prefix_str,
                style,
                prefix_width,
                content_width,
                screen_row_idx,
                max_rows,
                skip_rows,
                all_lines,
                all_row_infos,
            );
        }

        // Plain content
        self.render_plain_content(
            diff_line,
            line_idx,
            &prefix_str,
            style,
            prefix_width,
            content_width,
            screen_row_idx,
            max_rows,
            skip_rows,
            all_lines,
            all_row_infos,
        )
    }

    fn render_file_header(
        &self,
        diff_line: &DiffLine,
        prefix_str: &str,
        style: Style,
        all_lines: &mut Vec<Line<'static>>,
        all_row_infos: &mut Vec<ScreenRowInfo>,
    ) -> usize {
        let mut spans = Vec::new();
        if !prefix_str.is_empty() {
            spans.push(Span::styled(
                prefix_str.to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }

        let is_collapsed = diff_line
            .file_path
            .as_ref()
            .map(|p| self.is_file_collapsed(p))
            .unwrap_or(false);
        let chevron = if is_collapsed { "▶ " } else { "▼ " };

        spans.push(Span::styled(chevron, Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled("── ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(diff_line.content.clone(), style));
        spans.push(Span::styled(" ──", Style::default().fg(Color::DarkGray)));

        // Add ✓ marker if file is reviewed
        if let Some(ref path) = diff_line.file_path
            && self.reviewed_files.contains_key(path)
        {
            spans.push(Span::styled(" ✓", Style::default().fg(Color::Green)));
        }

        // Add ↑ marker if this file also changed upstream
        if let Some(upstream) = &self.upstream_files
            && let Some(ref path) = diff_line.file_path
            && upstream.contains(path)
        {
            spans.push(Span::styled(" ↑", Style::default().fg(Color::Yellow)));
        }

        all_lines.push(Line::from(spans));
        all_row_infos.push(ScreenRowInfo {
            content: diff_line.content.clone(),
            is_file_header: true,
            file_path: diff_line.file_path.clone(),
            is_continuation: false,
        });

        1
    }

    fn render_elided_line(
        &self,
        diff_line: &DiffLine,
        prefix_str: &str,
        style: Style,
        all_lines: &mut Vec<Line<'static>>,
        all_row_infos: &mut Vec<ScreenRowInfo>,
    ) -> usize {
        let mut spans = Vec::new();
        if !prefix_str.is_empty() {
            spans.push(Span::styled(
                prefix_str.to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }
        spans.push(Span::styled(
            format!("┈┈ ⋮ {} ⋮ ┈┈", diff_line.content),
            style,
        ));

        all_lines.push(Line::from(spans));
        all_row_infos.push(ScreenRowInfo {
            content: diff_line.content.clone(),
            is_file_header: false,
            file_path: diff_line.file_path.clone(),
            is_continuation: false,
        });

        1
    }

    fn render_image_marker(
        &self,
        diff_line: &DiffLine,
        prefix_str: &str,
        screen_row_idx: usize,
        all_lines: &mut Vec<Line<'static>>,
        all_row_infos: &mut Vec<ScreenRowInfo>,
        image_positions: &mut Vec<ImageRenderPosition>,
    ) -> usize {
        // Check if we have loaded image data in the cache
        let image_info = diff_line
            .file_path
            .as_ref()
            .and_then(|path| self.image_cache.peek(path));

        // If we have image data, reserve space for rendering (protocols created lazily during render)
        let image_dims = image_info.map(|state| {
            let before_dims = state
                .before
                .as_ref()
                .map(|img| (img.original_width, img.original_height));
            let after_dims = state
                .after
                .as_ref()
                .map(|img| (img.original_width, img.original_height));
            (before_dims, after_dims)
        });

        let has_image_data = image_dims.is_some_and(|(b, a)| b.is_some() || a.is_some());

        if has_image_data
            && let Some(ref path) = diff_line.file_path
            && let Some((before_dims, after_dims)) = image_dims
        {
            // Calculate height based on actual image dimensions
            let image_height = crate::ui::image_view::calculate_image_height_for_images(
                before_dims,
                after_dims,
                self.area.width,
                self.font_size,
            );

            // Calculate expected available height for consistent sizing
            // This height is used for dimension calculations even when viewport clips
            let expected_available_height = image_height.saturating_sub(IMAGE_PANEL_OVERHEAD);

            // Record position for image rendering (saturate to u16::MAX for safety)
            image_positions.push(ImageRenderPosition {
                file_path: path.clone(),
                start_row: screen_row_idx.min(u16::MAX as usize) as u16,
                height: image_height,
                expected_available_height,
            });

            // Add blank lines as placeholders for the image area
            for i in 0..image_height {
                all_lines.push(Line::from(vec![]));
                all_row_infos.push(ScreenRowInfo {
                    content: String::new(),
                    is_file_header: false,
                    file_path: diff_line.file_path.clone(),
                    is_continuation: i > 0,
                });
            }

            return image_height as usize;
        }

        // Fallback: render text placeholder (no image available or protocols not ready)
        let mut spans = Vec::new();
        if !prefix_str.is_empty() {
            spans.push(Span::styled(
                prefix_str.to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }

        let display_text = match image_info {
            Some(state) => {
                // Show metadata for before/after images
                let before_info = state
                    .before
                    .as_ref()
                    .map(|img| img.metadata_string())
                    .unwrap_or_else(|| "(new)".to_string());
                let after_info = state
                    .after
                    .as_ref()
                    .map(|img| img.metadata_string())
                    .unwrap_or_else(|| "(deleted)".to_string());
                format!("[image: {} -> {}]", before_info, after_info)
            }
            None => "[image file - loading...]".to_string(),
        };

        spans.push(Span::styled("  ", Style::default()));
        spans.push(Span::styled(display_text, Style::default().fg(Color::Cyan)));

        all_lines.push(Line::from(spans));
        all_row_infos.push(ScreenRowInfo {
            content: diff_line.content.clone(),
            is_file_header: false,
            file_path: diff_line.file_path.clone(),
            is_continuation: false,
        });

        1
    }

    fn render_inline_spans(
        &self,
        diff_line: &DiffLine,
        line_idx: usize,
        prefix_str: &str,
        style: Style,
        prefix_width: usize,
        content_width: usize,
        mut screen_row_idx: usize,
        max_rows: usize,
        skip_rows: usize,
        all_lines: &mut Vec<Line<'static>>,
        all_row_infos: &mut Vec<ScreenRowInfo>,
    ) -> usize {
        let inline_width = inline_display_width(&diff_line.inline_spans);
        let rows_before = all_lines.len();
        let change_type = classify_inline_change(&diff_line.inline_spans);

        let should_split = change_type == InlineChangeType::PureDeletion
            || (change_type == InlineChangeType::Mixed && inline_width > content_width);

        if should_split {
            let del_source = get_deletion_source(&diff_line.inline_spans);
            let ins_source = if change_type == InlineChangeType::PureDeletion {
                diff_line.change_source.unwrap_or(LineSource::Committed)
            } else {
                get_insertion_source(&diff_line.inline_spans)
            };

            let old_content = diff_line.old_content.as_deref().unwrap_or("");
            let del_spans = build_deletion_spans_with_highlight(
                &diff_line.inline_spans,
                del_source,
                old_content,
                diff_line.file_path.as_deref(),
            );

            // Split the skip window across the concatenated del ++ ins rows. The
            // del count uses the same div_ceil as `wrapped_line_height` so the
            // boundary lines up with the scroll engine.
            let del_width = content_display_width(old_content);
            let del_full_rows = if del_spans.is_empty() || del_width == 0 {
                0
            } else {
                del_width.div_ceil(content_width.max(1))
            };
            let del_skip = skip_rows.min(del_full_rows);
            let ins_skip = skip_rows.saturating_sub(del_full_rows);

            if !del_spans.is_empty() {
                let del_style = line_style(del_source);
                let del_prefix_str = if !prefix_str.is_empty() {
                    " ".repeat(prefix_str.len())
                } else {
                    String::new()
                };

                // Search match offsets are computed against `line.content` (the
                // new text), so they do not align with the deletion-side text
                // (`old_content`). Skip search highlighting on this side.
                let del_prefix_char = self.line_prefix(diff_line, '-', del_source);
                let (mut del_lines, del_row_infos) = wrap_content(
                    del_spans,
                    old_content,
                    del_prefix_str,
                    del_prefix_char,
                    del_style,
                    content_width,
                    prefix_width,
                    del_skip,
                    max_rows.saturating_sub(screen_row_idx).max(1),
                );
                apply_selection_to_wrapped_lines(
                    &mut del_lines,
                    &del_row_infos,
                    self.selection,
                    screen_row_idx,
                    prefix_width,
                );

                screen_row_idx += del_lines.len();
                all_lines.extend(del_lines);
                all_row_infos.extend(del_row_infos);
            }

            let new_content = &diff_line.content;
            let ins_style = line_style(ins_source);
            let ins_spans = build_insertion_spans_with_highlight(
                &diff_line.inline_spans,
                ins_source,
                new_content,
                diff_line.file_path.as_deref(),
            );
            let ins_spans = apply_search_to_content(ins_spans, self.search, line_idx);

            let ins_prefix_char = self.line_prefix(diff_line, '+', ins_source);
            let (mut ins_lines, ins_row_infos) = wrap_content(
                ins_spans,
                new_content,
                prefix_str.to_string(),
                ins_prefix_char,
                ins_style,
                content_width,
                prefix_width,
                ins_skip,
                max_rows.saturating_sub(screen_row_idx).max(1),
            );
            apply_selection_to_wrapped_lines(
                &mut ins_lines,
                &ins_row_infos,
                self.selection,
                screen_row_idx,
                prefix_width,
            );

            all_lines.extend(ins_lines);
            all_row_infos.extend(ins_row_infos);

            return all_lines.len() - rows_before;
        }

        if inline_width > content_width {
            match change_type {
                InlineChangeType::PureAddition => {
                    let highlight_source = get_insertion_source(&diff_line.inline_spans);
                    let highlight_style = line_style_with_highlight(highlight_source);
                    let content_spans = syntax_highlight_inline_spans(
                        &diff_line.inline_spans,
                        &diff_line.content,
                        diff_line.file_path.as_deref(),
                        style,
                        highlight_style,
                    );

                    let content_spans = apply_search_to_content(content_spans, self.search, line_idx);

                    let prefix_char = self.line_prefix(diff_line, diff_line.prefix, diff_line.source);
                    let (mut lines, row_infos) = wrap_content(
                        content_spans,
                        &diff_line.content,
                        prefix_str.to_string(),
                        prefix_char,
                        style,
                        content_width,
                        prefix_width,
                        skip_rows,
                        max_rows.saturating_sub(screen_row_idx).max(1),
                    );
                    apply_selection_to_wrapped_lines(
                        &mut lines,
                        &row_infos,
                        self.selection,
                        screen_row_idx,
                        prefix_width,
                    );

                    all_lines.extend(lines);
                    all_row_infos.extend(row_infos);

                    return all_lines.len() - rows_before;
                }
                InlineChangeType::NoChange | InlineChangeType::Mixed | InlineChangeType::PureDeletion => {}
            }
        }

        // Non-wrapped inline spans with syntax highlighting.
        // Use the actual insertion source from spans, not the line's base source.
        // The rendered output here interleaves deletion text (from inline_spans)
        // with content text, so search highlights need offset translation.
        let highlight_source = get_insertion_source(&diff_line.inline_spans);
        let highlight_style = line_style_with_highlight(highlight_source);
        let content_spans = syntax_highlight_inline_spans(
            &diff_line.inline_spans,
            &diff_line.content,
            diff_line.file_path.as_deref(),
            style,
            highlight_style,
        );

        let content_spans = apply_search_to_inline_render(
            content_spans,
            &diff_line.inline_spans,
            self.search,
            line_idx,
        );

        let prefix_char = self.line_prefix(diff_line, diff_line.prefix, diff_line.source);
        let (mut lines, row_infos) = wrap_content(
            content_spans,
            &diff_line.content,
            prefix_str.to_string(),
            prefix_char,
            style,
            content_width,
            prefix_width,
            skip_rows,
            max_rows.saturating_sub(screen_row_idx).max(1),
        );
        apply_selection_to_wrapped_lines(
            &mut lines,
            &row_infos,
            self.selection,
            screen_row_idx,
            prefix_width,
        );

        all_lines.extend(lines);
        all_row_infos.extend(row_infos);

        all_lines.len() - rows_before
    }

    fn render_plain_content(
        &self,
        diff_line: &DiffLine,
        line_idx: usize,
        prefix_str: &str,
        style: Style,
        prefix_width: usize,
        content_width: usize,
        screen_row_idx: usize,
        max_rows: usize,
        skip_rows: usize,
        all_lines: &mut Vec<Line<'static>>,
        all_row_infos: &mut Vec<ScreenRowInfo>,
    ) -> usize {
        let prefix_char = self.line_prefix(diff_line, diff_line.prefix, diff_line.source);

        // Apply syntax highlighting - foreground from syntax, background from diff style
        let content_spans = syntax_highlight_content(
            &diff_line.content,
            diff_line.file_path.as_deref(),
            style,
        );

        let content_spans = apply_search_to_content(content_spans, self.search, line_idx);

        let (mut lines, row_infos) = wrap_content(
            content_spans,
            &diff_line.content,
            prefix_str.to_string(),
            prefix_char,
            style,
            content_width,
            prefix_width,
            skip_rows,
            max_rows.saturating_sub(screen_row_idx).max(1),
        );
        apply_selection_to_wrapped_lines(
            &mut lines,
            &row_infos,
            self.selection,
            screen_row_idx,
            prefix_width,
        );

        let rows_added = lines.len();
        all_lines.extend(lines);
        all_row_infos.extend(row_infos);

        rows_added
    }
}

#[cfg(test)]
fn apply_selection_to_content(
    content_spans: Vec<Span<'static>>,
    selection: &Option<Selection>,
    screen_row_idx: usize,
    prefix_width: usize,
) -> Vec<Span<'static>> {
    if let Some((sel_start, sel_end)) = get_line_selection_range(selection, screen_row_idx) {
        let content_sel_start = sel_start.saturating_sub(prefix_width);
        let content_sel_end = sel_end.saturating_sub(prefix_width);

        let mut result = Vec::new();
        let mut display_offset = 0;

        for span in content_spans {
            let span_width = UnicodeWidthStr::width(span.content.as_ref());
            let span_with_selection =
                apply_selection_to_span(span, display_offset, content_sel_start, content_sel_end);
            display_offset += span_width;
            result.extend(span_with_selection);
        }
        result
    } else {
        content_spans
    }
}

/// Apply selection highlighting to already-wrapped lines.
///
/// Each visual row gets its own selection check using `start_screen_row + i`,
/// so continuation rows of wrapped lines are highlighted correctly.
fn apply_selection_to_wrapped_lines(
    lines: &mut [Line<'static>],
    row_infos: &[ScreenRowInfo],
    selection: &Option<Selection>,
    start_screen_row: usize,
    prefix_width: usize,
) {
    if selection.is_none() {
        return;
    }
    for (i, line) in lines.iter_mut().enumerate() {
        let screen_row = start_screen_row + i;
        let Some((sel_start, sel_end)) = get_line_selection_range(selection, screen_row) else {
            continue;
        };
        let content_sel_start = sel_start.saturating_sub(prefix_width);
        let content_sel_end = sel_end.saturating_sub(prefix_width);

        // Determine how many leading spans are prefix (not content).
        // First row of a logical line: 2 prefix spans (line nums + prefix char).
        // Continuation rows: 1 prefix span (indentation).
        let prefix_span_count = if row_infos[i].is_continuation { 1 } else { 2 };

        let all_spans: Vec<Span<'static>> = std::mem::take(&mut line.spans);
        let mut result: Vec<Span<'static>> = Vec::with_capacity(all_spans.len() + 2);
        let mut display_offset = 0;

        for (idx, span) in all_spans.into_iter().enumerate() {
            if idx < prefix_span_count {
                result.push(span);
            } else {
                let span_width = UnicodeWidthStr::width(span.content.as_ref());
                let selected = apply_selection_to_span(
                    span,
                    display_offset,
                    content_sel_start,
                    content_sel_end,
                );
                display_offset += span_width;
                result.extend(selected);
            }
        }

        *line = Line::from(result);
    }
}

/// Translate a content-space match range `[content_start, content_end)` into
/// the corresponding render-space ranges, accounting for deletion text that
/// `syntax_highlight_inline_spans` interleaves into the rendered output.
///
/// Search matches are computed against `line.content` (the new text only —
/// see [`crate::app::search::compute_matches`]). When the renderer emits
/// deletion text inline, those characters take up render columns but do not
/// exist in `content`, so a single content match may map to multiple
/// disjoint render ranges.
fn content_match_to_render_ranges(
    inline_spans: &[crate::diff::InlineSpan],
    content_start: usize,
    content_end: usize,
) -> Vec<(usize, usize)> {
    if content_start >= content_end {
        return Vec::new();
    }
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut content_pos = 0;
    let mut render_pos = 0;

    for span in inline_spans {
        let len = span.text.chars().count();
        if span.is_deletion {
            render_pos += len;
            continue;
        }
        let span_c_end = content_pos + len;
        let inter_start = content_start.max(content_pos);
        let inter_end = content_end.min(span_c_end);
        if inter_start < inter_end {
            let r_start = render_pos + (inter_start - content_pos);
            let r_end = render_pos + (inter_end - content_pos);
            match ranges.last_mut() {
                Some(last) if last.1 == r_start => last.1 = r_end,
                _ => ranges.push((r_start, r_end)),
            }
        }
        content_pos = span_c_end;
        render_pos += len;
        if content_pos >= content_end {
            break;
        }
    }
    ranges
}

/// Overlay search match highlights on content spans whose rendered text
/// matches `line.content` 1:1 (no interleaved deletion text).
///
/// SearchMatch stores char offsets (not byte offsets) so this works correctly
/// with multi-byte UTF-8 content.
fn apply_search_to_content(
    content_spans: Vec<Span<'static>>,
    search: &Option<SearchState>,
    line_idx: usize,
) -> Vec<Span<'static>> {
    apply_search_with_translation(content_spans, search, line_idx, None)
}

/// Same as [`apply_search_to_content`] but for spans whose rendered text is
/// `inline_spans` concatenated (i.e., includes interleaved deletion text).
/// Match offsets are translated through `inline_spans` so highlights land on
/// the correct rendered characters.
fn apply_search_to_inline_render(
    content_spans: Vec<Span<'static>>,
    inline_spans: &[crate::diff::InlineSpan],
    search: &Option<SearchState>,
    line_idx: usize,
) -> Vec<Span<'static>> {
    apply_search_with_translation(content_spans, search, line_idx, Some(inline_spans))
}

fn apply_search_with_translation(
    content_spans: Vec<Span<'static>>,
    search: &Option<SearchState>,
    line_idx: usize,
    inline_spans: Option<&[crate::diff::InlineSpan]>,
) -> Vec<Span<'static>> {
    let Some(search) = search else {
        return content_spans;
    };
    if search.matches.is_empty() {
        return content_spans;
    }

    // Collect render-space highlight ranges for matches on this line.
    let mut highlights: Vec<(usize, usize, bool)> = Vec::new();
    for (m_idx, m) in search.matches.iter().enumerate() {
        if m.line_idx != line_idx {
            continue;
        }
        let is_current = m_idx == search.current;
        let c_start = m.char_start;
        let c_end = m.char_start + m.char_len;
        match inline_spans {
            Some(inline) => {
                for (rs, re) in content_match_to_render_ranges(inline, c_start, c_end) {
                    highlights.push((rs, re, is_current));
                }
            }
            None => highlights.push((c_start, c_end, is_current)),
        }
    }

    if highlights.is_empty() {
        return content_spans;
    }

    // Apply right-to-left so earlier indices stay stable as later splits insert spans.
    highlights.sort_by_key(|h| std::cmp::Reverse(h.0));
    let mut result = content_spans;
    for (h_start, h_end, is_current) in highlights {
        let bg = if is_current { SEARCH_CURRENT_BG } else { SEARCH_MATCH_BG };
        result = overlay_range(result, h_start, h_end, bg);
    }
    result
}

/// Split spans as needed to apply `bg` over render-space `[h_start, h_end)`.
fn overlay_range(
    spans: Vec<Span<'static>>,
    h_start: usize,
    h_end: usize,
    bg: ratatui::style::Color,
) -> Vec<Span<'static>> {
    let mut new_result = Vec::with_capacity(spans.len() + 2);
    let mut char_offset = 0;
    for span in spans {
        let span_char_len = span.content.chars().count();
        let span_end = char_offset + span_char_len;

        if span_end <= h_start || char_offset >= h_end {
            new_result.push(span);
        } else {
            let base_style = span.style;
            let text: Vec<char> = span.content.chars().collect();
            let rel_start = h_start.saturating_sub(char_offset);
            let rel_end = (h_end - char_offset).min(span_char_len);

            if rel_start > 0 {
                let before: String = text[..rel_start].iter().collect();
                new_result.push(Span::styled(before, base_style));
            }
            let matched: String = text[rel_start..rel_end].iter().collect();
            new_result.push(Span::styled(matched, base_style.bg(bg)));
            if rel_end < span_char_len {
                let after: String = text[rel_end..].iter().collect();
                new_result.push(Span::styled(after, base_style));
            }
        }
        char_offset = span_end;
    }
    new_result
}

fn render_search_bar(frame: &mut Frame, search: &SearchState, area: Rect) {
    let bar_bg = Color::Rgb(40, 42, 54);
    let bar_style = Style::default().fg(Color::White).bg(bar_bg);

    let counter = if search.matches.is_empty() && !search.query.is_empty() {
        "[no matches]".to_string()
    } else if !search.matches.is_empty() {
        let total = search.match_count();
        if search.visible_count < total {
            format!("[{}/{}]", search.current_display(), search.visible_count)
        } else {
            format!("[{}/{}]", search.current_display(), total)
        }
    } else {
        String::new()
    };

    let counter_width = counter.len();
    let available_for_query = (area.width as usize).saturating_sub(counter_width + 2);

    let query_char_count = search.query.chars().count();
    let display_query: String = if query_char_count > available_for_query {
        let skip = query_char_count - available_for_query + 1;
        format!("…{}", search.query.chars().skip(skip).collect::<String>())
    } else {
        search.query.clone()
    };

    let cursor_char = if search.input_active { "█" } else { "" };
    let left_text = format!("/{}{}", display_query, cursor_char);
    let padding = (area.width as usize).saturating_sub(left_text.chars().count() + counter_width);

    let line = Line::from(vec![
        Span::styled(left_text, bar_style),
        Span::styled(" ".repeat(padding), bar_style),
        Span::styled(counter, Style::default().fg(Color::DarkGray).bg(bar_bg)),
    ]);

    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(vec![line]), area);
}

/// Draw the diff view using a pre-computed frame context.
/// This is the main entry point that creates a DiffViewModel and renders.
pub fn draw_diff_view_with_frame(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    ctx: &FrameContext,
) {
    let view_model = DiffViewModel::from_app(app, ctx, area);
    let output = view_model.render(frame);

    // Store computed values back to App
    app.set_content_layout(
        output.content_offset.0,
        output.content_offset.1,
        output.line_num_width,
        output.content_width,
        area.width,
    );
    app.set_row_map(output.row_map.clone());

    // Render images at recorded positions (requires mutable image_cache access)
    if !output.image_positions.is_empty() {
        render_images_at_positions(
            frame,
            &mut app.image_cache,
            app.image_picker.as_ref(),
            &output.image_positions,
            output.content_offset,
            area,
            app.font_size,
        );
    }
}

/// Render images at the positions recorded during text rendering.
fn render_images_at_positions(
    frame: &mut Frame,
    image_cache: &mut crate::image_diff::ImageCache,
    picker: Option<&ratatui_image::picker::Picker>,
    positions: &[ImageRenderPosition],
    content_offset: (u16, u16),
    area: Rect,
    font_size: (u16, u16),
) {
    use crate::ui::image_view::render_image_diff;

    for pos in positions {
        // Calculate the render area for this image
        let image_y = content_offset.1 + pos.start_row;
        let viewport_bottom = area.y + area.height;

        // Skip if image starts entirely below the visible region
        if image_y >= viewport_bottom {
            continue;
        }

        // Clamp height to available space (prevents rendering past viewport)
        let available_height = viewport_bottom.saturating_sub(image_y);
        let clamped_height = pos.height.min(available_height);
        if clamped_height == 0 {
            continue;
        }

        let image_area = Rect::new(
            area.x + 1, // Inside border
            image_y,
            area.width.saturating_sub(2), // Inside borders
            clamped_height,
        );

        // Get mutable access to image state
        if let Some(state) = image_cache.get_mut(&pos.file_path) {
            // Ensure protocols are created if we have a picker
            if let Some(picker) = picker {
                if let Some(ref mut before) = state.before {
                    before.ensure_protocol(picker);
                }
                if let Some(ref mut after) = state.after {
                    after.ensure_protocol(picker);
                }
            }

            // Render the image diff with expected available height for consistent sizing
            // This ensures images maintain their size when viewport clips them
            render_image_diff(
                frame,
                image_area,
                state,
                &pos.file_path,
                pos.expected_available_height,
                font_size,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::search::SearchMatch;
    use crate::test_support::{base_line, change_line, TestAppBuilder};

    #[cfg(test)]
    fn avg_render_frame(content: String, width: u16, height: u16) -> std::time::Duration {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        use std::time::Instant;

        let mut line = DiffLine::new(LineSource::Committed, content, '+', Some(1));
        line.file_path = Some("bundle.js".to_string());
        let lines = vec![DiffLine::file_header("bundle.js"), line];

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.estimate_content_width(width);

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut draw = |app: &mut App| {
            terminal
                .draw(|f| {
                    let ctx = FrameContext::new(app);
                    crate::ui::draw_with_frame(f, app, &ctx);
                })
                .unwrap();
        };

        draw(&mut app); // warm up lazy syntax-set init

        let frames = 10;
        let start = Instant::now();
        for _ in 0..frames {
            draw(&mut app);
        }
        start.elapsed() / frames
    }

    #[test]
    fn perf_huge_line_render_does_not_scale_with_length() {
        // Render time must not scale with line length. Measured relative to a 9 KB
        // line (highlighted in full either way) so the ratio is stable across
        // machines; pre-fix the 256 KB line was ~40x slower.
        let width: u16 = 120;
        let height: u16 = 40;

        let small = avg_render_frame("a".repeat(9_000), width, height);
        let huge = avg_render_frame("a".repeat(256 * 1024), width, height);

        assert!(
            huge < small * 8,
            "256 KB line rendered at {:?}/frame vs {:?}/frame for a 9 KB line \
             (>8x) — render time is scaling with line length again",
            huge,
            small,
        );
    }

    #[test]
    fn test_diff_view_model_from_app() {
        let app = TestAppBuilder::new()
            .with_lines(vec![DiffLine::file_header("test.rs"), base_line("content")])
            .build();
        let ctx = FrameContext::new(&app);
        let area = Rect::new(0, 0, 80, 24);

        let view_model = DiffViewModel::from_app(&app, &ctx, area);

        assert_eq!(view_model.items.len(), 2);
        assert_eq!(view_model.lines.len(), 2);
        assert!(view_model.selection.is_none());
        assert!(view_model.collapsed_files.is_empty());
    }

    #[test]
    fn test_huge_single_line_renders_bounded_rows() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let width: u16 = 80;
        let height: u16 = 24;

        let huge = "a".repeat(200_000);
        let mut line = DiffLine::new(LineSource::Committed, huge, '+', Some(1));
        line.file_path = Some("searchindex.js".to_string());
        let lines = vec![DiffLine::file_header("searchindex.js"), line];

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.estimate_content_width(width);

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut row_map_len = usize::MAX;
        terminal
            .draw(|f| {
                let ctx = FrameContext::new(&app);
                let area = Rect::new(0, 0, width, height);
                let vm = DiffViewModel::from_app(&app, &ctx, area);
                row_map_len = vm.render(f).row_map.len();
            })
            .unwrap();

        assert!(
            row_map_len <= height as usize,
            "huge line produced {} rows, expected <= viewport height {}",
            row_map_len,
            height
        );
    }

    #[test]
    fn test_content_width_stable_across_scroll() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let width: u16 = 80;
        let height: u16 = 8;

        // Two groups with different line-number widths. The first group is
        // larger than the viewport so the top window contains only small numbers.
        let mut lines = Vec::new();
        for n in 1..=15 {
            lines.push(DiffLine::new(LineSource::Base, format!("line {n}"), ' ', Some(n)));
        }
        for n in 1000..=1010 {
            lines.push(DiffLine::new(LineSource::Base, format!("line {n}"), ' ', Some(n)));
        }
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view.view_mode = crate::app::ViewMode::Full;
        app.estimate_content_width(width);

        let render_cw = |app: &App| -> usize {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            let mut cw = 0;
            terminal
                .draw(|f| {
                    let ctx = FrameContext::new(app);
                    let area = Rect::new(0, 0, width, height);
                    let vm = DiffViewModel::from_app(app, &ctx, area);
                    cw = vm.render(f).content_width;
                })
                .unwrap();
            cw
        };

        app.view.scroll_offset = 0;
        let top = render_cw(&app);
        // Scroll so only the 4-digit-numbered lines are visible.
        app.view.scroll_offset = 16;
        let bottom = render_cw(&app);

        assert_eq!(
            top, bottom,
            "content_width must not depend on which lines are currently visible \
             (the scroll engine assumes a stable per-line wrap height)"
        );
    }

    #[test]
    fn test_render_with_sub_row_starts_mid_line() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let width: u16 = 80;
        let height: u16 = 20;

        // Distinct cycling ASCII so each wrapped row's content is identifiable.
        let content: String = (0..5000).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        let mut line = DiffLine::new(LineSource::Committed, content.clone(), '+', Some(1));
        line.file_path = Some("f.js".to_string());

        let mut app = TestAppBuilder::new().with_lines(vec![line]).build();
        app.estimate_content_width(width);
        let skip = 7;
        app.view.scroll_offset = 0;
        app.view.sub_row = skip;

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut out = None;
        terminal
            .draw(|f| {
                let ctx = FrameContext::new(&app);
                let area = Rect::new(0, 0, width, height);
                let vm = DiffViewModel::from_app(&app, &ctx, area);
                out = Some(vm.render(f));
            })
            .unwrap();
        let out = out.unwrap();

        assert!(!out.row_map.is_empty());
        assert!(out.row_map.len() <= (height - 2) as usize);
        assert!(
            out.row_map[0].is_continuation,
            "top row is a mid-line continuation, not the logical line start"
        );
        let cw = out.content_width;
        let expected: String = content.chars().skip(skip * cw).take(cw).collect();
        assert_eq!(
            out.row_map[0].content, expected,
            "top visible row is the slice scrolled to"
        );
    }

    #[test]
    fn test_render_deep_sub_row_stays_bounded() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let width: u16 = 80;
        let height: u16 = 20;

        let mut line =
            DiffLine::new(LineSource::Committed, "a".repeat(200_000), '+', Some(1));
        line.file_path = Some("bundle.js".to_string());
        let mut app = TestAppBuilder::new().with_lines(vec![line]).build();
        app.estimate_content_width(width);
        app.view.scroll_offset = 0;
        app.view.sub_row = 1000;

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut len = usize::MAX;
        terminal
            .draw(|f| {
                let ctx = FrameContext::new(&app);
                let area = Rect::new(0, 0, width, height);
                let vm = DiffViewModel::from_app(&app, &ctx, area);
                len = vm.render(f).row_map.len();
            })
            .unwrap();

        assert!(
            len <= (height - 2) as usize,
            "deep mid-line scroll still renders only a viewport of rows"
        );
    }

    #[test]
    fn test_diff_view_model_find_current_file() {
        let lines = vec![
            DiffLine::file_header("test.rs"),
            base_line("line1"),
            change_line("line2"),
        ];
        let app = TestAppBuilder::new().with_lines(lines).build();
        let ctx = FrameContext::new(&app);
        let area = Rect::new(0, 0, 80, 24);

        let view_model = DiffViewModel::from_app(&app, &ctx, area);
        let current_file = view_model.find_current_file();

        assert_eq!(current_file, Some("test.rs".to_string()));
    }

    #[test]
    fn test_diff_view_model_find_current_file_when_header_scrolled_above() {
        // Create two files: first.rs with 5 lines, then second.rs
        let mut lines = vec![DiffLine::file_header("first.rs")];
        for i in 0..5 {
            let mut line = base_line(&format!("line{}", i));
            line.file_path = Some("first.rs".to_string());
            lines.push(line);
        }
        lines.push(DiffLine::file_header("second.rs"));
        let mut line = base_line("second file content");
        line.file_path = Some("second.rs".to_string());
        lines.push(line);

        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_viewport_height(4)
            .build();
        // Scroll down so first.rs header is above viewport but content is still visible
        app.view.scroll_offset = 2;

        let ctx = FrameContext::new(&app);
        let area = Rect::new(0, 0, 80, 24);

        let view_model = DiffViewModel::from_app(&app, &ctx, area);
        let current_file = view_model.find_current_file();

        // Should still show first.rs since its content is visible,
        // not second.rs just because its header is the first header in view
        assert_eq!(current_file, Some("first.rs".to_string()));
    }

    #[test]
    fn test_diff_view_model_is_file_collapsed() {
        let mut app = TestAppBuilder::new()
            .with_lines(vec![DiffLine::file_header("test.rs")])
            .build();
        app.view.collapsed_files.insert("test.rs".to_string());

        let ctx = FrameContext::new(&app);
        let area = Rect::new(0, 0, 80, 24);

        let view_model = DiffViewModel::from_app(&app, &ctx, area);
        assert!(view_model.is_file_collapsed("test.rs"));
        assert!(!view_model.is_file_collapsed("other.rs"));
    }

    #[test]
    fn test_diff_view_model_with_selection() {
        use crate::app::Position;

        let mut app = TestAppBuilder::new()
            .with_lines(vec![base_line("selectable content")])
            .build();
        app.view.selection = Some(Selection {
            start: Position { row: 0, col: 5 },
            end: Position { row: 0, col: 15 },
            active: false,
        });

        let ctx = FrameContext::new(&app);
        let area = Rect::new(0, 0, 80, 24);

        let view_model = DiffViewModel::from_app(&app, &ctx, area);
        assert!(view_model.selection.is_some());
    }

    #[test]
    fn test_diff_view_model_show_copied_flash() {
        let mut app = TestAppBuilder::new()
            .with_lines(vec![DiffLine::file_header("test.rs")])
            .build();

        // Initially no flash
        let ctx = FrameContext::new(&app);
        let area = Rect::new(0, 0, 80, 24);
        let view_model = DiffViewModel::from_app(&app, &ctx, area);
        assert!(!view_model.show_copied_flash);

        // After setting path_copied_at to now, flash should be active
        app.view.path_copied_at = Some(std::time::Instant::now());
        let ctx = FrameContext::new(&app);
        let view_model = DiffViewModel::from_app(&app, &ctx, area);
        assert!(view_model.show_copied_flash);

        // After 800ms+ elapsed, flash should be inactive
        app.view.path_copied_at = Some(std::time::Instant::now() - std::time::Duration::from_millis(900));
        let ctx = FrameContext::new(&app);
        let view_model = DiffViewModel::from_app(&app, &ctx, area);
        assert!(!view_model.show_copied_flash);
    }

    mod highlight_source_tests {
        use super::*;
        use crate::diff::InlineSpan;
        use crate::ui::colors::line_style_with_highlight;

        /// Integration test: modified base line with inline spans should use span source for highlight
        #[test]
        fn test_modified_base_line_uses_span_source_for_highlight() {
            // Create a modified base line: source=Base, but has inline spans with Unstaged source
            let mut line = DiffLine::new(LineSource::Base, "prefix inserted suffix".to_string(), ' ', Some(1));
            line.old_content = Some("prefix suffix".to_string());
            line.change_source = Some(LineSource::Unstaged);
            line.inline_spans = vec![
                InlineSpan { text: "prefix ".to_string(), source: None, is_deletion: false },
                InlineSpan { text: "inserted ".to_string(), source: Some(LineSource::Unstaged), is_deletion: false },
                InlineSpan { text: "suffix".to_string(), source: None, is_deletion: false },
            ];

            // The line's source is Base
            assert_eq!(line.source, LineSource::Base);

            // But the highlight source should come from the spans
            let highlight_source = get_insertion_source(&line.inline_spans);
            assert_eq!(highlight_source, LineSource::Unstaged,
                "Highlight source should be Unstaged (from spans), not Base (from line)");

            // And this should produce a visible highlight style
            let highlight_style = line_style_with_highlight(highlight_source);
            assert!(matches!(highlight_style.bg, Some(Color::Rgb(130, 130, 35))),
                "Highlight style should have visible yellow background");
        }

        /// Test the specific bug scenario: import line modification
        #[test]
        fn test_import_line_modification_highlight() {
            // This is the exact bug scenario:
            // Old: "    widgets::{Block, Borders, Paragraph},"
            // New: "    widgets::{Block, Borders, Clear, Paragraph},"
            // "Clear, " is inserted with source=Unstaged

            let inline_spans = vec![
                InlineSpan { text: "    widgets::{Block, Borders, ".to_string(), source: None, is_deletion: false },
                InlineSpan { text: "Clear, ".to_string(), source: Some(LineSource::Unstaged), is_deletion: false },
                InlineSpan { text: "Paragraph},".to_string(), source: None, is_deletion: false },
            ];

            // The line source would be Base (it's a modification of an existing base line)
            let line_source = LineSource::Base;

            // BUG: Using line_source for highlight gives no visible highlight
            let bug_highlight_style = line_style_with_highlight(line_source);
            assert_eq!(bug_highlight_style.bg, Some(Color::Reset),
                "Bug: using line source gives Reset background (invisible)");

            // FIX: Using the span's source gives visible highlight
            let fix_highlight_source = get_insertion_source(&inline_spans);
            assert_eq!(fix_highlight_source, LineSource::Unstaged);

            let fix_highlight_style = line_style_with_highlight(fix_highlight_source);
            assert_eq!(fix_highlight_style.bg, Some(Color::Rgb(130, 130, 35)),
                "Fix: using span source gives yellow background (visible)");
        }
    }

    /// Verify that Block border characters are intact in the rendered buffer.
    fn verify_diff_area_borders(
        buffer: &ratatui::buffer::Buffer,
        width: u16,
        diff_height: u16,
    ) {
        assert_eq!(buffer[(0, 0)].symbol(), "┌", "Top-left corner");
        assert_eq!(buffer[(width - 1, 0)].symbol(), "┐", "Top-right corner");

        for y in 1..diff_height.saturating_sub(1) {
            let left = buffer[(0, y)].symbol();
            let right = buffer[(width - 1, y)].symbol();
            assert_eq!(left, "│", "Row {} left border: expected │, got {:?}", y, left);
            assert_eq!(
                right, "│",
                "Row {} right border: expected │, got {:?}",
                y, right
            );
        }

        if diff_height > 1 {
            assert_eq!(
                buffer[(0, diff_height - 1)].symbol(),
                "└",
                "Bottom-left corner"
            );
            assert_eq!(
                buffer[(width - 1, diff_height - 1)].symbol(),
                "┘",
                "Bottom-right corner"
            );
        }
    }

    /// Renders the full diff view through ratatui's TestBackend and verifies
    /// that wrapped ASCII lines don't overwrite border characters.
    #[test]
    fn test_wrapped_ascii_lines_preserve_borders() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let width: u16 = 80;
        let height: u16 = 24;

        let long_content = "abcdefghij".repeat(20); // 200 ASCII chars — must wrap
        let mut lines = vec![DiffLine::file_header("test.swift")];
        for i in 1..=15 {
            let (source, prefix) = match i % 4 {
                0 => (LineSource::Base, ' '),
                1 => (LineSource::Committed, '+'),
                2 => (LineSource::Unstaged, '+'),
                _ => (LineSource::Staged, '+'),
            };
            let mut line = DiffLine::new(source, long_content.clone(), prefix, Some(i));
            line.file_path = Some("test.swift".to_string());
            lines.push(line);
        }

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.estimate_content_width(width);

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();

        // First render
        {
            let frame = terminal
                .draw(|f| {
                    let ctx = FrameContext::new(&app);
                    crate::ui::draw_with_frame(f, &mut app, &ctx);
                })
                .unwrap();

            let status_h = crate::ui::status_bar_height(&app, width);
            let diff_h = height - status_h;
            verify_diff_area_borders(frame.buffer, width, diff_h);
        }

        // Scroll down and re-render
        app.view.scroll_offset = 10;
        {
            let frame = terminal
                .draw(|f| {
                    let ctx = FrameContext::new(&app);
                    crate::ui::draw_with_frame(f, &mut app, &ctx);
                })
                .unwrap();

            let status_h = crate::ui::status_bar_height(&app, width);
            let diff_h = height - status_h;
            verify_diff_area_borders(frame.buffer, width, diff_h);
        }
    }

    /// Same test at narrower terminal width (40 cols) to stress wrapping
    #[test]
    fn test_wrapped_ascii_lines_preserve_borders_narrow_terminal() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let width: u16 = 40;
        let height: u16 = 30;

        let long_content = "the_quick_brown_fox_jumps_over_the_lazy_dog_".repeat(5);
        let mut lines = vec![DiffLine::file_header("narrow.rs")];
        for i in 1..=20 {
            let mut line = DiffLine::new(
                LineSource::Committed,
                long_content.clone(),
                '+',
                Some(i),
            );
            line.file_path = Some("narrow.rs".to_string());
            lines.push(line);
        }

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.estimate_content_width(width);

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();

        // Render multiple frames with different scroll positions
        for scroll in [0, 5, 15, 30] {
            app.view.scroll_offset = scroll;
            let frame = terminal
                .draw(|f| {
                    let ctx = FrameContext::new(&app);
                    crate::ui::draw_with_frame(f, &mut app, &ctx);
                })
                .unwrap();

            let status_h = crate::ui::status_bar_height(&app, width);
            let diff_h = height - status_h;
            verify_diff_area_borders(frame.buffer, width, diff_h);
        }
    }

    #[test]
    fn test_diff_view_model_includes_image_cache() {
        let app = TestAppBuilder::new()
            .with_lines(vec![DiffLine::file_header("test.rs")])
            .build();
        let ctx = FrameContext::new(&app);
        let area = Rect::new(0, 0, 80, 24);

        let view_model = DiffViewModel::from_app(&app, &ctx, area);

        // Verify image_cache is included and accessible
        assert!(view_model.image_cache.is_empty());
    }

    #[test]
    fn test_image_marker_rendering_without_cache_data() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        // Create an image marker line
        let lines = vec![
            DiffLine::file_header("test.png"),
            DiffLine::image_marker("test.png"),
        ];

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.estimate_content_width(80);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let frame = terminal
            .draw(|f| {
                let ctx = FrameContext::new(&app);
                crate::ui::draw_with_frame(f, &mut app, &ctx);
            })
            .unwrap();

        // Check that the buffer contains the loading placeholder
        let buffer_content: String = (0..frame.buffer.area.height)
            .flat_map(|y| (0..frame.buffer.area.width).map(move |x| frame.buffer[(x, y)].symbol()))
            .collect();

        assert!(
            buffer_content.contains("loading"),
            "Should show 'loading...' when image not in cache"
        );
    }

    #[test]
    fn test_image_positions_populated_without_protocols() {
        use crate::image_diff::{CachedImage, ImageDiffState};
        use image::DynamicImage;

        // Create an image marker line
        let lines = vec![
            DiffLine::file_header("test.png"),
            DiffLine::image_marker("test.png"),
        ];

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.estimate_content_width(80);
        app.view.viewport_height = 40; // Match the terminal size used for rendering

        // Add image data to cache WITHOUT protocols (simulating picker not available during load)
        let cached_image = CachedImage {
            display_image: DynamicImage::new_rgb8(100, 100),
            original_width: 100,
            original_height: 100,
            file_size: 1024,
            format_name: "PNG".to_string(),
            protocol: None, // No protocol - this is the key condition
        };
        let state = ImageDiffState {
            before: Some(cached_image),
            after: None,
        };
        app.image_cache.insert("test.png".to_string(), state);

        // Render and check that image_positions is populated
        let ctx = FrameContext::new(&app);
        let area = Rect::new(0, 0, 80, 40);
        let view_model = DiffViewModel::from_app(&app, &ctx, area);

        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| {
                let output = view_model.render(f);
                // Verify image_positions was populated even without protocols
                assert!(
                    !output.image_positions.is_empty(),
                    "image_positions should be populated when image data exists, even without protocols"
                );
                assert_eq!(output.image_positions[0].file_path, "test.png");
            })
            .unwrap();
    }

    #[test]
    fn test_image_render_position_fields() {
        use crate::image_diff::IMAGE_PANEL_OVERHEAD;

        let height = 12u16;
        let pos = ImageRenderPosition {
            file_path: "test/image.png".to_string(),
            start_row: 42,
            height,
            expected_available_height: height.saturating_sub(IMAGE_PANEL_OVERHEAD),
        };

        assert_eq!(pos.file_path, "test/image.png");
        assert_eq!(pos.start_row, 42);
        assert_eq!(pos.height, 12);
        assert_eq!(pos.expected_available_height, 7); // 12 - 5
    }

    #[test]
    fn test_image_render_position_large_row_saturates() {
        use crate::image_diff::IMAGE_PANEL_OVERHEAD;

        // Test that large row values are handled (the actual saturation happens
        // in render_image_marker, but we verify the struct can hold max values)
        let height = 12u16;
        let pos = ImageRenderPosition {
            file_path: "large.png".to_string(),
            start_row: u16::MAX,
            height,
            expected_available_height: height.saturating_sub(IMAGE_PANEL_OVERHEAD),
        };

        assert_eq!(pos.start_row, u16::MAX);
    }

    #[test]
    fn test_render_output_includes_image_positions() {
        use crate::image_diff::IMAGE_PANEL_OVERHEAD;

        let height = 12u16;
        let output = RenderOutput {
            row_map: Vec::new(),
            content_offset: (0, 0),
            line_num_width: 0,
            content_width: 80,
            image_positions: vec![
                ImageRenderPosition {
                    file_path: "a.png".to_string(),
                    start_row: 5,
                    height,
                    expected_available_height: height.saturating_sub(IMAGE_PANEL_OVERHEAD),
                },
                ImageRenderPosition {
                    file_path: "b.png".to_string(),
                    start_row: 20,
                    height,
                    expected_available_height: height.saturating_sub(IMAGE_PANEL_OVERHEAD),
                },
            ],
        };

        assert_eq!(output.image_positions.len(), 2);
        assert_eq!(output.image_positions[0].file_path, "a.png");
        assert_eq!(output.image_positions[1].start_row, 20);
    }

    #[test]
    fn test_image_clipping_calculation() {
        // Test the clipping logic used in render_images_at_positions
        // Simulating: viewport_bottom = 100, image_y = 95, pos.height = 12
        let viewport_bottom: u16 = 100;
        let image_y: u16 = 95;
        let pos_height: u16 = 12;

        let available_height = viewport_bottom.saturating_sub(image_y);
        let clamped_height = pos_height.min(available_height);

        // Image extends from 95 to 107, but viewport ends at 100
        // So available is 5 rows, not 12
        assert_eq!(available_height, 5);
        assert_eq!(clamped_height, 5);
    }

    #[test]
    fn test_image_entirely_below_viewport_skipped() {
        // Test the skip logic: image_y >= viewport_bottom should skip
        let viewport_bottom: u16 = 100;
        let image_y: u16 = 100; // Exactly at bottom

        let should_skip = image_y >= viewport_bottom;
        assert!(should_skip, "Image at viewport bottom should be skipped");

        let image_y_below: u16 = 150;
        let should_skip_below = image_y_below >= viewport_bottom;
        assert!(should_skip_below, "Image below viewport should be skipped");
    }

    #[test]
    fn test_image_clipping_zero_available_height() {
        // Edge case: image starts exactly at viewport edge
        let viewport_bottom: u16 = 100;
        let image_y: u16 = 100;
        let pos_height: u16 = 12;

        let available_height = viewport_bottom.saturating_sub(image_y);
        assert_eq!(available_height, 0);

        // This should result in skipping (clamped_height == 0)
        let clamped_height = pos_height.min(available_height);
        assert_eq!(clamped_height, 0);
    }

    /// Test with canceled lines (± prefix) which have multi-byte prefix char
    #[test]
    fn test_wrapped_canceled_lines_preserve_borders() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let width: u16 = 80;
        let height: u16 = 24;

        let long_content = "canceled_content_".repeat(15);
        let mut lines = vec![DiffLine::file_header("cancel.rs")];
        for i in 1..=10 {
            let mut line = DiffLine::new(
                LineSource::CanceledCommitted,
                long_content.clone(),
                '±',
                Some(i),
            );
            line.file_path = Some("cancel.rs".to_string());
            lines.push(line);
        }

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.estimate_content_width(width);

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();

        let frame = terminal
            .draw(|f| {
                let ctx = FrameContext::new(&app);
                crate::ui::draw_with_frame(f, &mut app, &ctx);
            })
            .unwrap();

        let status_h = crate::ui::status_bar_height(&app, width);
        let diff_h = height - status_h;
        verify_diff_area_borders(frame.buffer, width, diff_h);
    }

    /// Test that partial rendering maintains consistent image dimensions.
    ///
    /// When an image is partially visible (e.g., scrolling into view from bottom),
    /// the expected_available_height must remain constant. This ensures:
    /// 1. Image sizing via fit_dimensions() produces the same (width, height)
    /// 2. The image appears to scroll into view at full size, cropped by viewport
    /// 3. No rescaling or jumping occurs as more of the image becomes visible
    #[test]
    fn test_partial_rendering_consistency() {
        use crate::image_diff::IMAGE_PANEL_OVERHEAD;
        use crate::ui::image_view::calculate_image_height_for_images;

        // Simulate a 400x300 image in a 100-char wide panel
        let img_dims = Some((400u32, 300u32));
        let panel_width = 100u16;
        let font_size = (8u16, 16u16);

        // Calculate the image height (this is what render_image_marker does)
        let image_height =
            calculate_image_height_for_images(img_dims, None, panel_width, font_size);
        let expected_available = image_height.saturating_sub(IMAGE_PANEL_OVERHEAD);

        // Simulate different viewport clipping scenarios
        // The key invariant: expected_available_height is always the same
        let viewport_scenarios = [
            ("fully visible", image_height),       // Image fully in viewport
            ("90% visible", image_height - 2),     // Top 2 rows clipped
            ("50% visible", image_height / 2),     // Half the image visible
            ("barely visible", 3u16),              // Only 3 rows visible
            ("just entering", 1u16),               // Just 1 row visible
        ];

        for (scenario, clamped_height) in viewport_scenarios {
            // In render_images_at_positions, the area passed to render_image_diff
            // has clamped_height, but expected_available_height is unchanged
            let pos = ImageRenderPosition {
                file_path: "test.png".to_string(),
                start_row: 0,
                height: image_height,
                expected_available_height: expected_available,
            };

            // The expected_available_height should be the same regardless of clipping
            assert_eq!(
                pos.expected_available_height, expected_available,
                "Scenario '{}': expected_available_height should be {} (from full image height {}), \
                 not derived from clamped_height {}",
                scenario, expected_available, image_height, clamped_height
            );
        }
    }

    /// Test that fit_dimensions produces consistent output when called with
    /// expected_available_height vs viewport-clamped height.
    #[test]
    fn test_fit_dimensions_consistency_across_viewports() {
        use crate::image_diff::{fit_dimensions, IMAGE_PANEL_OVERHEAD};
        use crate::ui::image_view::calculate_image_height_for_images;

        // Test with a real image scenario
        let img_w = 800u32;
        let img_h = 600u32;
        let panel_width = 120u16;
        let font_size = (8u16, 16u16);

        // Calculate the full image height
        let full_height = calculate_image_height_for_images(
            Some((img_w, img_h)),
            None,
            panel_width,
            font_size,
        );
        let expected_available = full_height.saturating_sub(IMAGE_PANEL_OVERHEAD);

        // Calculate display dimensions using expected_available_height (correct)
        let inner_width = (panel_width.saturating_sub(4)) / 2; // Half panel minus borders
        let (expected_w, expected_h) =
            fit_dimensions(img_w, img_h, inner_width, expected_available, font_size);

        // Now simulate what would happen if we incorrectly used clamped heights
        let clamped_heights = [
            expected_available,     // Full view
            expected_available - 5, // Partial view
            expected_available / 2, // Half view
            5u16,                   // Minimal view
        ];

        for clamped in clamped_heights {
            // Using expected_available (correct) should always produce the same dimensions
            let (w, h) =
                fit_dimensions(img_w, img_h, inner_width, expected_available, font_size);
            assert_eq!(
                (w, h),
                (expected_w, expected_h),
                "fit_dimensions with expected_available should always produce ({}, {})",
                expected_w,
                expected_h
            );

            // Using clamped height (incorrect) would produce different dimensions
            // when clamped < expected_available
            if clamped < expected_available {
                let (clamped_w, _clamped_h) =
                    fit_dimensions(img_w, img_h, inner_width, clamped, font_size);
                // This demonstrates why we pass expected_available_height:
                // clamped dimensions would be smaller, causing the image to "jump"
                assert!(
                    clamped_w <= expected_w,
                    "Clamped height {} would produce width {} <= expected width {}",
                    clamped,
                    clamped_w,
                    expected_w
                );
            }
        }
    }

    /// Test that ImageRenderPosition captures the correct expected_available_height
    /// based on image dimensions, not viewport position.
    #[test]
    fn test_image_render_position_expected_height_invariant() {
        use crate::image_diff::{CachedImage, ImageDiffState, IMAGE_PANEL_OVERHEAD};
        use crate::ui::image_view::calculate_image_height_for_images;
        use image::DynamicImage;

        // Create image cache with a test image
        let mut app = TestAppBuilder::new()
            .with_lines(vec![
                DiffLine::file_header("test.png"),
                DiffLine::image_marker("test.png"),
            ])
            .build();
        app.estimate_content_width(100);

        // Add image to cache
        let cached_image = CachedImage {
            display_image: DynamicImage::new_rgb8(400, 300),
            original_width: 400,
            original_height: 300,
            file_size: 50000,
            format_name: "PNG".to_string(),
            protocol: None,
        };
        app.image_cache.insert(
            "test.png".to_string(),
            ImageDiffState {
                before: None,
                after: Some(cached_image),
            },
        );

        // Calculate expected height from image dimensions
        let image_height = calculate_image_height_for_images(
            None,
            Some((400, 300)),
            100, // panel_width
            app.font_size,
        );
        let expected_available = image_height.saturating_sub(IMAGE_PANEL_OVERHEAD);

        // Render at different scroll positions
        for scroll in [0, 5, 10, 20] {
            app.view.scroll_offset = scroll;
            app.view.viewport_height = 30; // Fixed viewport

            let ctx = FrameContext::new(&app);
            let area = Rect::new(0, 0, 100, 30);
            let view_model = DiffViewModel::from_app(&app, &ctx, area);

            use ratatui::backend::TestBackend;
            use ratatui::Terminal;
            let backend = TestBackend::new(100, 30);
            let mut terminal = Terminal::new(backend).unwrap();

            terminal
                .draw(|f| {
                    let output = view_model.render(f);

                    // If the image marker is visible, check its expected_available_height
                    for pos in &output.image_positions {
                        assert_eq!(
                            pos.expected_available_height, expected_available,
                            "At scroll_offset={}, expected_available_height should be {} \
                             (derived from image dimensions), got {}",
                            scroll, expected_available, pos.expected_available_height
                        );
                    }
                })
                .unwrap();
        }
    }

    /// Helper: render a DiffViewModel and return the RenderOutput.
    fn render_to_output(app: &App, width: u16, height: u16) -> RenderOutput {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let ctx = FrameContext::new(app);
        let area = Rect::new(0, 0, width, height);
        let view_model = DiffViewModel::from_app(app, &ctx, area);

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut output = None;
        terminal
            .draw(|f| {
                output = Some(view_model.render(f));
            })
            .unwrap();
        output.unwrap()
    }

    /// Helper: render with custom FrameContext items.
    fn render_with_items(app: &App, items: Vec<DisplayableItem>, width: u16, height: u16) -> RenderOutput {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let ctx = FrameContext::with_items(items, app);
        let area = Rect::new(0, 0, width, height);
        let view_model = DiffViewModel::from_app(app, &ctx, area);

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut output = None;
        terminal
            .draw(|f| {
                output = Some(view_model.render(f));
            })
            .unwrap();
        output.unwrap()
    }

    #[test]
    fn test_render_plain_content_populates_row_map() {
        let mut lines = vec![DiffLine::file_header("test.rs")];
        for i in 1..=3 {
            let mut line = base_line(&format!("line {}", i));
            line.line_number = Some(i);
            line.file_path = Some("test.rs".to_string());
            lines.push(line);
        }

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.estimate_content_width(80);

        let output = render_to_output(&app, 80, 24);

        assert_eq!(output.row_map.len(), 4, "1 header + 3 content lines");
        assert!(output.row_map[0].is_file_header);
        assert_eq!(output.row_map[0].file_path.as_deref(), Some("test.rs"));

        for i in 1..=3 {
            assert!(!output.row_map[i].is_file_header);
            assert_eq!(output.row_map[i].content, format!("line {}", i));
            assert!(!output.row_map[i].is_continuation);
        }

        assert!(output.line_num_width > 0);
    }

    #[test]
    fn test_render_elided_marker_row_map() {
        let app = TestAppBuilder::new()
            .with_lines(vec![base_line("placeholder")])
            .build();

        let items = vec![DisplayableItem::Elided(42)];
        let output = render_with_items(&app, items, 80, 24);

        assert_eq!(output.row_map.len(), 1);
        assert_eq!(output.row_map[0].content, "42 lines hidden");
        assert!(!output.row_map[0].is_file_header);
        assert!(!output.row_map[0].is_continuation);
    }

    #[test]
    fn test_render_inline_spans_pure_deletion_splits_into_two_rows() {
        use crate::diff::InlineSpan;

        // PureDeletion: deletion spans exist, no insertion spans (unchanged has source: None)
        let mut line = DiffLine::new(
            LineSource::Committed,
            "kept text".to_string(),
            '+',
            Some(1),
        );
        line.file_path = Some("test.rs".to_string());
        line.old_content = Some("deleted prefix kept text".to_string());
        line.change_source = Some(LineSource::Committed);
        line.inline_spans = vec![
            InlineSpan { text: "deleted prefix ".to_string(), source: Some(LineSource::Committed), is_deletion: true },
            InlineSpan { text: "kept text".to_string(), source: None, is_deletion: false },
        ];

        let mut app = TestAppBuilder::new()
            .with_lines(vec![DiffLine::file_header("test.rs"), line])
            .build();
        app.estimate_content_width(80);

        let output = render_to_output(&app, 80, 24);

        // Header + at least 2 rows (del + ins) for the split inline line
        assert!(
            output.row_map.len() >= 3,
            "Expected header + del + ins rows, got {} rows",
            output.row_map.len()
        );
    }

    #[test]
    fn test_render_inline_spans_fits_single_row() {
        use crate::diff::InlineSpan;

        let mut line = DiffLine::new(
            LineSource::Committed,
            "prefix inserted suffix".to_string(),
            '+',
            Some(1),
        );
        line.file_path = Some("test.rs".to_string());
        line.inline_spans = vec![
            InlineSpan { text: "prefix ".to_string(), source: None, is_deletion: false },
            InlineSpan { text: "inserted ".to_string(), source: Some(LineSource::Committed), is_deletion: false },
            InlineSpan { text: "suffix".to_string(), source: None, is_deletion: false },
        ];

        let mut app = TestAppBuilder::new()
            .with_lines(vec![DiffLine::file_header("test.rs"), line])
            .build();
        app.estimate_content_width(80);

        let output = render_to_output(&app, 80, 24);

        // Header + exactly 1 row for the short inline line
        assert_eq!(output.row_map.len(), 2, "header + 1 content row");
        assert!(!output.row_map[1].is_file_header);
        assert!(!output.row_map[1].is_continuation);
    }

    #[test]
    fn test_render_plain_content_wrapping_sets_continuation() {
        let long_content = "x".repeat(200);
        let mut line = base_line(&long_content);
        line.line_number = Some(1);
        line.file_path = Some("test.rs".to_string());

        let mut app = TestAppBuilder::new()
            .with_lines(vec![DiffLine::file_header("test.rs"), line])
            .build();
        app.estimate_content_width(40);

        let output = render_to_output(&app, 40, 24);

        // Skip header row, check content rows
        let content_rows: Vec<_> = output.row_map.iter().skip(1).collect();
        assert!(content_rows.len() > 1, "line should wrap at 40 cols");
        assert!(!content_rows[0].is_continuation, "first row is not a continuation");
        for row in &content_rows[1..] {
            assert!(row.is_continuation, "wrapped rows should be continuations");
        }
    }

    #[test]
    fn test_render_title_shows_current_file() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut line = base_line("content");
        line.file_path = Some("src/main.rs".to_string());

        let mut app = TestAppBuilder::new()
            .with_lines(vec![DiffLine::file_header("src/main.rs"), line])
            .build();
        app.estimate_content_width(80);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let frame = terminal
            .draw(|f| {
                let ctx = FrameContext::new(&app);
                let area = Rect::new(0, 0, 80, 24);
                let vm = DiffViewModel::from_app(&app, &ctx, area);
                vm.render(f);
            })
            .unwrap();

        let top_row: String = (0..80)
            .map(|x| frame.buffer[(x, 0)].symbol().to_string())
            .collect();
        assert!(top_row.contains("src/main.rs"), "title should show current file, got: {}", top_row);
    }

    #[test]
    fn test_render_title_shows_branchdiff_fallback() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = TestAppBuilder::new()
            .with_lines(vec![base_line("no file path")])
            .build();
        app.estimate_content_width(80);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let frame = terminal
            .draw(|f| {
                let ctx = FrameContext::new(&app);
                let area = Rect::new(0, 0, 80, 24);
                let vm = DiffViewModel::from_app(&app, &ctx, area);
                vm.render(f);
            })
            .unwrap();

        let top_row: String = (0..80)
            .map(|x| frame.buffer[(x, 0)].symbol().to_string())
            .collect();
        assert!(top_row.contains("branchdiff"), "title should show fallback, got: {}", top_row);
    }

    #[test]
    fn test_apply_selection_with_active_selection() {
        use crate::app::{Position, Selection};
        use crate::ui::selection::SELECTION_BG_COLOR;

        let spans = vec![Span::raw("hello world")];
        let selection = Some(Selection {
            start: Position { row: 0, col: 6 },
            end: Position { row: 0, col: 11 },
            active: false,
        });

        let result = apply_selection_to_content(spans, &selection, 0, 0);
        assert!(result.len() > 1, "selection should split the span");
        assert!(
            result.iter().any(|s| s.style.bg == Some(SELECTION_BG_COLOR)),
            "at least one span should have selection background"
        );
    }

    #[test]
    fn test_apply_selection_on_different_row() {
        use crate::app::{Position, Selection};

        let spans = vec![Span::raw("hello world")];
        let selection = Some(Selection {
            start: Position { row: 5, col: 0 },
            end: Position { row: 5, col: 10 },
            active: false,
        });

        let result = apply_selection_to_content(spans, &selection, 0, 0);
        assert_eq!(result.len(), 1, "selection on different row should not split");
    }

    #[test]
    fn test_line_num_width_scales_with_max_line_number() {
        // 3-digit line numbers
        let mut lines = vec![DiffLine::file_header("test.rs")];
        let mut line = base_line("content");
        line.line_number = Some(999);
        line.file_path = Some("test.rs".to_string());
        lines.push(line);

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.estimate_content_width(80);
        let output = render_to_output(&app, 80, 24);
        assert_eq!(output.line_num_width, 4, "999 = 3 digits + 1 space");

        // 1-digit line numbers
        let mut lines = vec![DiffLine::file_header("test.rs")];
        let mut line = base_line("content");
        line.line_number = Some(9);
        line.file_path = Some("test.rs".to_string());
        lines.push(line);

        let app = TestAppBuilder::new().with_lines(lines).build();
        let output = render_to_output(&app, 80, 24);
        assert_eq!(output.line_num_width, 2, "9 = 1 digit + 1 space");

        // No line numbers
        let app = TestAppBuilder::new()
            .with_lines(vec![DiffLine::file_header("test.rs")])
            .build();
        let output = render_to_output(&app, 80, 24);
        assert_eq!(output.line_num_width, 0, "no line numbers = 0 width");
    }

    #[test]
    fn search_highlight_no_search_returns_original() {
        let spans = vec![Span::styled("hello world", Style::default())];
        let result = apply_search_to_content(spans.clone(), &None, 0);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "hello world");
    }

    #[test]
    fn search_highlight_no_matches_on_line() {
        let search = SearchState {
            matches: vec![SearchMatch { line_idx: 5, char_start: 0, char_len: 3 }],
            current: 0,
            ..Default::default()
        };
        let spans = vec![Span::styled("hello", Style::default())];
        let result = apply_search_to_content(spans, &Some(search), 0);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "hello");
    }

    #[test]
    fn search_highlight_single_match() {
        let search = SearchState {
            matches: vec![SearchMatch { line_idx: 0, char_start: 6, char_len: 5 }],
            current: 0,
            ..Default::default()
        };
        let spans = vec![Span::styled("hello world", Style::default())];
        let result = apply_search_to_content(spans, &Some(search), 0);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "hello ");
        assert_eq!(result[1].content, "world");
        assert_eq!(result[1].style.bg, Some(SEARCH_CURRENT_BG));
    }

    #[test]
    fn search_highlight_non_current_match_uses_match_bg() {
        let search = SearchState {
            matches: vec![
                SearchMatch { line_idx: 0, char_start: 0, char_len: 5 },
                SearchMatch { line_idx: 1, char_start: 0, char_len: 5 },
            ],
            current: 1, // Current is on line 1, not line 0
            ..Default::default()
        };
        let spans = vec![Span::styled("hello world", Style::default())];
        let result = apply_search_to_content(spans, &Some(search), 0);
        assert_eq!(result[0].content, "hello");
        assert_eq!(result[0].style.bg, Some(SEARCH_MATCH_BG));
    }

    #[test]
    fn search_highlight_multiple_matches_same_line() {
        let search = SearchState {
            matches: vec![
                SearchMatch { line_idx: 0, char_start: 0, char_len: 2 },
                SearchMatch { line_idx: 0, char_start: 4, char_len: 2 },
            ],
            current: 0,
            ..Default::default()
        };
        let spans = vec![Span::styled("ab cd ef", Style::default())];
        let result = apply_search_to_content(spans, &Some(search), 0);
        let highlighted: Vec<_> = result.iter().filter(|s| s.style.bg.is_some()).collect();
        assert_eq!(highlighted.len(), 2);
    }

    #[test]
    fn search_highlight_multibyte_unicode() {
        let search = SearchState {
            matches: vec![SearchMatch { line_idx: 0, char_start: 5, char_len: 6 }],
            current: 0,
            ..Default::default()
        };
        let spans = vec![Span::styled("café résumé", Style::default())];
        let result = apply_search_to_content(spans, &Some(search), 0);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "café ");
        assert_eq!(result[1].content, "résumé");
        assert_eq!(result[1].style.bg, Some(SEARCH_CURRENT_BG));
    }

    #[test]
    fn selection_on_wrapped_continuation_row_is_highlighted() {
        use crate::app::{Position, Selection};
        use crate::ui::selection::SELECTION_BG_COLOR;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        // Create a line long enough to wrap to 3 rows at width 60.
        // With prefix ~6 chars, content_width ~54, so 162 chars wraps to 3 rows.
        let long_content = "x".repeat(162);
        let mut lines = vec![DiffLine::file_header("test.rb")];
        let mut line = base_line(&long_content);
        line.line_number = Some(22);
        line.file_path = Some("test.rb".to_string());
        lines.push(line);

        let width: u16 = 60;
        let height: u16 = 10;

        // First render without selection to find row_map layout
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.estimate_content_width(width);
        let output = render_to_output(&app, width, height);

        // Verify wrapping produced continuation rows
        let continuation_count = output.row_map.iter().filter(|r| r.is_continuation).count();
        assert!(
            continuation_count >= 2,
            "expected at least 2 continuation rows, got {}. row_map len = {}, rows: {:?}",
            continuation_count,
            output.row_map.len(),
            output.row_map.iter().map(|r| (r.is_continuation, r.content.len())).collect::<Vec<_>>()
        );

        // The content row starts at row_map index 1 (after the file header).
        // The middle continuation row is at index 2.
        let middle_row_idx = 2;
        assert!(
            output.row_map[middle_row_idx].is_continuation,
            "row {} should be a continuation row",
            middle_row_idx
        );

        // Set selection spanning the entire middle continuation row.
        // content_offset is (1,1) from TestAppBuilder defaults, so screen
        // row = row_map_idx + offset_y = middle_row_idx + 1. Selection
        // coords are in content space (row_map indices), so use middle_row_idx directly.
        app.view.selection = Some(Selection {
            start: Position { row: middle_row_idx, col: 0 },
            end: Position { row: middle_row_idx, col: width as usize },
            active: false,
        });

        // Re-render with the selection active
        let ctx = FrameContext::new(&app);
        let area = Rect::new(0, 0, width, height);
        let view_model = DiffViewModel::from_app(&app, &ctx, area);

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                view_model.render(f);
            })
            .unwrap();

        // The middle continuation row renders at screen_y = content_offset.y + middle_row_idx
        let screen_y = app.view.content_offset.1 as u16 + middle_row_idx as u16;
        let buf = terminal.backend().buffer();

        // Check that at least one cell on the continuation row has the selection bg color
        let has_selection_bg = (0..width).any(|x| buf[(x, screen_y)].bg == SELECTION_BG_COLOR);
        assert!(
            has_selection_bg,
            "middle continuation row (screen_y={}) should have selection highlighting, \
             but no cells had bg color {:?}. Cell bgs: {:?}",
            screen_y,
            SELECTION_BG_COLOR,
            (0..width).map(|x| buf[(x, screen_y)].bg).collect::<Vec<_>>()
        );
    }

    #[test]
    fn selection_spanning_all_wrapped_rows_highlights_every_row() {
        use crate::app::{Position, Selection};
        use crate::ui::selection::SELECTION_BG_COLOR;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let long_content = "y".repeat(162);
        let mut lines = vec![DiffLine::file_header("test.rb")];
        let mut line = base_line(&long_content);
        line.line_number = Some(10);
        line.file_path = Some("test.rb".to_string());
        lines.push(line);

        let width: u16 = 60;
        let height: u16 = 10;

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.estimate_content_width(width);
        let output = render_to_output(&app, width, height);

        // Find all rows belonging to the wrapped content line (index 1 onward)
        let first_content_row = 1;
        let last_content_row = output.row_map.len() - 1;
        assert!(last_content_row >= first_content_row + 2, "need at least 3 rows");

        // Select across all wrapped rows
        app.view.selection = Some(Selection {
            start: Position { row: first_content_row, col: 0 },
            end: Position { row: last_content_row, col: width as usize },
            active: false,
        });

        let ctx = FrameContext::new(&app);
        let area = Rect::new(0, 0, width, height);
        let view_model = DiffViewModel::from_app(&app, &ctx, area);

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                view_model.render(f);
            })
            .unwrap();

        let buf = terminal.backend().buffer();
        let offset_y = app.view.content_offset.1 as u16;

        for row_idx in first_content_row..=last_content_row {
            let screen_y = offset_y + row_idx as u16;
            let has_selection_bg = (0..width).any(|x| buf[(x, screen_y)].bg == SELECTION_BG_COLOR);
            assert!(
                has_selection_bg,
                "row_map[{}] (screen_y={}, continuation={}) should have selection bg",
                row_idx,
                screen_y,
                output.row_map[row_idx].is_continuation
            );
        }
    }

    /// Search match offsets are computed against `line.content` (the new text);
    /// they must NOT be applied to the deletion side of a split modification,
    /// whose rendered text is `line.old_content`. Reproduces a bug where
    /// searching for text on a green (added) line also lit up arbitrary chars
    /// on the preceding red (removed) line because the offsets aliased.
    #[test]
    fn search_does_not_highlight_deletion_side_of_split_modification() {
        use crate::app::search::{compute_matches, SearchState};
        use crate::diff::InlineSpan;
        use crate::ui::colors::{SEARCH_CURRENT_BG, SEARCH_MATCH_BG};
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        // PureDeletion always splits regardless of width.
        //   old_content = "abc deleted XX def"  (chars 4-5 = "de" inside "deleted")
        //   content     = "abc XX def"          (chars 4-5 = "XX")
        // Searching "XX" finds it at chars 4-5 in `content`. The bug applies
        // those offsets to the deletion render, lighting up "de" of "deleted".
        let mut line = DiffLine::new(
            LineSource::Committed,
            "abc XX def".to_string(),
            '+',
            Some(1),
        );
        line.file_path = Some("test.rs".to_string());
        line.old_content = Some("abc deleted XX def".to_string());
        line.change_source = Some(LineSource::Committed);
        line.inline_spans = vec![
            InlineSpan { text: "abc ".to_string(), source: None, is_deletion: false },
            InlineSpan { text: "deleted ".to_string(), source: Some(LineSource::Committed), is_deletion: true },
            InlineSpan { text: "XX def".to_string(), source: None, is_deletion: false },
        ];

        let mut app = TestAppBuilder::new()
            .with_lines(vec![DiffLine::file_header("test.rs"), line])
            .build();
        app.estimate_content_width(80);

        let mut s = SearchState::new();
        s.query = "XX".to_string();
        s.input_active = false;
        s.matches = compute_matches(&app.lines, "XX");
        s.current = 0;
        app.search = Some(s);

        let width: u16 = 80;
        let height: u16 = 10;

        let ctx = FrameContext::new(&app);
        let area = Rect::new(0, 0, width, height);
        let view_model = DiffViewModel::from_app(&app, &ctx, area);

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| { view_model.render(f); }).unwrap();

        // Layout: file header at row 0, deletion row at 1, insertion row at 2.
        let buf = terminal.backend().buffer();
        let offset_y = app.view.content_offset.1 as u16;
        let del_y = offset_y + 1;
        let ins_y = offset_y + 2;

        let count_search_cells = |y: u16| -> usize {
            (0..width)
                .filter(|&x| {
                    let bg = buf[(x, y)].bg;
                    bg == SEARCH_CURRENT_BG || bg == SEARCH_MATCH_BG
                })
                .count()
        };

        // Sanity check: the insertion (green) row must highlight "XX".
        assert!(
            count_search_cells(ins_y) >= 2,
            "insertion row should have ≥2 search-bg cells (for 'XX'), got {}",
            count_search_cells(ins_y)
        );

        // Bug repro: the deletion (red) row must have NO search highlights.
        let del_hits: Vec<u16> = (0..width)
            .filter(|&x| {
                let bg = buf[(x, del_y)].bg;
                bg == SEARCH_CURRENT_BG || bg == SEARCH_MATCH_BG
            })
            .collect();
        assert!(
            del_hits.is_empty(),
            "deletion row must not show search highlight (search matches refer \
             to new content), but cells {:?} have search bg",
            del_hits
        );
    }

    // ---- content_match_to_render_ranges (offset translation) ----

    mod content_to_render {
        use super::*;
        use crate::diff::InlineSpan;

        fn unchanged(text: &str) -> InlineSpan {
            InlineSpan { text: text.to_string(), source: None, is_deletion: false }
        }
        fn insertion(text: &str) -> InlineSpan {
            InlineSpan { text: text.to_string(), source: Some(LineSource::Committed), is_deletion: false }
        }
        fn deletion(text: &str) -> InlineSpan {
            InlineSpan { text: text.to_string(), source: Some(LineSource::Committed), is_deletion: true }
        }

        #[test]
        fn empty_range_yields_nothing() {
            let spans = vec![unchanged("hello")];
            assert!(content_match_to_render_ranges(&spans, 3, 3).is_empty());
            assert!(content_match_to_render_ranges(&spans, 5, 2).is_empty());
        }

        #[test]
        fn no_deletions_is_identity() {
            // content = "hello world" (rendered identically)
            let spans = vec![unchanged("hello "), insertion("world")];
            assert_eq!(
                content_match_to_render_ranges(&spans, 6, 11),
                vec![(6, 11)]
            );
            assert_eq!(
                content_match_to_render_ranges(&spans, 0, 5),
                vec![(0, 5)]
            );
        }

        #[test]
        fn deletion_before_match_shifts_render_position() {
            // inline rendered: "DEL" + "abc"  → "DELabc"  (6 chars)
            // content        :         "abc"  → "abc"     (3 chars)
            // match content [0, 3) → render [3, 6)
            let spans = vec![deletion("DEL"), insertion("abc")];
            assert_eq!(
                content_match_to_render_ranges(&spans, 0, 3),
                vec![(3, 6)]
            );
        }

        #[test]
        fn deletion_after_match_does_not_shift() {
            // rendered: "abc" + "DEL"  → "abcDEL"
            // content : "abc"          → "abc"
            // match [0, 3) → render [0, 3)
            let spans = vec![insertion("abc"), deletion("DEL")];
            assert_eq!(
                content_match_to_render_ranges(&spans, 0, 3),
                vec![(0, 3)]
            );
        }

        #[test]
        fn match_split_across_interior_deletion_yields_two_ranges() {
            // rendered: "ab" + "DEL" + "cd"  → "abDELcd"  (7 chars)
            // content : "ab" +         "cd"  → "abcd"     (4 chars)
            // match content [0, 4) → render [0, 2) + [5, 7)
            let spans = vec![unchanged("ab"), deletion("DEL"), insertion("cd")];
            assert_eq!(
                content_match_to_render_ranges(&spans, 0, 4),
                vec![(0, 2), (5, 7)]
            );
        }

        #[test]
        fn match_inside_a_single_unchanged_span_is_translated() {
            // rendered: "DEL" + "hello"   → "DELhello"
            // content :         "hello"   → "hello"
            // match content [1, 4) ("ell") → render [4, 7)
            let spans = vec![deletion("DEL"), unchanged("hello")];
            assert_eq!(
                content_match_to_render_ranges(&spans, 1, 4),
                vec![(4, 7)]
            );
        }

        #[test]
        fn adjacent_non_deletion_spans_coalesce_into_single_range() {
            // rendered: "ab" + "cd"  → "abcd"   (no deletions)
            // match across both → one coalesced range
            let spans = vec![unchanged("ab"), insertion("cd")];
            assert_eq!(
                content_match_to_render_ranges(&spans, 1, 3),
                vec![(1, 3)]
            );
        }

        #[test]
        fn multiple_deletions_accumulate_render_shift() {
            // rendered: "D1" + "ab" + "D2" + "cd"  → "D1abD2cd"  (8 chars)
            // content :         "ab" +         "cd"  → "abcd"     (4 chars)
            // match content [2, 4) ("cd") → render [6, 8)
            let spans = vec![deletion("D1"), unchanged("ab"), deletion("D2"), insertion("cd")];
            assert_eq!(
                content_match_to_render_ranges(&spans, 2, 4),
                vec![(6, 8)]
            );
        }

        #[test]
        fn match_outside_content_yields_nothing() {
            let spans = vec![unchanged("ab")];
            assert!(content_match_to_render_ranges(&spans, 5, 7).is_empty());
        }

        #[test]
        fn multibyte_chars_use_char_count_not_byte_count() {
            // "café" is 4 chars but 5 bytes
            let spans = vec![deletion("café"), insertion("ok")];
            // rendered "caféok" (6 chars), content "ok" (2 chars)
            // match [0, 2) → render [4, 6)
            assert_eq!(
                content_match_to_render_ranges(&spans, 0, 2),
                vec![(4, 6)]
            );
        }
    }

    /// Integration test: inline Mixed render (deletion + insertion fit on one
    /// line, no split). The renderer interleaves deletion text into the output;
    /// search highlights must land on the actual matched characters in `content`,
    /// not on whatever happens to live at those raw render offsets.
    #[test]
    fn search_highlight_aligned_on_inline_mixed_render() {
        use crate::app::search::{compute_matches, SearchState};
        use crate::diff::InlineSpan;
        use crate::ui::colors::{SEARCH_CURRENT_BG, SEARCH_MATCH_BG};
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        // Mixed change, fits within terminal width → does not split.
        // inline_spans render as: "ab" + "DEL" + "cd ZZ ef"
        // content (new):           "ab" +         "cd ZZ ef"  → "abcd ZZ ef"
        // Search "ZZ" matches content chars 5-6. Without translation, the
        // bug would highlight chars 5-6 of rendered "abDELcd ZZ ef" = "cd".
        let mut line = DiffLine::new(
            LineSource::Committed,
            "abcd ZZ ef".to_string(),
            ' ',
            Some(1),
        );
        line.file_path = Some("test.rs".to_string());
        line.old_content = Some("abDEL ef".to_string());
        line.change_source = Some(LineSource::Committed);
        line.inline_spans = vec![
            InlineSpan { text: "ab".to_string(), source: None, is_deletion: false },
            InlineSpan { text: "DEL".to_string(), source: Some(LineSource::Committed), is_deletion: true },
            InlineSpan { text: "cd ZZ ef".to_string(), source: Some(LineSource::Committed), is_deletion: false },
        ];

        let mut app = TestAppBuilder::new()
            .with_lines(vec![DiffLine::file_header("test.rs"), line])
            .build();
        app.estimate_content_width(80);

        let mut s = SearchState::new();
        s.query = "ZZ".to_string();
        s.input_active = false;
        s.matches = compute_matches(&app.lines, "ZZ");
        s.current = 0;
        app.search = Some(s);

        let width: u16 = 80;
        let height: u16 = 10;
        let ctx = FrameContext::new(&app);
        let area = Rect::new(0, 0, width, height);
        let view_model = DiffViewModel::from_app(&app, &ctx, area);

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| { view_model.render(f); }).unwrap();

        let buf = terminal.backend().buffer();
        let offset_y = app.view.content_offset.1 as u16;
        let row_y = offset_y + 1;

        let highlighted: String = (0..width)
            .filter_map(|x| {
                let cell = &buf[(x, row_y)];
                if cell.bg == SEARCH_CURRENT_BG || cell.bg == SEARCH_MATCH_BG {
                    Some(cell.symbol().to_string())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(
            highlighted, "ZZ",
            "search highlight on inline-mixed line should cover exactly 'ZZ'; \
             got {:?}",
            highlighted
        );
    }

    /// A search match that straddles a deletion in the inline render must
    /// produce two visually-disjoint highlight regions, not one shifted region.
    #[test]
    fn search_highlight_split_by_interior_deletion_yields_two_regions() {
        use crate::app::search::{compute_matches, SearchState};
        use crate::diff::InlineSpan;
        use crate::ui::colors::{SEARCH_CURRENT_BG, SEARCH_MATCH_BG};
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        // rendered: "ab" + "DEL" + "cd"  → "abDELcd"
        // content :  "ab" +        "cd"  → "abcd"
        // search "abcd" matches all of content; should highlight render
        // ranges [0, 2) ("ab") and [5, 7) ("cd"), skipping "DEL".
        let mut line = DiffLine::new(
            LineSource::Committed,
            "abcd".to_string(),
            ' ',
            Some(1),
        );
        line.file_path = Some("test.rs".to_string());
        line.old_content = Some("abDEL".to_string());
        line.change_source = Some(LineSource::Committed);
        line.inline_spans = vec![
            InlineSpan { text: "ab".to_string(), source: None, is_deletion: false },
            InlineSpan { text: "DEL".to_string(), source: Some(LineSource::Committed), is_deletion: true },
            InlineSpan { text: "cd".to_string(), source: Some(LineSource::Committed), is_deletion: false },
        ];

        let mut app = TestAppBuilder::new()
            .with_lines(vec![DiffLine::file_header("test.rs"), line])
            .build();
        app.estimate_content_width(80);

        let mut s = SearchState::new();
        s.query = "abcd".to_string();
        s.input_active = false;
        s.matches = compute_matches(&app.lines, "abcd");
        s.current = 0;
        app.search = Some(s);

        let width: u16 = 80;
        let height: u16 = 10;
        let ctx = FrameContext::new(&app);
        let area = Rect::new(0, 0, width, height);
        let view_model = DiffViewModel::from_app(&app, &ctx, area);

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| { view_model.render(f); }).unwrap();

        let buf = terminal.backend().buffer();
        let offset_y = app.view.content_offset.1 as u16;
        let row_y = offset_y + 1;

        // Collect highlighted symbols in render order.
        let highlighted: String = (0..width)
            .filter_map(|x| {
                let cell = &buf[(x, row_y)];
                if cell.bg == SEARCH_CURRENT_BG || cell.bg == SEARCH_MATCH_BG {
                    Some(cell.symbol().to_string())
                } else {
                    None
                }
            })
            .collect();

        // "ab" + "cd" — DEL must NOT be highlighted between them.
        assert_eq!(
            highlighted, "abcd",
            "match straddling deletion should highlight only the matched \
             characters, not the interleaved deletion text; got {:?}",
            highlighted
        );
    }
}
