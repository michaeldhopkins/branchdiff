use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::{App, DisplayableItem, FrameContext};
use crate::diff::LineSource;

use super::colors::line_style;
use super::selection::{get_line_selection_range, apply_selection_to_span};
use super::spans::{coalesce_spans, inline_display_width, get_deletion_source, get_insertion_source, build_deletion_spans_with_highlight, build_insertion_spans_with_highlight, classify_inline_change, InlineChangeType};
use super::wrapping::wrap_content;
use super::{ScreenRowInfo, PREFIX_CHAR_WIDTH};

fn apply_selection_to_content(
    content_spans: Vec<Span<'static>>,
    selection: &Option<crate::app::Selection>,
    screen_row_idx: usize,
    prefix_width: usize,
) -> Vec<Span<'static>> {
    if let Some((sel_start, sel_end)) = get_line_selection_range(selection, screen_row_idx) {
        let content_sel_start = sel_start.saturating_sub(prefix_width);
        let content_sel_end = sel_end.saturating_sub(prefix_width);

        let mut result = Vec::new();
        let mut char_offset = 0;

        for span in content_spans {
            let span_with_selection = apply_selection_to_span(
                span.clone(),
                char_offset,
                content_sel_start,
                content_sel_end,
            );
            char_offset += span.content.len();
            result.extend(span_with_selection);
        }
        result
    } else {
        content_spans
    }
}

