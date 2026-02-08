//! Diff view rendering with pure data model separation.
//!
//! The DiffViewModel provides a pure view model for rendering, enabling
//! easier unit testing without requiring a full App instance.

use std::collections::HashSet;

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::{App, DisplayableItem, FrameContext, Selection};
use crate::diff::{DiffLine, LineSource};
use crate::image_diff::ImageCache;

use super::colors::{line_style, status_symbol};
use super::selection::{apply_selection_to_span, get_line_selection_range};
use super::colors::line_style_with_highlight;
use super::spans::{
    build_deletion_spans_with_highlight, build_insertion_spans_with_highlight, classify_inline_change,
    get_deletion_source, get_insertion_source, inline_display_width, InlineChangeType,
    syntax_highlight_content, syntax_highlight_inline_spans,
};
use super::wrapping::wrap_content;
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
}

/// Output from rendering (data App needs to store).
pub struct RenderOutput {
    pub row_map: Vec<ScreenRowInfo>,
    pub content_offset: (u16, u16),
    pub line_num_width: usize,
    pub content_width: usize,
}

impl<'a> DiffViewModel<'a> {
    /// Create view model from App and FrameContext.
    pub fn from_app(app: &'a App, ctx: &'a FrameContext, area: Rect) -> Self {
        let (start, end) = ctx.visible_range(app);
        let items = &ctx.items()[start..end];

        Self {
            items,
            lines: &app.lines,
            selection: &app.selection,
            collapsed_files: &app.collapsed_files,
            area,
            show_copied_flash: app.should_show_copied_flash(),
            image_cache: &app.image_cache,
        }
    }

    /// Check if a file is collapsed.
    fn is_file_collapsed(&self, path: &str) -> bool {
        self.collapsed_files.contains(path)
    }

