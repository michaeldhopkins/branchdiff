//! How a comment names its location: "new line 3" (words) or "line 3 (+)" (signs).
//!
//! Words say which file version a number belongs to, so "old line 2" is not
//! mistaken for the second line on screen. The choice is [`DEFAULT_LABEL_STYLE`];
//! the signs form stays for comparison and is selected with the unadvertised
//! `BRANCHDIFF_COMMENT_LABELS=signs` environment variable.

use super::{CommentSide, ReviewComment};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelStyle {
    /// `new line 3`, `old lines 7-8`, `new line 500, outside diff`.
    Words,
    /// `line 3 (+)`, `lines 7-8 (-)`, `line 500 (+, outside diff)`.
    Signs,
}

/// The one place to change how locations read.
pub const DEFAULT_LABEL_STYLE: LabelStyle = LabelStyle::Words;

const STYLE_ENV: &str = "BRANCHDIFF_COMMENT_LABELS";

pub(super) fn style_from_env(value: Option<&str>) -> LabelStyle {
    match value {
        Some("signs") => LabelStyle::Signs,
        Some("words") => LabelStyle::Words,
        _ => DEFAULT_LABEL_STYLE,
    }
}

pub fn label_style() -> LabelStyle {
    static STYLE: std::sync::OnceLock<LabelStyle> = std::sync::OnceLock::new();
    *STYLE.get_or_init(|| style_from_env(std::env::var(STYLE_ENV).ok().as_deref()))
}

/// The location of `comment`, in `style`. File-level comments read the same in both.
pub fn location_label(comment: &ReviewComment, outdated: bool, style: LabelStyle) -> String {
    let (lo, hi) = match (comment.start_line, comment.end_line) {
        (Some(s), Some(e)) => (s.min(e), s.max(e)),
        (Some(s), None) | (None, Some(s)) => (s, s),
        (None, None) => return "file-level".to_string(),
    };
    let range = if lo == hi { format!("line {lo}") } else { format!("lines {lo}-{hi}") };
    match style {
        LabelStyle::Words => {
            let side = match comment.side {
                CommentSide::New => "new",
                CommentSide::Old => "old",
            };
            let outside = if outdated { ", outside diff" } else { "" };
            format!("{side} {range}{outside}")
        }
        LabelStyle::Signs => {
            let sign = match comment.side {
                CommentSide::New => "+",
                CommentSide::Old => "-",
            };
            let outside = if outdated { ", outside diff" } else { "" };
            format!("{range} ({sign}{outside})")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(side: CommentSide, start: Option<usize>, end: Option<usize>) -> ReviewComment {
        ReviewComment {
            path: "a.rs".into(),
            start_line: start,
            end_line: end,
            side,
            body: String::new(),
            author: None,
            suggestion: None,
        }
    }

    #[test]
    fn words_name_the_side_in_front_of_the_range() {
        let label = |c: &ReviewComment, outdated| location_label(c, outdated, LabelStyle::Words);
        assert_eq!(label(&comment(CommentSide::Old, Some(2), Some(2)), false), "old line 2");
        assert_eq!(label(&comment(CommentSide::New, Some(3), Some(5)), false), "new lines 3-5");
        assert_eq!(label(&comment(CommentSide::Old, Some(8), Some(7)), false), "old lines 7-8");
        assert_eq!(
            label(&comment(CommentSide::New, Some(500), None), true),
            "new line 500, outside diff"
        );
        assert_eq!(label(&comment(CommentSide::New, None, None), false), "file-level");
    }

    #[test]
    fn signs_keep_the_earlier_form() {
        let label = |c: &ReviewComment, outdated| location_label(c, outdated, LabelStyle::Signs);
        assert_eq!(label(&comment(CommentSide::Old, Some(2), Some(2)), false), "line 2 (-)");
        assert_eq!(label(&comment(CommentSide::New, Some(3), Some(5)), false), "lines 3-5 (+)");
        assert_eq!(
            label(&comment(CommentSide::New, Some(500), Some(500)), true),
            "line 500 (+, outside diff)"
        );
        assert_eq!(label(&comment(CommentSide::Old, None, None), true), "file-level");
    }

    #[test]
    fn the_environment_picks_the_style_and_anything_else_is_the_default() {
        assert_eq!(style_from_env(Some("signs")), LabelStyle::Signs);
        assert_eq!(style_from_env(Some("words")), LabelStyle::Words);
        assert_eq!(style_from_env(Some("nonsense")), DEFAULT_LABEL_STYLE);
        assert_eq!(style_from_env(None), DEFAULT_LABEL_STYLE);
        assert_eq!(DEFAULT_LABEL_STYLE, LabelStyle::Words);
    }
}