pub fn draw_diff_view(frame: &mut Frame, app: &mut App, area: Rect) {
    let visible_lines = app.visible_lines();

    let max_line_num = visible_lines
        .iter()
        .filter_map(|l| l.line_number)
        .max()
        .unwrap_or(0);
    let line_num_width = if max_line_num > 0 {
        max_line_num.to_string().len() + 1
    } else {
        0
    };

    let available_width = area.width.saturating_sub(2) as usize;
    let prefix_width = if line_num_width > 0 { line_num_width + 1 } else { 0 } + PREFIX_CHAR_WIDTH;
    let content_width = available_width.saturating_sub(prefix_width);

    let content_offset_x = area.x + 1;
    let content_offset_y = area.y + 1;
    app.set_content_layout(content_offset_x, content_offset_y, line_num_width, content_width);

    let selection = app.selection.clone();

    let mut all_lines: Vec<Line> = Vec::new();
    let mut all_row_infos: Vec<ScreenRowInfo> = Vec::new();
    let mut screen_row_idx = 0;

    for diff_line in visible_lines.iter() {
        let style = line_style(diff_line.source);

        let prefix_str = if let Some(num) = diff_line.line_number {
            format!("{:>width$} ", num, width = line_num_width)
        } else if line_num_width > 0 {
            " ".repeat(line_num_width + 1)
        } else {
            String::new()
        };

        if diff_line.source == LineSource::FileHeader {
            let mut spans = Vec::new();
            if !prefix_str.is_empty() {
                spans.push(Span::styled(prefix_str, Style::default().fg(Color::DarkGray)));
            }
            let is_collapsed = diff_line.file_path.as_ref()
                .map(|p| app.is_file_collapsed(p))
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
            });
            screen_row_idx += 1;
            continue;
        } else if diff_line.source == LineSource::Elided {
            let mut spans = Vec::new();
            if !prefix_str.is_empty() {
                spans.push(Span::styled(prefix_str, Style::default().fg(Color::DarkGray)));
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
            });
            screen_row_idx += 1;
            continue;
        }

        if !diff_line.inline_spans.is_empty() {
            let inline_width = inline_display_width(&diff_line.inline_spans);

            if inline_width > content_width {
                let change_type = classify_inline_change(&diff_line.inline_spans);

                match change_type {
                    InlineChangeType::Mixed => {
                        let del_source = get_deletion_source(&diff_line.inline_spans);
                        let ins_source = get_insertion_source(&diff_line.inline_spans);
                        let del_spans = build_deletion_spans_with_highlight(&diff_line.inline_spans, del_source);

                        if !del_spans.is_empty() {
                            let del_style = line_style(del_source);
                            let del_prefix_str = if line_num_width > 0 {
                                " ".repeat(line_num_width + 1)
                            } else {
                                String::new()
                            };

                            let old_content: String = del_spans.iter().map(|s| s.content.as_ref()).collect();
                            let del_spans = apply_selection_to_content(del_spans, &selection, screen_row_idx, prefix_width);

                            let (del_lines, del_row_infos) = wrap_content(
                                del_spans,
                                &old_content,
                                del_prefix_str,
                                "- ".to_string(),
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
                        let ins_spans = build_insertion_spans_with_highlight(&diff_line.inline_spans, ins_source);
                        let ins_spans = apply_selection_to_content(ins_spans, &selection, screen_row_idx, prefix_width);

                        let (ins_lines, ins_row_infos) = wrap_content(
                            ins_spans,
                            new_content,
                            prefix_str.clone(),
                            "+ ".to_string(),
                            ins_style,
                            content_width,
                            prefix_width,
                        );

                        screen_row_idx += ins_lines.len();
                        all_lines.extend(ins_lines);
                        all_row_infos.extend(ins_row_infos);
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

                        let content_spans = apply_selection_to_content(content_spans, &selection, screen_row_idx, prefix_width);

                        let (lines, row_infos) = wrap_content(
                            content_spans,
                            &diff_line.content,
                            prefix_str.clone(),
                            "  ".to_string(),
                            style,
                            content_width,
                            prefix_width,
                        );

                        screen_row_idx += lines.len();
                        all_lines.extend(lines);
                        all_row_infos.extend(row_infos);
                    }
                    InlineChangeType::NoChange => {}
                }

                if change_type != InlineChangeType::NoChange {
                    continue;
                }
            }

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

            let content_spans = apply_selection_to_content(content_spans, &selection, screen_row_idx, prefix_width);

            let prefix_char = format!("{} ", diff_line.prefix);
            let (lines, row_infos) = wrap_content(
                content_spans,
                &diff_line.content,
                prefix_str,
                prefix_char,
                style,
                content_width,
                prefix_width,
            );

            screen_row_idx += lines.len();
            all_lines.extend(lines);
            all_row_infos.extend(row_infos);
        } else {
            let prefix_char = format!("{} ", diff_line.prefix);
            let content_spans = vec![Span::styled(diff_line.content.clone(), style)];
            let content_spans = apply_selection_to_content(content_spans, &selection, screen_row_idx, prefix_width);

            let (lines, row_infos) = wrap_content(
                content_spans,
                &diff_line.content,
                prefix_str,
                prefix_char,
                style,
                content_width,
                prefix_width,
            );

            screen_row_idx += lines.len();
            all_lines.extend(lines);
            all_row_infos.extend(row_infos);
        }
    }

    app.set_row_map(all_row_infos);

    let title = match app.current_file() {
        Some(ref file) => Line::from(vec![
            Span::styled(format!(" {} ", file), Style::default().fg(Color::White)),
        ]),
        None => Line::from(vec![
            Span::styled(" branchdiff ", Style::default().fg(Color::DarkGray)),
        ]),
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(all_lines).block(block);

    frame.render_widget(paragraph, area);
}

/// Draw the diff view using a pre-computed frame context
pub fn draw_diff_view_with_frame(frame: &mut Frame, app: &mut App, area: Rect, ctx: &FrameContext) {
    let (start, end) = ctx.visible_range();
    let items = ctx.items();
    let visible_items = &items[start..end];

    // Find max line number from visible lines
    let max_line_num = visible_items
        .iter()
        .filter_map(|item| {
            if let DisplayableItem::Line(idx) = item {
                app.lines[*idx].line_number
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

    let available_width = area.width.saturating_sub(2) as usize;
    let prefix_width = if line_num_width > 0 { line_num_width + 1 } else { 0 } + PREFIX_CHAR_WIDTH;
    let content_width = available_width.saturating_sub(prefix_width);

    let content_offset_x = area.x + 1;
    let content_offset_y = area.y + 1;
    app.set_content_layout(content_offset_x, content_offset_y, line_num_width, content_width);

    let selection = app.selection.clone();

    let mut all_lines: Vec<Line> = Vec::new();
    let mut all_row_infos: Vec<ScreenRowInfo> = Vec::new();
    let mut screen_row_idx = 0;

    for item in visible_items {
        match item {
            DisplayableItem::Elided(count) => {
                // Render elided marker
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
                });
                screen_row_idx += 1;
            }
            DisplayableItem::Line(idx) => {
                let diff_line = &app.lines[*idx];
                let style = line_style(diff_line.source);

                let prefix_str = if let Some(num) = diff_line.line_number {
                    format!("{:>width$} ", num, width = line_num_width)
                } else if line_num_width > 0 {
                    " ".repeat(line_num_width + 1)
                } else {
                    String::new()
                };

                if diff_line.source == LineSource::FileHeader {
                    let mut spans = Vec::new();
                    if !prefix_str.is_empty() {
                        spans.push(Span::styled(prefix_str, Style::default().fg(Color::DarkGray)));
                    }
                    let is_collapsed = diff_line.file_path.as_ref()
                        .map(|p| app.is_file_collapsed(p))
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
                    });
                    screen_row_idx += 1;
                    continue;
                } else if diff_line.source == LineSource::Elided {
                    // Legacy path for DiffLine-based Elided (shouldn't happen with FrameContext)
                    let mut spans = Vec::new();
                    if !prefix_str.is_empty() {
                        spans.push(Span::styled(prefix_str, Style::default().fg(Color::DarkGray)));
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
                    });
                    screen_row_idx += 1;
                    continue;
                }

                if !diff_line.inline_spans.is_empty() {
                    let inline_width = inline_display_width(&diff_line.inline_spans);

                    if inline_width > content_width {
                        let change_type = classify_inline_change(&diff_line.inline_spans);

                        match change_type {
                            InlineChangeType::Mixed => {
                                let del_source = get_deletion_source(&diff_line.inline_spans);
                                let ins_source = get_insertion_source(&diff_line.inline_spans);
                                let del_spans = build_deletion_spans_with_highlight(&diff_line.inline_spans, del_source);

                                if !del_spans.is_empty() {
                                    let del_style = line_style(del_source);
                                    let del_prefix_str = if line_num_width > 0 {
                                        " ".repeat(line_num_width + 1)
                                    } else {
                                        String::new()
                                    };

                                    let old_content: String = del_spans.iter().map(|s| s.content.as_ref()).collect();
                                    let del_spans = apply_selection_to_content(del_spans, &selection, screen_row_idx, prefix_width);

                                    let (del_lines, del_row_infos) = wrap_content(
                                        del_spans,
                                        &old_content,
                                        del_prefix_str,
                                        "- ".to_string(),
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
                                let ins_spans = build_insertion_spans_with_highlight(&diff_line.inline_spans, ins_source);
                                let ins_spans = apply_selection_to_content(ins_spans, &selection, screen_row_idx, prefix_width);

                                let (ins_lines, ins_row_infos) = wrap_content(
                                    ins_spans,
                                    new_content,
                                    prefix_str.clone(),
                                    "+ ".to_string(),
                                    ins_style,
                                    content_width,
                                    prefix_width,
                                );

                                screen_row_idx += ins_lines.len();
                                all_lines.extend(ins_lines);
                                all_row_infos.extend(ins_row_infos);
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

                                let content_spans = apply_selection_to_content(content_spans, &selection, screen_row_idx, prefix_width);

                                let (lines, row_infos) = wrap_content(
                                    content_spans,
                                    &diff_line.content,
                                    prefix_str.clone(),
                                    "  ".to_string(),
                                    style,
                                    content_width,
                                    prefix_width,
                                );

                                screen_row_idx += lines.len();
                                all_lines.extend(lines);
                                all_row_infos.extend(row_infos);
                            }
                            InlineChangeType::NoChange => {}
                        }

                        if change_type != InlineChangeType::NoChange {
                            continue;
                        }
                    }

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

                    let content_spans = apply_selection_to_content(content_spans, &selection, screen_row_idx, prefix_width);

                    let prefix_char = format!("{} ", diff_line.prefix);
                    let (lines, row_infos) = wrap_content(
                        content_spans,
                        &diff_line.content,
                        prefix_str,
                        prefix_char,
                        style,
                        content_width,
                        prefix_width,
                    );

                    screen_row_idx += lines.len();
                    all_lines.extend(lines);
                    all_row_infos.extend(row_infos);
                } else {
                    let prefix_char = format!("{} ", diff_line.prefix);
                    let content_spans = vec![Span::styled(diff_line.content.clone(), style)];
                    let content_spans = apply_selection_to_content(content_spans, &selection, screen_row_idx, prefix_width);

                    let (lines, row_infos) = wrap_content(
                        content_spans,
                        &diff_line.content,
                        prefix_str,
                        prefix_char,
                        style,
                        content_width,
                        prefix_width,
                    );

                    screen_row_idx += lines.len();
                    all_lines.extend(lines);
                    all_row_infos.extend(row_infos);
                }
            }
        }
    }

    app.set_row_map(all_row_infos);

    let title = match app.current_file() {
        Some(ref file) => Line::from(vec![
            Span::styled(format!(" {} ", file), Style::default().fg(Color::White)),
        ]),
        None => Line::from(vec![
            Span::styled(" branchdiff ", Style::default().fg(Color::DarkGray)),
        ]),
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(all_lines).block(block);

    frame.render_widget(paragraph, area);
}
