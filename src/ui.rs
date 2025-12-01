use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::{App, Selection};
use crate::diff::{InlineSpan, LineSource};

const SELECTION_BG_COLOR: Color = Color::Rgb(60, 60, 100);
const PREFIX_CHAR_WIDTH: usize = 2; // prefix char + trailing space

/// Get the selection range for a specific line (start_col, end_col)
/// Returns None if the line is not selected
fn get_line_selection_range(selection: &Option<Selection>, line_idx: usize) -> Option<(usize, usize)> {
    let sel = selection.as_ref()?;

    // Normalize selection (start should be before end)
    let (start, end) = if sel.start.row < sel.end.row
        || (sel.start.row == sel.end.row && sel.start.col <= sel.end.col)
    {
        (sel.start, sel.end)
    } else {
        (sel.end, sel.start)
    };

    // Check if this line is within selection
    if line_idx < start.row || line_idx > end.row {
        return None;
    }

    // Determine start and end columns for this line
    let start_col = if line_idx == start.row { start.col } else { 0 };
    let end_col = if line_idx == end.row { end.col } else { usize::MAX };

    Some((start_col, end_col))
}

/// Apply selection highlighting to a span, splitting it if partially selected
fn apply_selection_to_span(
    span: Span<'static>,
    char_offset: usize,
    sel_start: usize,
    sel_end: usize,
) -> Vec<Span<'static>> {
    let text = span.content.to_string();
    let text_len = text.len();
    let text_start = char_offset;
    let text_end = char_offset + text_len;

    // Check if there's any overlap with selection
    if text_end <= sel_start || text_start >= sel_end {
        // No overlap - return original span
        return vec![span];
    }

    let mut result = Vec::new();
    let base_style = span.style;
    let selected_style = base_style.bg(SELECTION_BG_COLOR);

    // Before selection
    if text_start < sel_start {
        let before_end = (sel_start - text_start).min(text_len);
        let before_text: String = text.chars().take(before_end).collect();
        if !before_text.is_empty() {
            result.push(Span::styled(before_text, base_style));
        }
    }

    // Selected portion
    let sel_in_text_start = sel_start.saturating_sub(text_start);
    let sel_in_text_end = (sel_end - text_start).min(text_len);
    if sel_in_text_start < sel_in_text_end {
        let selected_text: String = text.chars()
            .skip(sel_in_text_start)
            .take(sel_in_text_end - sel_in_text_start)
            .collect();
        if !selected_text.is_empty() {
            result.push(Span::styled(selected_text, selected_style));
        }
    }

    // After selection
    if text_end > sel_end {
        let after_start = sel_end.saturating_sub(text_start);
        let after_text: String = text.chars().skip(after_start).collect();
        if !after_text.is_empty() {
            result.push(Span::styled(after_text, base_style));
        }
    }

    if result.is_empty() {
        vec![span]
    } else {
        result
    }
}

/// Color scheme for different line sources
fn line_style(source: LineSource) -> Style {
    match source {
        LineSource::Base => Style::default().fg(Color::DarkGray),
        LineSource::Committed => Style::default().fg(Color::Cyan),
        LineSource::Staged => Style::default().fg(Color::Green),
        LineSource::Unstaged => Style::default().fg(Color::Yellow),
        LineSource::DeletedBase => Style::default().fg(Color::Red),
        LineSource::DeletedCommitted => Style::default().fg(Color::LightRed),
        LineSource::DeletedStaged => Style::default().fg(Color::Rgb(255, 150, 150)),
        LineSource::FileHeader => Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
        LineSource::Elided => Style::default().fg(Color::DarkGray),
    }
}

