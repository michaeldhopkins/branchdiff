mod theme;

use std::cell::RefCell;
use std::path::Path;
use std::sync::OnceLock;

use ratatui::style::Color;
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::{SyntaxReference, SyntaxSet};
use two_face::syntax as extra_syntax;

use crate::ui::colors::DEFAULT_FG;

/// A syntax-highlighted segment of text
#[derive(Debug, Clone)]
pub struct SyntaxSegment {
    pub text: String,
    pub fg_color: Color,
}

/// Global syntax highlighter with lazy initialization
pub struct SyntaxHighlighter {
    syntax_set: SyntaxSet,
    theme: Theme,
}

/// Thread-local state for multi-line syntax highlighting
struct HighlightState<'a> {
    file_path: String,
    highlighter: HighlightLines<'a>,
}

thread_local! {
    static HIGHLIGHT_STATE: RefCell<Option<HighlightState<'static>>> = const { RefCell::new(None) };
}

static HIGHLIGHTER: OnceLock<SyntaxHighlighter> = OnceLock::new();

impl SyntaxHighlighter {
    /// Get the global syntax highlighter instance
    pub fn global() -> &'static SyntaxHighlighter {
        HIGHLIGHTER.get_or_init(|| {
            // Use two-face's extended syntax set (includes Swift, TypeScript, Kotlin, etc.)
            let syntax_set = extra_syntax::extra_newlines();
            let theme = theme::branchdiff_theme();
            SyntaxHighlighter { syntax_set, theme }
        })
    }

    /// Get syntax definition for a file path based on extension
    fn syntax_for_path(&self, path: &str) -> &SyntaxReference {
        Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .and_then(|ext| self.syntax_set.find_syntax_by_extension(ext))
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text())
    }

    /// Highlight a single line of code, maintaining state for multi-line constructs.
    /// Call `reset_highlight_state` when switching files or when line order is non-sequential.
    pub fn highlight_line(&'static self, content: &str, file_path: Option<&str>) -> Vec<SyntaxSegment> {
        // Handle empty content
        if content.is_empty() {
            return vec![];
        }

        let path = file_path.unwrap_or("");
        let syntax = if path.is_empty() {
            self.syntax_set.find_syntax_plain_text()
        } else {
            self.syntax_for_path(path)
        };

        HIGHLIGHT_STATE.with(|state| {
            let mut state = state.borrow_mut();

            // Check if we need to create or reset the highlighter
            let needs_reset = match &*state {
                Some(s) => s.file_path != path,
                None => true,
            };

            if needs_reset {
                *state = Some(HighlightState {
                    file_path: path.to_string(),
                    highlighter: HighlightLines::new(syntax, &self.theme),
                });
            }

            let hl_state = state.as_mut().unwrap();

            // Append newline so syntect properly terminates line comments (e.g., // in JS)
            // Without this, line comments bleed into subsequent lines.
            let content_with_newline = format!("{}\n", content);

            match hl_state
                .highlighter
                .highlight_line(&content_with_newline, &self.syntax_set)
            {
                Ok(ranges) => ranges
                    .into_iter()
                    .filter_map(|(style, text)| {
                        // Strip the trailing newline we added
                        let text = text.strip_suffix('\n').unwrap_or(text);
                        if text.is_empty() {
                            None
                        } else {
                            Some(SyntaxSegment {
                                text: text.to_string(),
                                fg_color: Color::Rgb(
                                    style.foreground.r,
                                    style.foreground.g,
                                    style.foreground.b,
                                ),
                            })
                        }
                    })
                    .collect(),
                Err(_) => {
                    // Fallback to plain text on error
                    vec![SyntaxSegment {
                        text: content.to_string(),
                        fg_color: DEFAULT_FG,
                    }]
                }
            }
        })
    }
}

/// Reset the highlight state - call when switching files or after non-sequential rendering
pub fn reset_highlight_state() {
    HIGHLIGHT_STATE.with(|state| {
        *state.borrow_mut() = None;
    });
}

