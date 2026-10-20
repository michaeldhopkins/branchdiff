//! Writing text into the `--html` page: escaping, and joining neighbouring
//! pieces that share a style into one `<span>`.

use std::io::Write;

use ratatui::style::Color;

use crate::html_style::token_class;

/// `html_escape` straight into the page, copying the runs between special
/// characters rather than building a `String` per call.
pub(crate) fn write_escaped(out: &mut impl Write, s: &str) -> std::io::Result<()> {
    let mut start = 0;
    for (i, byte) in s.bytes().enumerate() {
        let entity = match byte {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            b'\'' => "&#39;",
            _ => continue,
        };
        out.write_all(&s.as_bytes()[start..i])?;
        out.write_all(entity.as_bytes())?;
        start = i + 1;
    }
    out.write_all(&s.as_bytes()[start..])
}

pub(crate) fn color_to_css(color: Color) -> Option<String> {
    match color {
        Color::Rgb(r, g, b) => Some(format!("rgb({r},{g},{b})")),
        _ => None,
    }
}

/// How a run of text is wrapped: not at all, or in one `<span>`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunStyle {
    Bare,
    /// A theme token class (`tk-keyword`).
    Class(&'static str),
    /// A colour the theme has no class for.
    Inline(Color),
    /// A word-level change highlight, and whether the text was deleted.
    Highlight(&'static str, bool),
}

impl RunStyle {
    pub(crate) fn of_token(color: Color) -> Self {
        if let Some(class) = token_class(color) {
            Self::Class(class)
        } else if color == Color::Rgb(200, 200, 200) || color_to_css(color).is_none() {
            Self::Bare
        } else {
            Self::Inline(color)
        }
    }

    pub(crate) fn of_highlight(class: &'static str, is_deletion: bool) -> Self {
        if class.is_empty() && !is_deletion {
            Self::Bare
        } else {
            Self::Highlight(class, is_deletion)
        }
    }

    /// Whether neighbours of this style join into one span. Highlight spans do not:
    /// each keeps its own box, so the seams between changed words stay visible.
    fn merges(self) -> bool {
        !matches!(self, Self::Highlight(..))
    }

    fn write_open(self, out: &mut impl Write) -> std::io::Result<()> {
        match self {
            Self::Bare => Ok(()),
            Self::Class(class) => write!(out, "<span class=\"{class}\">"),
            Self::Inline(color) => {
                write!(out, "<span style=\"color:{}\">", color_to_css(color).unwrap_or_default())
            }
            Self::Highlight(class, del) => {
                write!(out, "<span class=\"{class}{}\">", if del { " del" } else { "" })
            }
        }
    }
}

/// Write `items` as markup, joining neighbours that share a token or colour style
/// into one `<span>` so a long line is not a chain of identical wrappers.
pub(crate) fn write_runs<T>(
    out: &mut impl Write,
    items: &[T],
    style: impl Fn(&T) -> RunStyle,
    text: impl Fn(&T) -> &str,
) -> std::io::Result<()> {
    let mut start = 0;
    while start < items.len() {
        let run_style = style(&items[start]);
        run_style.write_open(out)?;
        let mut end = start;
        write_escaped(out, text(&items[end]))?;
        end += 1;
        while run_style.merges() && end < items.len() && style(&items[end]) == run_style {
            write_escaped(out, text(&items[end]))?;
            end += 1;
        }
        if run_style != RunStyle::Bare {
            out.write_all(b"</span>")?;
        }
        start = end;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN_STYLES: [RunStyle; 6] = [
        RunStyle::Bare,
        RunStyle::Class("tk-keyword"),
        RunStyle::Class("tk-string"),
        RunStyle::Inline(Color::Rgb(1, 2, 3)),
        RunStyle::Highlight("hl-committed", false),
        RunStyle::Highlight("hl-committed", true),
    ];

    fn unescape(s: &str) -> String {
        s.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&")
    }

    /// Each visible character with the open tag of the span around it.
    fn per_character_style(markup: &str) -> Vec<(String, char)> {
        let mut out = Vec::new();
        let mut style = String::new();
        let mut rest = markup;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix("</span>") {
                style.clear();
                rest = after;
            } else if rest.starts_with("<span") {
                let end = rest.find('>').unwrap() + 1;
                style = rest[..end].to_string();
                rest = &rest[end..];
            } else {
                let end = rest.find('<').unwrap_or(rest.len());
                out.extend(unescape(&rest[..end]).chars().map(|c| (style.clone(), c)));
                rest = &rest[end..];
            }
        }
        out
    }

    proptest::proptest! {
        #[test]
        fn merging_neighbouring_runs_changes_no_character_s_text_or_style(
            items in proptest::collection::vec(("[a-z<&\"']{0,4}", 0..RUN_STYLES.len()), 0..12),
        ) {
            let styled: Vec<(String, RunStyle)> =
                items.into_iter().map(|(t, i)| (t, RUN_STYLES[i])).collect();
            let mut merged = Vec::new();
            write_runs(&mut merged, &styled, |(_, s)| *s, |(t, _)| t).unwrap();
            let mut unmerged = Vec::new();
            for (text, style) in &styled {
                write_runs(&mut unmerged, &[(text.clone(), *style)], |(_, s)| *s, |(t, _)| t).unwrap();
            }
            let (merged, unmerged) = (String::from_utf8(merged).unwrap(), String::from_utf8(unmerged).unwrap());
            proptest::prop_assert_eq!(per_character_style(&merged), per_character_style(&unmerged));
            let mut runs = 0;
            let mut previous = None;
            for (_, style) in &styled {
                if *style != RunStyle::Bare && (!style.merges() || previous != Some(*style)) {
                    runs += 1;
                }
                previous = Some(*style);
            }
            proptest::prop_assert_eq!(merged.matches("<span").count(), runs, "{}", merged);
        }
    }

    #[test]
    fn neighbouring_highlight_spans_keep_their_own_boxes() {
        let style = RunStyle::Highlight("hl-committed", false);
        let items = ["ab", "cd"];
        let mut buf = Vec::new();
        write_runs(&mut buf, &items, |_| style, |s| s).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "<span class=\"hl-committed\">ab</span><span class=\"hl-committed\">cd</span>"
        );
    }

    #[test]
    fn a_line_of_same_coloured_tokens_is_one_span() {
        let segments = ["let", " x", " ="].map(|text| crate::syntax::SyntaxSegment {
            text: text.into(),
            fg_color: Color::Rgb(180, 140, 200),
        });
        let mut buf = Vec::new();
        write_runs(&mut buf, &segments, |s| RunStyle::of_token(s.fg_color), |s| &s.text).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "<span class=\"tk-keyword\">let x =</span>");
    }
}