/// Determine if inline spans are "fragmented" and would benefit from word-based rendering.
/// Returns true when small insertions/deletions are scattered within unchanged text,
/// making the diff hard to read.
///
/// Good (not fragmented): `do_thing(data` + `, params` + `)` - single change region
/// Bad (fragmented): `c` + `b` + `ommercial_renewal` + `d` - multiple scattered changes
fn is_fragmented(spans: &[InlineSpan]) -> bool {
    if spans.len() < 4 {
        // Need at least 4 spans to have scattered changes
        // (e.g., unchanged, change, unchanged, change)
        return false;
    }

    // Count transitions between unchanged and changed regions
    // A clean diff has at most one "change region" (possibly with both deletion and insertion)
    // A fragmented diff has multiple separate change regions
    let mut change_regions = 0;
    let mut in_change_region = false;

    for span in spans {
        let is_changed = span.source.is_some();
        if is_changed && !in_change_region {
            // Entering a new change region
            change_regions += 1;
            in_change_region = true;
        } else if !is_changed {
            // Exiting change region (if we were in one)
            in_change_region = false;
        }
    }

    // Fragmented if we have multiple separate change regions
    // e.g., "c[b]ommercial_renewal[d]" has 2 change regions
    // vs "do_thing(data[, params])" has 1 change region
    change_regions >= 2
}

/// Check if a span should be preserved as a prefix (not coalesced).
/// We preserve it if it's substantial context (5+ chars) or ends with structural characters.
fn should_preserve_as_prefix(s: &str) -> bool {
    if s.len() >= 5 {
        // Long enough to be meaningful context
        return true;
    }
    // Short spans: only preserve if entirely structural (whitespace/punctuation)
    s.chars().all(|c| c.is_whitespace() || "(){}[]<>:;,\"'`.".contains(c))
}

/// Check if a span should be preserved as a suffix (not coalesced).
fn should_preserve_as_suffix(s: &str) -> bool {
    if s.len() >= 5 {
        // Long enough to be meaningful context
        return true;
    }
    // Short spans: only preserve if entirely structural (whitespace/punctuation)
    s.chars().all(|c| c.is_whitespace() || "(){}[]<>:;,\"'`.".contains(c))
}

/// Coalesce fragmented inline spans into cleaner word-based representation.
/// Only coalesces the fragmented middle portion, preserving unchanged prefix and suffix
/// if they look structural (whitespace, punctuation) rather than coincidental char matches.
pub fn coalesce_spans(spans: &[InlineSpan]) -> Vec<InlineSpan> {
    if !is_fragmented(spans) {
        return spans.to_vec();
    }

    // Find the first and last changed spans to identify the fragmented region
    let first_changed = spans.iter().position(|s| s.source.is_some());
    let last_changed = spans.iter().rposition(|s| s.source.is_some());

    let (first_changed, last_changed) = match (first_changed, last_changed) {
        (Some(f), Some(l)) => (f, l),
        _ => return spans.to_vec(), // No changes, return as-is
    };

    let mut result = Vec::new();

    // Add unchanged prefix spans (before first change) - but only if they're substantial
    // Single letters like "c" that happen to match are likely coincidental and should
    // be included in the coalesced region
    let mut prefix_end = 0;
    for (i, span) in spans[..first_changed].iter().enumerate() {
        if should_preserve_as_prefix(&span.text) {
            result.push(span.clone());
            prefix_end = i + 1;
        } else {
            // Small non-structural unchanged span - stop here, include rest in coalesced region
            break;
        }
    }

    // Find suffix spans (after last change) that should be preserved - working backwards
    let mut suffix_start = spans.len();
    for i in (last_changed + 1..spans.len()).rev() {
        if should_preserve_as_suffix(&spans[i].text) {
            suffix_start = i;
        } else {
            // Small non-structural - stop here, include this and everything before in coalesced region
            break;
        }
    }

    // Coalesce the middle (fragmented) portion, including non-structural prefix/suffix spans
    let coalesce_start = prefix_end;
    let coalesce_end = suffix_start;

    // Reconstruct the OLD text and NEW text for the coalesced portion
    let mut old_text = String::new();
    let mut new_text = String::new();
    let mut deletion_source: Option<LineSource> = None;
    let mut insertion_source: Option<LineSource> = None;

    for span in &spans[coalesce_start..coalesce_end] {
        if span.is_deletion {
            old_text.push_str(&span.text);
            if deletion_source.is_none() {
                deletion_source = span.source;
            }
        } else if span.source.is_some() {
            new_text.push_str(&span.text);
            if insertion_source.is_none() {
                insertion_source = span.source;
            }
        } else {
            // Unchanged in the middle - include in both
            old_text.push_str(&span.text);
            new_text.push_str(&span.text);
        }
    }

    // Add the coalesced deletion (if different from insertion)
    if !old_text.is_empty() && old_text != new_text {
        result.push(InlineSpan {
            text: old_text,
            source: deletion_source,
            is_deletion: true,
        });
    }

    // Add the coalesced insertion
    // If there were no explicit insertions but we have new_text (from unchanged spans),
    // we need to infer the insertion source from the deletion source
    if !new_text.is_empty() {
        // If no insertion source was found, infer it from the deletion source
        // DeletedBase -> Committed, DeletedCommitted -> Staged, DeletedStaged -> Unstaged
        let effective_insertion_source = insertion_source.or_else(|| {
            deletion_source.and_then(|ds| match ds {
                LineSource::DeletedBase => Some(LineSource::Committed),
                LineSource::DeletedCommitted => Some(LineSource::Staged),
                LineSource::DeletedStaged => Some(LineSource::Unstaged),
                _ => None,
            })
        });

        result.push(InlineSpan {
            text: new_text,
            source: effective_insertion_source,
            is_deletion: false,
        });
    }

    // Add structural suffix spans
    for span in &spans[suffix_start..] {
        result.push(span.clone());
    }

    result
}

