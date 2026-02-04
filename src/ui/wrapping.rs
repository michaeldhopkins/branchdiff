use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::ScreenRowInfo;

/// Compute the display width of content accounting for tab expansion and
/// control character replacement. Must match `sanitize_for_display` behavior
/// so that height estimation agrees with actual rendering.
pub fn content_display_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthChar;
    s.chars()
        .map(|ch| {
            if ch == '\t' {
                4
            } else {
                UnicodeWidthChar::width(ch).unwrap_or(1)
            }
        })
        .sum()
}

/// Replace characters that cause terminal rendering artifacts.
/// Tabs expand to 4 spaces; other control characters (unicode-width None)
/// become spaces so they have predictable 1-column display width.
fn sanitize_for_display(s: &str) -> Option<String> {
    use unicode_width::UnicodeWidthChar;
    if !s
        .chars()
        .any(|ch| ch == '\t' || UnicodeWidthChar::width(ch).is_none())
    {
        return None;
    }
    let mut result = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch == '\t' {
            result.push_str("    ");
        } else if UnicodeWidthChar::width(ch).is_none() {
            result.push(' ');
        } else {
            result.push(ch);
        }
    }
    Some(result)
}

/// Find the byte offset where accumulated display width reaches `max_width`.
/// Returns `s.len()` if the entire string fits within `max_width`.
fn display_width_split(s: &str, max_width: usize) -> usize {
    let mut width = 0;
    for (i, ch) in s.char_indices() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cw > max_width {
            return i;
        }
        width += cw;
    }
    s.len()
}