/// Convenience function to highlight a line
pub fn highlight_line(content: &str, file_path: Option<&str>) -> Vec<SyntaxSegment> {
    SyntaxHighlighter::global().highlight_line(content, file_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_highlight_rust_keyword() {
        reset_highlight_state();
        let segments = highlight_line("fn main() {}", Some("test.rs"));
        assert!(!segments.is_empty());
        // Should have "fn" as keyword with purple-ish color
        let fn_segment = segments.iter().find(|s| s.text == "fn");
        assert!(fn_segment.is_some());
    }

    #[test]
    fn test_highlight_rust_string() {
        reset_highlight_state();
        let segments = highlight_line("let s = \"hello\";", Some("test.rs"));
        assert!(!segments.is_empty());
        // Should have the string segment
        let has_string = segments.iter().any(|s| s.text.contains("hello"));
        assert!(has_string);
    }

    #[test]
    fn test_highlight_unknown_extension() {
        reset_highlight_state();
        let segments = highlight_line("some random text", Some("file.xyz123"));
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "some random text");
    }

    #[test]
    fn test_highlight_no_file_path() {
        reset_highlight_state();
        let segments = highlight_line("fn main() {}", None);
        // Should still work, just without syntax highlighting
        assert!(!segments.is_empty());
    }

    #[test]
    fn test_highlight_python() {
        reset_highlight_state();
        let segments = highlight_line("def hello():", Some("test.py"));
        assert!(!segments.is_empty());
        let def_segment = segments.iter().find(|s| s.text == "def");
        assert!(def_segment.is_some());
    }

    #[test]
    fn test_highlight_javascript() {
        reset_highlight_state();
        let segments = highlight_line("const x = 42;", Some("test.js"));
        assert!(!segments.is_empty());
        let const_segment = segments.iter().find(|s| s.text == "const");
        assert!(const_segment.is_some());
    }

    #[test]
    fn test_highlight_empty_content() {
        reset_highlight_state();
        let segments = highlight_line("", Some("test.rs"));
        assert!(segments.is_empty());
    }

    #[test]
    fn test_multiline_string_state_preserved() {
        reset_highlight_state();
        // Start a multi-line string
        let _line1 = highlight_line("let s = \"start of", Some("test.rs"));
        // Continue on next line - should still be in string context
        let line2 = highlight_line("middle of string", Some("test.rs"));
        // End the string
        let line3 = highlight_line("end of string\";", Some("test.rs"));

        // Line 2 should be treated as string content (peach color 220, 180, 140)
        // The entire line should be string-colored since we're inside quotes
        assert!(!line2.is_empty());
        // Check that line2 has string-like coloring (not default gray)
        let has_string_color = line2.iter().any(|s| {
            matches!(s.fg_color, Color::Rgb(r, g, b) if r > 200 && g > 150 && b < 180)
        });
        assert!(has_string_color, "Line 2 should have string highlighting color");

        // Line 3 should end the string
        assert!(!line3.is_empty());
    }

    #[test]
    fn test_file_change_resets_state() {
        reset_highlight_state();
        // Highlight some Rust
        highlight_line("fn main() {}", Some("test.rs"));
        // Switch to Python - should reset state
        let py_segments = highlight_line("def hello():", Some("test.py"));
        // Should have Python keyword coloring
        let def_segment = py_segments.iter().find(|s| s.text == "def");
        assert!(def_segment.is_some());
    }

    #[test]
    fn test_highlight_swift() {
        reset_highlight_state();
        let segments = highlight_line("func hello() -> String {", Some("test.swift"));
        assert!(!segments.is_empty());
        // Should have "func" as keyword
        let func_segment = segments.iter().find(|s| s.text == "func");
        assert!(func_segment.is_some(), "Swift 'func' keyword should be highlighted");
    }

    #[test]
    fn test_highlight_typescript() {
        reset_highlight_state();
        let segments = highlight_line("const x: number = 42;", Some("test.ts"));
        assert!(!segments.is_empty());
        // Should have "const" as keyword
        let const_segment = segments.iter().find(|s| s.text == "const");
        assert!(const_segment.is_some(), "TypeScript 'const' keyword should be highlighted");
    }

    #[test]
    fn test_js_line_comment_does_not_bleed() {
        reset_highlight_state();
        // Simulate rendering lines from a JS file
        // The highlight_line function now appends \n internally to fix this
        let _line1 = highlight_line("// This is a comment", Some("test.js"));
        let line2 = highlight_line("const BOT_PATTERNS = [", Some("test.js"));

        // Line 2 should have "const" as a keyword, NOT as comment text
        let const_segment = line2.iter().find(|s| s.text == "const");
        assert!(
            const_segment.is_some(),
            "JS 'const' after line comment should be highlighted as keyword, got: {:?}",
            line2
        );
    }
}