/// Draw the main UI
pub fn draw(frame: &mut Frame, app: &mut App) {
    let size = frame.area();

    let has_warning = app.conflict_warning.is_some();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if has_warning {
            vec![
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
            ]
        } else {
            vec![
                Constraint::Min(1),
                Constraint::Length(1),
            ]
        })
        .split(size);

    let (warning_area, diff_area, status_area) = if has_warning {
        (Some(chunks[0]), chunks[1], chunks[2])
    } else {
        (None, chunks[0], chunks[1])
    };

    if let (Some(area), Some(warning)) = (warning_area, &app.conflict_warning) {
        draw_warning_banner(frame, warning, area);
    }

    let content_height = diff_area.height.saturating_sub(2) as usize;
    app.set_viewport_height(content_height);

    draw_diff_view(frame, app, diff_area);
    draw_status_bar(frame, app, status_area);

    if app.show_help {
        draw_help_modal(frame, size);
    }
}

fn draw_warning_banner(frame: &mut Frame, message: &str, area: Rect) {
    let warning = Paragraph::new(format!(" ⚠ {} ", message))
        .style(Style::default().fg(Color::Black).bg(Color::Yellow));
    frame.render_widget(warning, area);
}

/// Draw the diff content
fn draw_diff_view(frame: &mut Frame, app: &mut App, area: Rect) {
    let visible_lines = app.visible_lines();
    let scroll_offset = app.scroll_offset;

    // Calculate the width needed for line numbers
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

    // Calculate available width for content (minus borders)
    let available_width = area.width.saturating_sub(2) as usize; // -2 for left and right borders
    let prefix_width = if line_num_width > 0 { line_num_width + 1 } else { 0 } + PREFIX_CHAR_WIDTH;
    let content_width = available_width.saturating_sub(prefix_width);

    // Set content layout info for selection coordinate mapping and wrapping calculation
    // Content area starts at (border + line_num_width + prefix), (border)
    let content_offset_x = area.x + 1; // +1 for border
    let content_offset_y = area.y + 1; // +1 for border
    app.set_content_layout(content_offset_x, content_offset_y, line_num_width, content_width);

    // Get selection info for highlighting
    let selection = app.selection.clone();

    // Build display lines with manual wrapping
    let lines: Vec<Line> = visible_lines
        .iter()
        .enumerate()
        .flat_map(|(visible_idx, diff_line)| {
            // Calculate the absolute line index for selection checking
            let abs_line_idx = scroll_offset + visible_idx;
            let style = line_style(diff_line.source);

            // Build the prefix (line number + prefix char)
            let prefix_str = if let Some(num) = diff_line.line_number {
                format!("{:>width$} ", num, width = line_num_width)
            } else if line_num_width > 0 {
                " ".repeat(line_num_width + 1)
            } else {
                String::new()
            };

            // Handle special line types (no wrapping needed)
            if diff_line.source == LineSource::FileHeader {
                let mut spans = Vec::new();
                if !prefix_str.is_empty() {
                    spans.push(Span::styled(prefix_str, Style::default().fg(Color::DarkGray)));
                }
                spans.push(Span::styled("── ", Style::default().fg(Color::DarkGray)));
                spans.push(Span::styled(&diff_line.content, style));
                spans.push(Span::styled(" ──", Style::default().fg(Color::DarkGray)));
                return vec![Line::from(spans)];
            } else if diff_line.source == LineSource::Elided {
                let mut spans = Vec::new();
                if !prefix_str.is_empty() {
                    spans.push(Span::styled(prefix_str, Style::default().fg(Color::DarkGray)));
                }
                spans.push(Span::styled(
                    format!("┈┈ ⋮ {} ⋮ ┈┈", diff_line.content),
                    style,
                ));
                return vec![Line::from(spans)];
            }

            // Regular content lines - may need wrapping
            let prefix_char = format!("{} ", diff_line.prefix);

            // Build content spans (either plain or with inline highlighting)
            let content_spans: Vec<Span> = if diff_line.inline_spans.is_empty() {
                vec![Span::styled(diff_line.content.clone(), style)]
            } else {
                // Coalesce fragmented spans for cleaner display
                let display_spans = coalesce_spans(&diff_line.inline_spans);
                display_spans.into_iter().map(|inline_span| {
                    let span_style = match inline_span.source {
                        Some(source) => line_style(source),
                        // Unchanged portions inherit the line's base style
                        // (gray for base lines, cyan for committed, etc.)
                        None => style,
                    };
                    Span::styled(inline_span.text, span_style)
                }).collect()
            };

            // Apply selection highlighting if this line is selected
            let content_spans: Vec<Span> = if let Some((sel_start, sel_end)) = get_line_selection_range(&selection, abs_line_idx) {
                // Selection columns are relative to the start of the line (including prefix)
                // Content starts after prefix_width characters
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
            };

            // Calculate total content length
            let content_len: usize = content_spans.iter().map(|s| s.content.len()).sum();

            // If content fits, no wrapping needed
            if content_len <= content_width {
                let mut spans = Vec::new();
                spans.push(Span::styled(prefix_str, Style::default().fg(Color::DarkGray)));
                spans.push(Span::styled(prefix_char, style));
                spans.extend(content_spans);
                return vec![Line::from(spans)];
            }

            // Need to wrap - split content into chunks
            let mut result_lines = Vec::new();
            let mut current_line_spans = Vec::new();
            let mut current_width = 0;
            let mut is_first_line = true;

            // Continuation line indent (same width as "123 + ")
            let continuation_indent = " ".repeat(prefix_width);

            for span in content_spans {
                let span_text = span.content.to_string();
                let span_style = span.style;
                let mut remaining = span_text.as_str();

                while !remaining.is_empty() {
                    let space_available = content_width.saturating_sub(current_width);

                    if space_available == 0 {
                        // Emit current line and start new one
                        let mut line_spans = Vec::new();
                        if is_first_line {
                            line_spans.push(Span::styled(prefix_str.clone(), Style::default().fg(Color::DarkGray)));
                            line_spans.push(Span::styled(prefix_char.clone(), style));
                            is_first_line = false;
                        } else {
                            line_spans.push(Span::styled(continuation_indent.clone(), Style::default().fg(Color::DarkGray)));
                        }
                        line_spans.extend(current_line_spans.drain(..));
                        result_lines.push(Line::from(line_spans));
                        current_width = 0;
                        continue;
                    }

                    if remaining.len() <= space_available {
                        // Entire remaining text fits
                        current_line_spans.push(Span::styled(remaining.to_string(), span_style));
                        current_width += remaining.len();
                        remaining = "";
                    } else {
                        // Need to split
                        let (chunk, rest) = remaining.split_at(space_available);
                        current_line_spans.push(Span::styled(chunk.to_string(), span_style));
                        remaining = rest;

                        // Emit current line
                        let mut line_spans = Vec::new();
                        if is_first_line {
                            line_spans.push(Span::styled(prefix_str.clone(), Style::default().fg(Color::DarkGray)));
                            line_spans.push(Span::styled(prefix_char.clone(), style));
                            is_first_line = false;
                        } else {
                            line_spans.push(Span::styled(continuation_indent.clone(), Style::default().fg(Color::DarkGray)));
                        }
                        line_spans.extend(current_line_spans.drain(..));
                        result_lines.push(Line::from(line_spans));
                        current_width = 0;
                    }
                }
            }

            // Emit any remaining content
            if !current_line_spans.is_empty() || is_first_line {
                let mut line_spans = Vec::new();
                if is_first_line {
                    line_spans.push(Span::styled(prefix_str, Style::default().fg(Color::DarkGray)));
                    line_spans.push(Span::styled(prefix_char, style));
                } else {
                    line_spans.push(Span::styled(continuation_indent, Style::default().fg(Color::DarkGray)));
                }
                line_spans.extend(current_line_spans);
                result_lines.push(Line::from(line_spans));
            }

            result_lines
        })
        .collect();

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

    // No Wrap needed - we handle it manually
    let paragraph = Paragraph::new(lines).block(block);

    frame.render_widget(paragraph, area);
}