    /// Render the diff view and return output data.
    pub fn render(&self, frame: &mut Frame) -> RenderOutput {
        let max_line_num = self
            .items
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

        let mut all_lines: Vec<Line> = Vec::new();
        let mut all_row_infos: Vec<ScreenRowInfo> = Vec::new();
        let mut screen_row_idx = 0;

        for item in self.items {
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
                DisplayableItem::Line(idx) => {
                    let rows_added = self.render_diff_line(
                        &self.lines[*idx],
                        line_num_width,
                        prefix_width,
                        content_width,
                        screen_row_idx,
                        &mut all_lines,
                        &mut all_row_infos,
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

        RenderOutput {
            row_map: all_row_infos,
            content_offset: (content_offset_x, content_offset_y),
            line_num_width,
            content_width,
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

    /// Render a diff line and return the number of screen rows used.
    fn render_diff_line(
        &self,
        diff_line: &DiffLine,
        line_num_width: usize,
        prefix_width: usize,
        content_width: usize,
        screen_row_idx: usize,
        all_lines: &mut Vec<Line<'static>>,
        all_row_infos: &mut Vec<ScreenRowInfo>,
    ) -> usize {
        let style = line_style(diff_line.source);

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
                all_lines,
                all_row_infos,
            );
        }

        // Lines with inline spans
        if !diff_line.inline_spans.is_empty() {
            return self.render_inline_spans(
                diff_line,
                &prefix_str,
                style,
                prefix_width,
                content_width,
                screen_row_idx,
                all_lines,
                all_row_infos,
            );
        }

        // Plain content
        self.render_plain_content(
            diff_line,
            &prefix_str,
            style,
            prefix_width,
            content_width,
            screen_row_idx,
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

        // Check if we have loaded image data in the cache
        let image_info = diff_line
            .file_path
            .as_ref()
            .and_then(|path| self.image_cache.peek(path));

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
        prefix_str: &str,
        style: Style,
        prefix_width: usize,
        content_width: usize,
        mut screen_row_idx: usize,
        all_lines: &mut Vec<Line<'static>>,
        all_row_infos: &mut Vec<ScreenRowInfo>,
    ) -> usize {
        let inline_width = inline_display_width(&diff_line.inline_spans);
        let rows_before = all_lines.len();

        if inline_width > content_width {
            let change_type = classify_inline_change(&diff_line.inline_spans);

            match change_type {
                InlineChangeType::Mixed => {
                    let del_source = get_deletion_source(&diff_line.inline_spans);
                    let ins_source = get_insertion_source(&diff_line.inline_spans);

                    // Get old content for deletion line syntax highlighting
                    let old_content = diff_line.old_content.as_deref().unwrap_or("");
                    let del_spans = build_deletion_spans_with_highlight(
                        &diff_line.inline_spans,
                        del_source,
                        old_content,
                        diff_line.file_path.as_deref(),
                    );

                    if !del_spans.is_empty() {
                        let del_style = line_style(del_source);
                        let del_prefix_str = if !prefix_str.is_empty() {
                            " ".repeat(prefix_str.len())
                        } else {
                            String::new()
                        };

                        let del_spans = apply_selection_to_content(
                            del_spans,
                            self.selection,
                            screen_row_idx,
                            prefix_width,
                        );

                        let del_prefix_char = format!("- {} ", status_symbol(del_source));
                        let (del_lines, del_row_infos) = wrap_content(
                            del_spans,
                            old_content,
                            del_prefix_str,
                            del_prefix_char,
                            del_style,
                            content_width,
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
                    let ins_spans = apply_selection_to_content(
                        ins_spans,
                        self.selection,
                        screen_row_idx,
                        prefix_width,
                    );

                    let ins_prefix_char = format!("+ {} ", status_symbol(ins_source));
                    let (ins_lines, ins_row_infos) = wrap_content(
                        ins_spans,
                        new_content,
                        prefix_str.to_string(),
                        ins_prefix_char,
                        ins_style,
                        content_width,
                        prefix_width,
                    );

                    all_lines.extend(ins_lines);
                    all_row_infos.extend(ins_row_infos);

                    return all_lines.len() - rows_before;
                }
                InlineChangeType::PureDeletion | InlineChangeType::PureAddition => {
                    // Use the actual source from spans, not the line's base source
                    let highlight_source = get_insertion_source(&diff_line.inline_spans);
                    let highlight_style = line_style_with_highlight(highlight_source);
                    let content_spans = syntax_highlight_inline_spans(
                        &diff_line.inline_spans,
                        &diff_line.content,
                        diff_line.file_path.as_deref(),
                        style,
                        highlight_style,
                    );

                    let content_spans = apply_selection_to_content(
                        content_spans,
                        self.selection,
                        screen_row_idx,
                        prefix_width,
                    );

                    let prefix_char = format!("{} {} ", diff_line.prefix, status_symbol(diff_line.source));
                    let (lines, row_infos) = wrap_content(
                        content_spans,
                        &diff_line.content,
                        prefix_str.to_string(),
                        prefix_char,
                        style,
                        content_width,
                        prefix_width,
                    );

                    all_lines.extend(lines);
                    all_row_infos.extend(row_infos);

                    return all_lines.len() - rows_before;
                }
                InlineChangeType::NoChange => {}
            }
        }

        // Non-wrapped inline spans with syntax highlighting
        // Use the actual insertion source from spans, not the line's base source
        let highlight_source = get_insertion_source(&diff_line.inline_spans);
        let highlight_style = line_style_with_highlight(highlight_source);
        let content_spans = syntax_highlight_inline_spans(
            &diff_line.inline_spans,
            &diff_line.content,
            diff_line.file_path.as_deref(),
            style,
            highlight_style,
        );

        let content_spans =
            apply_selection_to_content(content_spans, self.selection, screen_row_idx, prefix_width);

        let prefix_char = format!("{} {} ", diff_line.prefix, status_symbol(diff_line.source));
        let (lines, row_infos) = wrap_content(
            content_spans,
            &diff_line.content,
            prefix_str.to_string(),
            prefix_char,
            style,
            content_width,
            prefix_width,
        );

        all_lines.extend(lines);
        all_row_infos.extend(row_infos);

        all_lines.len() - rows_before
    }

    fn render_plain_content(
        &self,
        diff_line: &DiffLine,
        prefix_str: &str,
        style: Style,
        prefix_width: usize,
        content_width: usize,
        screen_row_idx: usize,
        all_lines: &mut Vec<Line<'static>>,
        all_row_infos: &mut Vec<ScreenRowInfo>,
    ) -> usize {
        let prefix_char = format!("{} {} ", diff_line.prefix, status_symbol(diff_line.source));

        // Apply syntax highlighting - foreground from syntax, background from diff style
        let content_spans = syntax_highlight_content(
            &diff_line.content,
            diff_line.file_path.as_deref(),
            style,
        );

        let content_spans =
            apply_selection_to_content(content_spans, self.selection, screen_row_idx, prefix_width);

        let (lines, row_infos) = wrap_content(
            content_spans,
            &diff_line.content,
            prefix_str.to_string(),
            prefix_char,
            style,
            content_width,
            prefix_width,
        );

        let rows_added = lines.len();
        all_lines.extend(lines);
        all_row_infos.extend(row_infos);

        rows_added
    }
}

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
        let mut char_offset = 0;

        for span in content_spans {
            let span_with_selection =
                apply_selection_to_span(span.clone(), char_offset, content_sel_start, content_sel_end);
            char_offset += span.content.len();
            result.extend(span_with_selection);
        }
        result
    } else {
        content_spans
    }
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
    );
    app.set_row_map(output.row_map);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{base_line, change_line, TestAppBuilder};

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
        app.scroll_offset = 2;

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
        app.collapsed_files.insert("test.rs".to_string());

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
        app.selection = Some(Selection {
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
    fn test_render_output_fields() {
        let output = RenderOutput {
            row_map: vec![
                ScreenRowInfo {
                    content: "test".to_string(),
                    is_file_header: false,
                    file_path: None,
                    is_continuation: false,
                },
            ],
            content_offset: (1, 2),
            line_num_width: 4,
            content_width: 76,
        };

        assert_eq!(output.row_map.len(), 1);
        assert_eq!(output.content_offset, (1, 2));
        assert_eq!(output.line_num_width, 4);
        assert_eq!(output.content_width, 76);
    }

    #[test]
    fn test_apply_selection_to_content_no_selection() {
        let spans = vec![Span::raw("test content")];
        let result = apply_selection_to_content(spans.clone(), &None, 0, 0);
        assert_eq!(result.len(), 1);
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
        app.path_copied_at = Some(std::time::Instant::now());
        let ctx = FrameContext::new(&app);
        let view_model = DiffViewModel::from_app(&app, &ctx, area);
        assert!(view_model.show_copied_flash);

        // After 800ms+ elapsed, flash should be inactive
        app.path_copied_at = Some(std::time::Instant::now() - std::time::Duration::from_millis(900));
        let ctx = FrameContext::new(&app);
        let view_model = DiffViewModel::from_app(&app, &ctx, area);
        assert!(!view_model.show_copied_flash);
    }

    // Tests for inline span highlight source computation
    mod highlight_source_tests {
        use super::*;
        use crate::diff::InlineSpan;
        use crate::ui::colors::{highlight_bg_color, line_style_with_highlight};

        /// Verifies that get_insertion_source extracts source from changed spans,
        /// not from the line's base source.
        #[test]
        fn test_get_insertion_source_extracts_from_spans() {
            // Simulate spans for "    widgets::{Block, Borders, Clear, Paragraph},"
            // where "Clear, " was inserted (source=Unstaged)
            let spans = vec![
                InlineSpan { text: "prefix ".to_string(), source: None, is_deletion: false },
                InlineSpan { text: "inserted".to_string(), source: Some(LineSource::Unstaged), is_deletion: false },
                InlineSpan { text: " suffix".to_string(), source: None, is_deletion: false },
            ];

            let source = get_insertion_source(&spans);
            assert_eq!(source, LineSource::Unstaged);
        }

        #[test]
        fn test_get_insertion_source_with_committed_change() {
            let spans = vec![
                InlineSpan { text: "unchanged".to_string(), source: None, is_deletion: false },
                InlineSpan { text: "committed_text".to_string(), source: Some(LineSource::Committed), is_deletion: false },
            ];

            let source = get_insertion_source(&spans);
            assert_eq!(source, LineSource::Committed);
        }

        #[test]
        fn test_get_insertion_source_with_staged_change() {
            let spans = vec![
                InlineSpan { text: "staged_insertion".to_string(), source: Some(LineSource::Staged), is_deletion: false },
            ];

            let source = get_insertion_source(&spans);
            assert_eq!(source, LineSource::Staged);
        }

        #[test]
        fn test_get_insertion_source_ignores_deletions() {
            // Deletions should not be considered as insertion source
            let spans = vec![
                InlineSpan { text: "deleted".to_string(), source: Some(LineSource::DeletedBase), is_deletion: true },
                InlineSpan { text: "inserted".to_string(), source: Some(LineSource::Unstaged), is_deletion: false },
            ];

            let source = get_insertion_source(&spans);
            assert_eq!(source, LineSource::Unstaged);
        }

        /// Verifies that Base source has no highlight background (the bug symptom)
        #[test]
        fn test_base_source_has_no_highlight_background() {
            let bg = highlight_bg_color(LineSource::Base);
            assert_eq!(bg, Color::Reset, "Base source should have Reset (no) background");
        }

        /// Verifies that Unstaged source has a visible highlight background
        #[test]
        fn test_unstaged_source_has_visible_highlight() {
            let bg = highlight_bg_color(LineSource::Unstaged);
            // Unstaged highlight is yellow-ish: Rgb(130, 130, 35)
            assert!(matches!(bg, Color::Rgb(130, 130, 35)), "Unstaged should have yellow highlight");
        }

        /// Test that line_style_with_highlight produces different styles for Base vs Unstaged
        #[test]
        fn test_highlight_style_differs_by_source() {
            let base_style = line_style_with_highlight(LineSource::Base);
            let unstaged_style = line_style_with_highlight(LineSource::Unstaged);

            // The background colors should be different
            assert_ne!(base_style.bg, unstaged_style.bg,
                "Base and Unstaged highlight styles should have different backgrounds");

            // Base should have Reset background
            assert_eq!(base_style.bg, Some(Color::Reset));

            // Unstaged should have a visible color
            assert!(matches!(unstaged_style.bg, Some(Color::Rgb(130, 130, 35))));
        }

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

        /// Test that syntax_highlight_inline_spans applies highlight_style to changed portions
        #[test]
        fn test_syntax_highlight_inline_spans_applies_highlight_to_changes() {
            // Use distinct words to avoid substring matching issues
            let inline_spans = vec![
                InlineSpan { text: "prefix ".to_string(), source: None, is_deletion: false },
                InlineSpan { text: "INSERTED".to_string(), source: Some(LineSource::Unstaged), is_deletion: false },
                InlineSpan { text: " suffix".to_string(), source: None, is_deletion: false },
            ];
            let content = "prefix INSERTED suffix";
            let base_style = Style::default().bg(Color::Reset);
            let highlight_style = line_style_with_highlight(LineSource::Unstaged);

            let result = syntax_highlight_inline_spans(
                &inline_spans,
                content,
                None, // no syntax highlighting
                base_style,
                highlight_style,
            );

            // Should have spans
            assert!(!result.is_empty());

            // Find a span that contains the inserted text
            let inserted_span = result.iter().find(|s| s.content.contains("INSERTED"));
            assert!(inserted_span.is_some(), "Should have a span containing 'INSERTED'");
            let inserted_span = inserted_span.unwrap();

            // The inserted portion should have the Unstaged highlight background
            assert_eq!(inserted_span.style.bg, Some(Color::Rgb(130, 130, 35)),
                "Inserted portion should have Unstaged highlight background");
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
        app.scroll_offset = 10;
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
            app.scroll_offset = scroll;
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
}