/// Wrap content spans into multiple lines if needed, returning Lines and ScreenRowInfo entries
pub fn wrap_content(
    content_spans: Vec<Span<'static>>,
    content: &str,
    prefix_str: String,
    prefix_char: String,
    style: Style,
    content_width: usize,
    prefix_width: usize,
) -> (Vec<Line<'static>>, Vec<ScreenRowInfo>) {
    // Sanitize control characters that cause terminal rendering artifacts:
    // tabs expand to 4 spaces, other control chars become spaces.
    let content_spans: Vec<Span<'static>> = content_spans
        .into_iter()
        .map(|span| match sanitize_for_display(&span.content) {
            Some(sanitized) => Span::styled(sanitized, span.style),
            None => span,
        })
        .collect();
    let content = sanitize_for_display(content)
        .unwrap_or_else(|| content.to_string());

    let content_display_width: usize = content_spans.iter().map(|s| s.content.width()).sum();

    // If content fits, no wrapping needed
    if content_display_width <= content_width {
        let mut spans = Vec::new();
        spans.push(Span::styled(prefix_str, Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(prefix_char, style));
        spans.extend(content_spans);

        let row_info = ScreenRowInfo {
            content,
            is_file_header: false,
            file_path: None,
            is_continuation: false,
        };

        return (vec![Line::from(spans)], vec![row_info]);
    }

    // Need to wrap - split content into chunks
    let mut result_lines = Vec::new();
    let mut row_infos = Vec::new();
    let mut current_line_spans = Vec::new();
    let mut current_width = 0;
    let mut is_first_line = true;
    let mut current_content = String::new();

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
                line_spans.append(&mut current_line_spans);
                result_lines.push(Line::from(line_spans));

                row_infos.push(ScreenRowInfo {
                    content: std::mem::take(&mut current_content),
                    is_file_header: false,
                    file_path: None,
                    is_continuation: !row_infos.is_empty(),
                });

                current_width = 0;
                continue;
            }

            let remaining_display_width = remaining.width();

            if remaining_display_width <= space_available {
                // Entire remaining text fits
                current_line_spans.push(Span::styled(remaining.to_string(), span_style));
                current_content.push_str(remaining);
                current_width += remaining_display_width;
                remaining = "";
            } else {
                // Split at display width boundary, taking at least one char
                let split_at = display_width_split(remaining, space_available)
                    .max(remaining.ceil_char_boundary(1));
                let (chunk, rest) = remaining.split_at(split_at);
                current_line_spans.push(Span::styled(chunk.to_string(), span_style));
                current_content.push_str(chunk);
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
                line_spans.append(&mut current_line_spans);
                result_lines.push(Line::from(line_spans));

                row_infos.push(ScreenRowInfo {
                    content: std::mem::take(&mut current_content),
                    is_file_header: false,
                    file_path: None,
                    is_continuation: !row_infos.is_empty(),
                });

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

        row_infos.push(ScreenRowInfo {
            content: current_content,
            is_file_header: false,
            file_path: None,
            is_continuation: !row_infos.is_empty(),
        });
    }

    (result_lines, row_infos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrapped_line_widths_do_not_exceed_available_width() {
        let content_width = 40;
        let prefix_width = 8;

        let content = "a".repeat(content_width + 20);
        let content_spans = vec![Span::styled(content.clone(), Style::default())];

        // '±' prefix: 2 bytes, 1 display column — historically caused overflow
        let prefix_str = "    ".to_string();
        let prefix_char = "± C ".to_string();

        let (lines, _) = wrap_content(
            content_spans,
            &content,
            prefix_str,
            prefix_char,
            Style::default(),
            content_width,
            prefix_width,
        );

        assert!(lines.len() > 1, "Content should wrap");

        for (i, line) in lines.iter().enumerate() {
            let display_width: usize = line
                .spans
                .iter()
                .map(|s| s.content.width())
                .sum();
            assert!(
                display_width <= prefix_width + content_width,
                "Line {} has display width {} but max is {} (prefix {} + content {})",
                i,
                display_width,
                prefix_width + content_width,
                prefix_width,
                content_width
            );
        }
    }

    #[test]
    fn test_wrapped_unicode_content_correct_width() {
        let content_width = 20;
        let prefix_width = 6;

        // Mix of ASCII and multi-byte chars (→ is 1 display col, 3 bytes)
        let content = "→ hello → world → foo → bar → baz";
        let content_spans = vec![Span::styled(content.to_string(), Style::default())];

        let (lines, _) = wrap_content(
            content_spans,
            content,
            "  ".to_string(),
            "+ C ".to_string(),
            Style::default(),
            content_width,
            prefix_width,
        );

        for (i, line) in lines.iter().enumerate() {
            let display_width: usize = line
                .spans
                .iter()
                .map(|s| s.content.width())
                .sum();
            assert!(
                display_width <= prefix_width + content_width,
                "Line {} display width {} exceeds max {}",
                i,
                display_width,
                prefix_width + content_width
            );
        }
    }

    #[test]
    fn test_no_wrap_when_content_fits() {
        let content_width = 40;
        let prefix_width = 6;

        let content = "short line";
        let content_spans = vec![Span::styled(content.to_string(), Style::default())];

        let (lines, row_infos) = wrap_content(
            content_spans,
            content,
            "  ".to_string(),
            "+ C ".to_string(),
            Style::default(),
            content_width,
            prefix_width,
        );

        assert_eq!(lines.len(), 1);
        assert_eq!(row_infos.len(), 1);
        assert!(!row_infos[0].is_continuation);
    }

    #[test]
    fn test_continuation_lines_marked_correctly() {
        let content_width = 10;
        let prefix_width = 6;

        let content = "a".repeat(25);
        let content_spans = vec![Span::styled(content.clone(), Style::default())];

        let (lines, row_infos) = wrap_content(
            content_spans,
            &content,
            "  ".to_string(),
            "+ C ".to_string(),
            Style::default(),
            content_width,
            prefix_width,
        );

        assert!(lines.len() >= 3, "Should produce 3+ wrapped lines");
        assert!(!row_infos[0].is_continuation);
        for info in &row_infos[1..] {
            assert!(info.is_continuation);
        }
    }

    #[test]
    fn test_wide_cjk_characters_wrap_correctly() {
        let content_width = 10;
        let prefix_width = 6;

        // Each CJK char is 2 display columns, so 5 chars = 10 columns = fills exactly
        // 6 chars = 12 columns = should wrap
        let content = "你好世界你好";
        let content_spans = vec![Span::styled(content.to_string(), Style::default())];

        let (lines, _) = wrap_content(
            content_spans,
            content,
            "  ".to_string(),
            "+ C ".to_string(),
            Style::default(),
            content_width,
            prefix_width,
        );

        for (i, line) in lines.iter().enumerate() {
            let display_width: usize = line
                .spans
                .iter()
                .map(|s| s.content.width())
                .sum();
            assert!(
                display_width <= prefix_width + content_width,
                "Line {} display width {} exceeds max {}",
                i,
                display_width,
                prefix_width + content_width
            );
        }
    }

    #[test]
    fn test_tab_characters_expanded_before_wrapping() {
        let content_width = 30;
        let prefix_width = 6;

        // Two tabs + text: without expansion, unicode-width sees tabs as 0-width
        // and thinks the line fits. With expansion (4 spaces each), width is correct.
        let content = "\t\tsome_long_identifier = value;";
        let content_spans = vec![Span::styled(content.to_string(), Style::default())];

        let (lines, row_infos) = wrap_content(
            content_spans,
            content,
            "  ".to_string(),
            "+ C ".to_string(),
            Style::default(),
            content_width,
            prefix_width,
        );

        // Content after tab expansion: "        some_long_identifier = value;" = 38 chars
        // This exceeds content_width (30), so wrapping should occur
        assert!(lines.len() > 1, "Tab-containing content should wrap when expanded width exceeds limit");

        // Verify no tabs remain in rendered spans
        for line in &lines {
            for span in &line.spans {
                assert!(
                    !span.content.contains('\t'),
                    "Rendered spans should not contain tab characters"
                );
            }
        }

        // Verify no tabs in ScreenRowInfo content
        for info in &row_infos {
            assert!(
                !info.content.contains('\t'),
                "ScreenRowInfo content should not contain tabs"
            );
        }

        // Verify display widths don't exceed limits
        for (i, line) in lines.iter().enumerate() {
            let display_width: usize = line
                .spans
                .iter()
                .map(|s| s.content.width())
                .sum();
            assert!(
                display_width <= prefix_width + content_width,
                "Line {} display width {} exceeds max {}",
                i,
                display_width,
                prefix_width + content_width
            );
        }
    }

    #[test]
    fn test_tabs_without_wrapping_still_expanded() {
        let content_width = 40;
        let prefix_width = 6;

        // Short content with a tab - fits even after expansion
        let content = "\thi";
        let content_spans = vec![Span::styled(content.to_string(), Style::default())];

        let (lines, row_infos) = wrap_content(
            content_spans,
            content,
            "  ".to_string(),
            "+ C ".to_string(),
            Style::default(),
            content_width,
            prefix_width,
        );

        assert_eq!(lines.len(), 1);

        // Verify tab is expanded in spans
        for span in &lines[0].spans {
            assert!(!span.content.contains('\t'), "Tab should be expanded to spaces");
        }

        // Verify tab is expanded in row info content
        assert_eq!(row_infos[0].content, "    hi");
    }

    #[test]
    fn test_control_characters_sanitized() {
        let content_width = 40;
        let prefix_width = 6;

        // \x01 and \x7f are control chars with unicode-width None
        let content = "hello\x01world\x7f!";
        let content_spans = vec![Span::styled(content.to_string(), Style::default())];

        let (lines, row_infos) = wrap_content(
            content_spans,
            content,
            "  ".to_string(),
            "+ C ".to_string(),
            Style::default(),
            content_width,
            prefix_width,
        );

        assert_eq!(lines.len(), 1);

        // Control chars replaced with spaces in rendered spans
        let rendered: String = lines[0].spans.iter().map(|s| s.content.to_string()).collect();
        assert!(rendered.contains("hello world !"), "Control chars should become spaces, got: {:?}", rendered);

        // Same in row info
        assert_eq!(row_infos[0].content, "hello world !");
    }

    #[test]
    fn test_content_display_width_matches_sanitized_width() {
        let test_cases = [
            "hello world",
            "\t\tindented",
            "has\x01control\x7fchars",
            "mixed\t\x01both",
            "你好世界",
        ];

        for input in &test_cases {
            let width = content_display_width(input);
            let sanitized = sanitize_for_display(input)
                .unwrap_or_else(|| input.to_string());
            assert_eq!(
                width,
                sanitized.width(),
                "content_display_width and sanitized width disagree for {:?}",
                input
            );
        }
    }
}