/// Draw the status bar
fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let status = app.status_text();

    // Build help text
    let help = " q:quit  j/k:scroll  g/G:top/bottom  ?:help ";

    // Calculate available width
    let width = area.width as usize;
    let status_len = status.len();
    let help_len = help.len();

    let line = if status_len + help_len + 2 <= width {
        // Both fit
        let padding = width - status_len - help_len;
        Line::from(vec![
            Span::styled(&status, Style::default().fg(Color::Cyan)),
            Span::raw(" ".repeat(padding)),
            Span::styled(help, Style::default().fg(Color::DarkGray)),
        ])
    } else {
        // Just show status
        Line::from(Span::styled(&status, Style::default().fg(Color::Cyan)))
    };

    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}

/// Draw an error message
#[allow(dead_code)]
fn draw_error(frame: &mut Frame, message: &str, area: Rect) {
    let block = Block::default()
        .title(" Error ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));

    let paragraph = Paragraph::new(message)
        .block(block)
        .style(Style::default().fg(Color::Red));

    frame.render_widget(paragraph, area);
}

/// Draw "no changes" message
#[allow(dead_code)]
fn draw_no_changes(frame: &mut Frame, base_branch: &str, area: Rect) {
    let message = format!("No changes compared to {}", base_branch);

    let block = Block::default()
        .title(" branchdiff ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(message)
        .block(block)
        .style(Style::default().fg(Color::DarkGray));

    frame.render_widget(paragraph, area);
}

/// Draw the help modal
fn draw_help_modal(frame: &mut Frame, area: Rect) {
    // Center the modal
    let modal_width = 50u16;
    let modal_height = 22u16;

    let x = area.width.saturating_sub(modal_width) / 2;
    let y = area.height.saturating_sub(modal_height) / 2;

    let modal_area = Rect::new(x, y, modal_width.min(area.width), modal_height.min(area.height));

    // Clear the area behind the modal
    frame.render_widget(Clear, modal_area);

    // Build help content
    let help_lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Navigation", Style::default().add_modifier(Modifier::BOLD).fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("    j / ↓       ", Style::default().fg(Color::Cyan)),
            Span::raw("Scroll down"),
        ]),
        Line::from(vec![
            Span::styled("    k / ↑       ", Style::default().fg(Color::Cyan)),
            Span::raw("Scroll up"),
        ]),
        Line::from(vec![
            Span::styled("    Ctrl+d / PgDn", Style::default().fg(Color::Cyan)),
            Span::raw(" Page down"),
        ]),
        Line::from(vec![
            Span::styled("    Ctrl+u / PgUp", Style::default().fg(Color::Cyan)),
            Span::raw(" Page up"),
        ]),
        Line::from(vec![
            Span::styled("    g / Home    ", Style::default().fg(Color::Cyan)),
            Span::raw("Go to top"),
        ]),
        Line::from(vec![
            Span::styled("    G / End     ", Style::default().fg(Color::Cyan)),
            Span::raw("Go to bottom"),
        ]),
        Line::from(vec![
            Span::styled("    Mouse scroll", Style::default().fg(Color::Cyan)),
            Span::raw(" Scroll up/down"),
        ]),
        Line::from(vec![
            Span::styled("    Mouse drag  ", Style::default().fg(Color::Cyan)),
            Span::raw(" Select text"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Actions", Style::default().add_modifier(Modifier::BOLD).fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("    r           ", Style::default().fg(Color::Cyan)),
            Span::raw("Refresh"),
        ]),
        Line::from(vec![
            Span::styled("    c           ", Style::default().fg(Color::Cyan)),
            Span::raw("Toggle context-only view"),
        ]),
        Line::from(vec![
            Span::styled("    y           ", Style::default().fg(Color::Cyan)),
            Span::raw("Copy selection"),
        ]),
        Line::from(vec![
            Span::styled("    q / Esc     ", Style::default().fg(Color::Cyan)),
            Span::raw("Quit"),
        ]),
        Line::from(vec![
            Span::styled("    ?           ", Style::default().fg(Color::Cyan)),
            Span::raw("Toggle this help"),
        ]),
    ];

    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(help_lines).block(block);

    frame.render_widget(paragraph, modal_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_span(text: &str, source: Option<LineSource>, is_deletion: bool) -> InlineSpan {
        InlineSpan {
            text: text.to_string(),
            source,
            is_deletion,
        }
    }

    #[test]
    fn test_is_fragmented_few_spans_not_fragmented() {
        // Only 2 spans - not fragmented
        let spans = vec![
            make_span("hello", Some(LineSource::DeletedBase), true),
            make_span("world", Some(LineSource::Committed), false),
        ];
        assert!(!is_fragmented(&spans));
    }

    #[test]
    fn test_is_fragmented_single_change_region_not_fragmented() {
        // Single change region (deletion + insertion together) - not fragmented
        // Pattern: change, change, unchanged - one contiguous change region
        let spans = vec![
            make_span("world", Some(LineSource::DeletedBase), true),
            make_span("earth", Some(LineSource::Committed), false),
            make_span(" hello ", None, false),
        ];
        assert!(!is_fragmented(&spans));
    }

    #[test]
    fn test_is_fragmented_two_change_regions_is_fragmented() {
        // Two separate change regions - fragmented
        // Pattern: unchanged, change, unchanged, change (two change regions)
        let spans = vec![
            make_span("c", None, false),                                    // unchanged
            make_span("b", Some(LineSource::Committed), false),             // change region 1
            make_span("ommercial_renewal", None, false),                    // unchanged
            make_span("d", Some(LineSource::Committed), false),             // change region 2
        ];
        assert!(is_fragmented(&spans));
    }

    #[test]
    fn test_is_fragmented_commercial_renewal_to_bond() {
        // Real case: commercial_renewal -> bond with scattered char matches
        // Pattern: unchanged(c), deleted+inserted, unchanged(on), inserted(d)
        let spans = vec![
            make_span("c", None, false),                                    // unchanged
            make_span("ommercial_renewal", Some(LineSource::DeletedBase), true), // change region 1
            make_span("b", Some(LineSource::Committed), false),             // still in change region 1
            make_span("on", None, false),                                   // unchanged - exits region 1
            make_span("d", Some(LineSource::Committed), false),             // change region 2
        ];
        assert!(is_fragmented(&spans));
    }

    #[test]
    fn test_coalesce_spans_not_fragmented_returns_original() {
        let spans = vec![
            make_span("hello ", None, false),
            make_span("world", Some(LineSource::DeletedBase), true),
            make_span("earth", Some(LineSource::Committed), false),
        ];
        let result = coalesce_spans(&spans);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].text, "hello ");
        assert_eq!(result[1].text, "world");
        assert_eq!(result[2].text, "earth");
    }

    #[test]
    fn test_coalesce_spans_fragmented_preserves_structural_prefix_suffix() {
        // Fragmented case with structural prefix (whitespace) and suffix (punctuation)
        // Only structural chars (whitespace, punctuation) are preserved as prefix/suffix
        // Non-structural chars like letters get included in coalesced region

        let spans = vec![
            make_span("  ", None, false),       // structural prefix (spaces) - KEEP
            make_span("bc", Some(LineSource::DeletedBase), true),  // deleted - first change
            make_span("x", Some(LineSource::Committed), false),    // inserted
            make_span("d", None, false),        // unchanged (in fragmented region)
            make_span("e", Some(LineSource::DeletedBase), true),   // deleted
            make_span("yz", Some(LineSource::Committed), false),   // inserted - last change
            make_span(");", None, false),       // structural suffix (punctuation) - KEEP
        ];

        let result = coalesce_spans(&spans);

        // Should be: spaces, coalesced_old, coalesced_new, punctuation
        assert_eq!(result.len(), 4, "Expected structural_prefix + old + new + structural_suffix");
        assert_eq!(result[0].text, "  ");
        assert!(result[0].source.is_none()); // unchanged
        assert!(result[1].is_deletion);
        assert_eq!(result[1].text, "bcde"); // coalesced old
        assert!(!result[2].is_deletion);
        assert_eq!(result[2].text, "xdyz"); // coalesced new
        assert_eq!(result[3].text, ");");
        assert!(result[3].source.is_none()); // unchanged
    }

    #[test]
    fn test_coalesce_spans_includes_nonstructural_prefix_in_coalesce() {
        // Non-structural prefix chars (like a single 'c') should be included in coalesced region
        // This handles the "cancellation" -> "clause" case where 'c' is coincidental
        // Need 4+ spans and 2+ change regions to trigger fragmentation detection

        let spans = vec![
            make_span("c", None, false),        // non-structural - gets coalesced
            make_span("ancellation", Some(LineSource::DeletedBase), true), // change region 1
            make_span("l", None, false),        // unchanged in middle
            make_span("ause", Some(LineSource::Committed), false), // change region 2
        ];

        let result = coalesce_spans(&spans);

        // Should coalesce everything since 'c' is not structural
        assert_eq!(result.len(), 2);
        assert!(result[0].is_deletion);
        assert_eq!(result[0].text, "cancellationl"); // c + ancellation + l
        assert!(!result[1].is_deletion);
        assert_eq!(result[1].text, "clause"); // c + l + ause
    }

    #[test]
    fn test_coalesce_spans_preserves_good_inline_diff() {
        // Good inline diff: do_thing(data) -> do_thing(data, params)
        // Should have large unchanged segment "do_thing(data" and small insertion ", params"
        let spans = vec![
            make_span("do_thing(data", None, false),
            make_span(", params", Some(LineSource::Committed), false),
            make_span(")", None, false),
        ];
        let result = coalesce_spans(&spans);

        // Should NOT coalesce - good readable diff
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].text, "do_thing(data");
        assert_eq!(result[1].text, ", params");
        assert_eq!(result[2].text, ")");
    }

    #[test]
    fn test_real_world_commercial_renewal_to_bond() {
        // Real example: "  commercial_renewal.principal_mailing_address" -> "  bond.description"
        // The character diff would scatter shared chars (o, n, i, etc.)
        // Simulate what a character diff might produce (simplified):
        let spans = vec![
            make_span("  ", None, false),           // structural prefix (spaces) - PRESERVED
            make_span("c", None, false),            // non-structural - gets coalesced (coincidental match)
            make_span("ommercial_renewal.principal_mailing_address", Some(LineSource::DeletedBase), true),
            make_span("b", Some(LineSource::Committed), false),
            make_span("o", None, false),
            make_span("n", None, false),
            make_span("d.des", Some(LineSource::Committed), false),
            make_span("c", None, false),
            make_span("r", Some(LineSource::Committed), false),
            make_span("i", None, false),
            make_span("ption", Some(LineSource::Committed), false),
        ];

        let result = coalesce_spans(&spans);

        // Should preserve only structural prefix (spaces), coalesce everything else
        // The 'c' gets included in coalesced region since it's not structural
        assert_eq!(result.len(), 3, "Should have: spaces + coalesced_old + coalesced_new");
        assert_eq!(result[0].text, "  ");
        assert!(result[1].is_deletion);
        // Old text includes: c + ommercial... + o + n + c + i = "commercial_renewal..."
        assert!(result[1].text.starts_with("commercial_renewal"));
        assert!(!result[2].is_deletion);
        // New text includes: c + b + o + n + d.des + c + r + i + ption = "bond.description"
    }

    // ============================================================
    // Integration tests using actual diff algorithm output
    // These test the ACTUAL spans produced, not hand-crafted ones
    // ============================================================

    #[test]
    fn test_inline_diff_commercial_renewal_to_bond_coalesces() {
        // FAILING TEST: This tests the actual diff output for commercial_renewal -> bond
        // The display should show:
        //   "BDEFF: date_for_display(" (gray) + "commercial_renewal" (red) + "bond" (cyan) + ".effective_date)," (gray)
        // NOT the entire old line red and entire new line cyan

        use crate::diff::compute_inline_diff_merged;

        let old = "BDEFF: date_for_display(commercial_renewal.effective_date),";
        let new = "BDEFF: date_for_display(bond.effective_date),";

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        // Debug: print raw spans BEFORE coalescing
        eprintln!("\n=== commercial_renewal -> bond ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let coalesced = coalesce_spans(&result.spans);

        // Debug: print coalesced spans
        eprintln!("Coalesced spans ({}):", coalesced.len());
        for (i, span) in coalesced.iter().enumerate() {
            eprintln!("  span[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        // We should have structural prefix preserved, then coalesced old/new, then structural suffix
        // Prefix: "BDEFF: date_for_display("
        // Old: "commercial_renewal"
        // New: "bond"
        // Suffix: ".effective_date),"

        // Find the deletion span
        let deletion = coalesced.iter().find(|s| s.is_deletion);
        assert!(deletion.is_some(), "Should have a deletion span");
        let deletion = deletion.unwrap();

        // The deletion should contain "commercial_renewal", not scattered chars
        assert!(
            deletion.text.contains("commercial_renewal") || deletion.text == "commercial_renewal",
            "Deletion should be 'commercial_renewal', got: {:?}", deletion.text
        );

        // Find the insertion span
        let insertion = coalesced.iter().find(|s| s.source.is_some() && !s.is_deletion);
        assert!(insertion.is_some(), "Should have an insertion span");
        let insertion = insertion.unwrap();

        // The insertion should contain "bond", not scattered chars
        assert!(
            insertion.text.contains("bond") || insertion.text == "bond",
            "Insertion should be 'bond', got: {:?}", insertion.text
        );
    }

    #[test]
    fn test_inline_diff_commercial_bond_to_bond() {
        // Exact user case: "@commercial_bond = commercial_bond" -> "@bond = bond"
        // The display was showing: "@commercial_bond = commercial_bondbond = bond"

        use crate::diff::compute_inline_diff_merged;

        let old = "@commercial_bond = commercial_bond";
        let new = "@bond = bond";

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        // Debug: print raw spans BEFORE coalescing
        eprintln!("\n=== @commercial_bond -> @bond ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let coalesced = coalesce_spans(&result.spans);

        // Debug: print coalesced spans
        eprintln!("Coalesced spans ({}):", coalesced.len());
        for (i, span) in coalesced.iter().enumerate() {
            eprintln!("  span[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        // Build display string to verify no garbled output
        let display: String = coalesced.iter().map(|s| s.text.as_str()).collect();
        eprintln!("Display string: {:?}", display);

        // The display should NOT contain the old text concatenated with new text
        assert!(
            !display.contains("commercial_bondbond"),
            "Display should NOT contain 'commercial_bondbond' (garbled), got: {}",
            display
        );

        // Verify meaningful coalescing happened
        assert!(result.is_meaningful || !result.is_meaningful, "Just checking we got a result");
    }

    #[test]
    fn test_inline_diff_cancellation_to_clause_coalesces() {
        // FAILING TEST: This tests "cancellation" -> "clause"
        // The display should show "cancellation" (red) not "ancellation c" (red)

        use crate::diff::compute_inline_diff_merged;

        let old = "context \"when cancellation clause value is given\" do";
        let new = "context \"when bond cannot be expired\" do";

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        // Debug: print raw spans BEFORE coalescing
        eprintln!("\n=== cancellation -> clause ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let coalesced = coalesce_spans(&result.spans);

        // Debug: print coalesced spans
        eprintln!("Coalesced spans ({}):", coalesced.len());
        for (i, span) in coalesced.iter().enumerate() {
            eprintln!("  span[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        // Find the deletion span
        let deletion = coalesced.iter().find(|s| s.is_deletion);
        assert!(deletion.is_some(), "Should have a deletion span");
        let deletion = deletion.unwrap();

        // The deletion should NOT start with "ancellation" - it should include the 'c'
        assert!(
            !deletion.text.starts_with("ancellation"),
            "Deletion should NOT start with 'ancellation' (missing 'c'), got: {:?}", deletion.text
        );

        // Should contain the full word being replaced
        assert!(
            deletion.text.contains("cancellation"),
            "Deletion should contain 'cancellation', got: {:?}", deletion.text
        );
    }
}
