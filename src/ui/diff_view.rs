//! Diff view rendering with pure data model separation.
//!
//! The DiffViewModel provides a pure view model for rendering, enabling
//! easier unit testing without requiring a full App instance.

use std::collections::HashSet;

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::{App, DisplayableItem, FrameContext, Selection};
use crate::diff::{DiffLine, LineSource};

use super::colors::{line_style, status_symbol};
use super::selection::{apply_selection_to_span, get_line_selection_range};
use super::spans::{
    build_deletion_spans_with_highlight, build_insertion_spans_with_highlight, classify_inline_change,
    coalesce_spans, get_deletion_source, get_insertion_source, inline_display_width, InlineChangeType,
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
        let (start, end) = ctx.visible_range();
        let items = &ctx.items()[start..end];

        Self {
            items,
            lines: &app.lines,
            selection: &app.selection,
            collapsed_files: &app.collapsed_files,
            area,
            show_copied_flash: app.should_show_copied_flash(),
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
                    let del_spans =
                        build_deletion_spans_with_highlight(&diff_line.inline_spans, del_source);

                    if !del_spans.is_empty() {
                        let del_style = line_style(del_source);
                        let del_prefix_str = if !prefix_str.is_empty() {
                            " ".repeat(prefix_str.len())
                        } else {
                            String::new()
                        };

                        let old_content: String =
                            del_spans.iter().map(|s| s.content.as_ref()).collect();
                        let del_spans = apply_selection_to_content(
                            del_spans,
                            self.selection,
                            screen_row_idx,
                            prefix_width,
                        );

                        let del_prefix_char = format!("- {} ", status_symbol(del_source));
                        let (del_lines, del_row_infos) = wrap_content(
                            del_spans,
                            &old_content,
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
                    let ins_spans =
                        build_insertion_spans_with_highlight(&diff_line.inline_spans, ins_source);
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
                    let display_spans = coalesce_spans(&diff_line.inline_spans);
                    let content_spans: Vec<Span> = display_spans
                        .into_iter()
                        .map(|inline_span| {
                            let span_style = match inline_span.source {
                                Some(source) => line_style(source),
                                None => style,
                            };
                            Span::styled(inline_span.text, span_style)
                        })
                        .collect();

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

        // Non-wrapped inline spans
        let display_spans = coalesce_spans(&diff_line.inline_spans);
        let content_spans: Vec<Span> = display_spans
            .into_iter()
            .map(|inline_span| {
                let span_style = match inline_span.source {
                    Some(source) => line_style(source),
                    None => style,
                };
                Span::styled(inline_span.text, span_style)
            })
            .collect();

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
        let content_spans = vec![Span::styled(diff_line.content.clone(), style)];
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
}
