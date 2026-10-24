use std::io::Write;

use anyhow::Result;
use base64::Engine;
use ratatui::style::Color;
use rayon::prelude::*;

use crate::comments::render::write_html_comments;
use crate::diff::{DiffLine, LineSource};
use crate::image_diff::{CachedImage, ImageCache};
use crate::output::{OutputData, OutputFile};
use crate::syntax;
use crate::ui::gutter::{Gutter, two_column_numbers};
use crate::ui::spans::coalesce_spans;

use crate::html_page::{HEAD_THEME_SCRIPT, Refresh, cached_script, noscript_style};
use crate::html_runs::{RunStyle, write_escaped, write_runs};
use crate::html_style::cached_stylesheet;
#[cfg(test)]
use crate::{html_runs::color_to_css, html_style::token_class};

/// A rendered row's usual size, so a file's section is allocated once rather
/// than grown by doubling (pages measured at 280-300 bytes a row).
const SECTION_BYTES_PER_LINE: usize = 320;
/// Room for most of a page, so a typical one leaves in few writes.
const OUTPUT_BUFFER_BYTES: usize = 64 * 1024;

pub(crate) fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn source_line_class(source: LineSource) -> &'static str {
    match source {
        LineSource::Base => "line-base",
        LineSource::Committed => "line-committed",
        LineSource::Staged => "line-staged",
        LineSource::Unstaged => "line-unstaged",
        LineSource::DeletedBase => "line-del-base",
        LineSource::DeletedCommitted => "line-del-committed",
        LineSource::DeletedStaged => "line-del-staged",
        LineSource::CanceledCommitted | LineSource::CanceledStaged => "line-canceled",
        LineSource::FileHeader => "line-header",
        LineSource::Elided => "line-elided",
    }
}

fn source_highlight_class(source: LineSource) -> &'static str {
    match source {
        LineSource::Committed => "hl-committed",
        LineSource::Staged => "hl-staged",
        LineSource::Unstaged => "hl-unstaged",
        LineSource::DeletedBase => "hl-del-base",
        LineSource::DeletedCommitted => "hl-del-committed",
        LineSource::DeletedStaged => "hl-del-staged",
        LineSource::CanceledCommitted | LineSource::CanceledStaged => "hl-canceled",
        _ => "",
    }
}

fn encode_image_to_base64(img: &CachedImage) -> Option<String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.display_image.write_to(&mut buf, image::ImageFormat::Png).ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(buf.into_inner()))
}

/// Write the `--html` page for `data` to stdout.
pub fn print_html(data: &OutputData, images: &ImageCache, refresh: Refresh) -> Result<()> {
    render_html_buffered(std::io::stdout().lock(), data, images, refresh)
}

/// Write the page to `out` through a buffer, flushed before returning.
///
/// Stdout is line-buffered, which turns every newline of the header and the
/// footer into a write call.
pub fn render_html_buffered(
    out: impl Write,
    data: &OutputData,
    images: &ImageCache,
    refresh: Refresh,
) -> Result<()> {
    let mut out = std::io::BufWriter::with_capacity(OUTPUT_BUFFER_BYTES, out);
    render_html(&mut out, data, images, refresh)?;
    out.flush()?;
    Ok(())
}

/// Write the `--html` page for `data` to `out`.
pub fn render_html(
    out: &mut impl Write,
    data: &OutputData,
    images: &ImageCache,
    refresh: Refresh,
) -> Result<()> {
    render_html_in(crate::vcs::vcs_thread_pool(), out, data, images, refresh)
}

/// `render_html` on a pool of the caller's choosing. The allocation bench renders
/// on a one-thread pool, so its count does not depend on how files are scheduled.
#[doc(hidden)]
pub fn render_html_in(
    pool: &rayon::ThreadPool,
    out: &mut impl Write,
    data: &OutputData,
    images: &ImageCache,
    refresh: Refresh,
) -> Result<()> {
    write_header(out, data)?;

    let gutter = Gutter::measure(data.files.iter().flat_map(|f| &f.lines), None);

    // Highlighting is nearly all of a page's cost, and every file is
    // highlighted from a clean parser state (the highlighter resets whenever the
    // path changes), so files render independently and are written in order.
    let sections: Vec<Result<Vec<u8>>> = pool.install(|| {
        data.files
            .par_iter()
            .enumerate()
            .map(|(i, file)| {
                // `syntax::highlight_line` carries parser state across calls in a
                // thread-local; a pool thread may hold whatever it last rendered.
                syntax::reset_highlight_state();
                let mut section = Vec::with_capacity(SECTION_BYTES_PER_LINE * file.lines.len());
                write_file(&mut section, i, file, images, gutter)?;
                Ok(section)
            })
            .collect()
    });
    for section in sections {
        out.write_all(&section?)?;
    }

    write_footer(out, refresh)?;
    Ok(())
}

fn write_header(out: &mut impl Write, data: &OutputData) -> Result<()> {
    let branch_info = format!("{} | {} vs {}", data.repo_name, data.to_label, data.from_label);
    let file_count = data.files.len();

    write!(
        out,
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{branch_info} — branchdiff</title>
<style>
{style}
</style>
{head_theme}
{noscript}
</head>
<body>
<div class="header">
  <h1>{branch_info}</h1>
  <div class="stats">{file_count} file{file_s} · <span class="adds">+{adds}</span> <span class="dels">-{dels}</span></div>
  <button class="theme-toggle" aria-label="Toggle light/dark theme">☀</button>
</div>
"#,
        branch_info = html_escape(&branch_info),
        file_count = file_count,
        file_s = if file_count == 1 { "" } else { "s" },
        adds = data.total_additions,
        dels = data.total_deletions,
        style = cached_stylesheet(),
        noscript = noscript_style(),
        head_theme = HEAD_THEME_SCRIPT,
    )?;

    // Table of contents
    writeln!(out, "<div class=\"toc\">")?;
    writeln!(out, "<div class=\"toc-title\">Files</div>")?;
    for (i, file) in data.files.iter().enumerate() {
        write!(out, "<a href=\"#file-{i}\">{path}", i = i, path = html_escape(&file.path))?;
        if file.additions > 0 {
            write!(out, " <span class=\"adds\">+{}</span>", file.additions)?;
        }
        if file.deletions > 0 {
            write!(out, " <span class=\"dels\">-{}</span>", file.deletions)?;
        }
        writeln!(out, "</a>")?;
    }
    writeln!(out, "</div>")?;

    Ok(())
}

fn write_inline_spans(out: &mut impl Write, line: &DiffLine) -> Result<()> {
    let display_spans = coalesce_spans(&line.inline_spans);
    write_runs(
        out,
        &display_spans,
        |span| {
            RunStyle::of_highlight(span.source.map_or("", source_highlight_class), span.is_deletion)
        },
        |span| &span.text,
    )?;
    Ok(())
}

fn write_syntax_highlighted(
    out: &mut impl Write,
    content: &str,
    file_path: Option<&str>,
) -> Result<()> {
    let segments = syntax::highlight_line(content, file_path);
    if segments.is_empty()
        || (segments.len() == 1 && segments[0].fg_color == Color::Rgb(200, 200, 200))
    {
        write_escaped(out, content)?;
        return Ok(());
    }
    write_runs(out, &segments, |seg| RunStyle::of_token(seg.fg_color), |seg| &seg.text)?;
    Ok(())
}

/// The string form a single token run must stay byte-identical to.
#[cfg(test)]
fn token_span(text: &str, color: Color) -> String {
    let escaped = html_escape(text);
    if let Some(class) = token_class(color) {
        format!("<span class=\"{class}\">{escaped}</span>")
    } else if color == Color::Rgb(200, 200, 200) {
        escaped
    } else if let Some(css_color) = color_to_css(color) {
        format!("<span style=\"color:{css_color}\">{escaped}</span>")
    } else {
        escaped
    }
}

fn write_image(
    out: &mut impl Write,
    line: &DiffLine,
    images: &ImageCache,
    columns: usize,
) -> Result<()> {
    let path = match &line.file_path {
        Some(p) => p.as_str(),
        None => return Ok(()),
    };

    let Some(state) = images.peek(path) else {
        write!(
            out,
            "<tr><td colspan=\"{columns}\" class=\"content\">[image: {path}]</td></tr>",
            path = html_escape(path)
        )?;
        return Ok(());
    };

    writeln!(out, "<tr><td colspan=\"{columns}\"><div class=\"image-container\">")?;

    if let Some(ref before) = state.before {
        write_image_panel(out, before, "before")?;
    }
    if let Some(ref after) = state.after {
        write_image_panel(out, after, "after")?;
    }

    writeln!(out, "</div></td></tr>")?;
    Ok(())
}

fn write_image_panel(out: &mut impl Write, img: &CachedImage, label: &str) -> Result<()> {
    writeln!(out, "<div class=\"image-panel\">")?;
    writeln!(out, "<div class=\"image-label\">{label}</div>")?;
    if let Some(b64) = encode_image_to_base64(img) {
        writeln!(out, "<img src=\"data:image/png;base64,{b64}\" alt=\"{label}\">")?;
    }
    writeln!(out, "<div class=\"meta\">{}</div>", html_escape(&img.metadata_string()))?;
    writeln!(out, "</div>")?;
    Ok(())
}

fn write_file(
    out: &mut impl Write,
    file_index: usize,
    file: &OutputFile,
    images: &ImageCache,
    gutter: Gutter,
) -> Result<()> {
    let ln_cells = gutter.ln_cells();
    let columns = ln_cells + 2;
    let open = if file.collapsed { "" } else { " open" };
    write!(
        out,
        "<details{open} id=\"file-{file_index}\" data-path=\"{path}\"><summary>{path}",
        open = open,
        file_index = file_index,
        path = html_escape(&file.path),
    )?;
    if file.additions > 0 {
        write!(out, " <span class=\"adds\">+{}</span>", file.additions)?;
    }
    if file.deletions > 0 {
        write!(out, " <span class=\"dels\">-{}</span>", file.deletions)?;
    }
    writeln!(out, "</summary>")?;
    writeln!(out, "<table>")?;

    let file_path = Some(file.path.as_str());

    for (line_idx, line) in file.lines.iter().enumerate() {
        if line.source == LineSource::FileHeader {
            write_html_comments(out, &file.comments, line_idx, ln_cells)?;
            continue;
        }

        if line.is_image_marker() {
            write_image(out, line, images, columns)?;
            write_html_comments(out, &file.comments, line_idx, ln_cells)?;
            continue;
        }

        let is_moved = line.move_target.is_some();
        let class = if is_moved { "line-moved" } else { source_line_class(line.source) };

        if line.source == LineSource::Elided {
            writeln!(
                out,
                "<tr class=\"{class}\"><td colspan=\"{columns}\">┈┈ ⋮ {} ⋮ ┈┈</td></tr>",
                html_escape(&line.content)
            )?;
            continue;
        }

        write!(out, "<tr class=\"{class}\">")?;
        write_html_line_numbers(out, line, gutter)?;
        let prefix = if is_moved { 'M' } else { line.prefix };
        write!(out, "<td class=\"gutter\">{prefix}</td><td class=\"content\">")?;

        if !line.inline_spans.is_empty() {
            write_inline_spans(out, line)?;
        } else {
            write_syntax_highlighted(out, &line.content, file_path)?;
        }

        writeln!(out, "</td></tr>")?;
        write_html_comments(out, &file.comments, line_idx, ln_cells)?;
    }

    writeln!(out, "</table>")?;
    writeln!(out, "</details>")?;
    Ok(())
}

/// The number cells of a row: old and new in two columns, else the one column.
fn write_html_line_numbers(
    out: &mut impl Write,
    line: &DiffLine,
    gutter: Gutter,
) -> std::io::Result<()> {
    if gutter.is_two_column() {
        let (old, new) = two_column_numbers(line);
        for (class, number) in [("ln ln-old", old), ("ln ln-new", new)] {
            write!(out, "<td class=\"{class}\">")?;
            if let Some(n) = number {
                write!(out, "{n}")?;
            }
            write!(out, "</td>")?;
        }
        return Ok(());
    }
    write!(out, "<td class=\"ln\">")?;
    match line.line_number {
        Some(num) => write!(out, "{num:>width$}", width = gutter.print_width())?,
        None => write!(out, "{:width$}", "", width = gutter.print_width())?,
    }
    write!(out, "</td>")
}

fn write_footer(out: &mut impl Write, refresh: Refresh) -> Result<()> {
    writeln!(
        out,
        "<div class=\"footer\">Generated by <a href=\"https://github.com/michaeldhopkins/branchdiff\">branchdiff</a></div>"
    )?;
    writeln!(out, "{}", cached_script(refresh))?;
    writeln!(out, "</body></html>")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::InlineSpan;
    use crate::html_page::Refresh;
    use crate::image_diff::ImageCache;

    #[test]
    fn test_html_escape_special_chars() {
        assert_eq!(html_escape("a & b"), "a &amp; b");
        assert_eq!(html_escape("<div>"), "&lt;div&gt;");
        assert_eq!(html_escape("x=\"y\""), "x=&quot;y&quot;");
        assert_eq!(html_escape("it's"), "it&#39;s");
        assert_eq!(html_escape("plain text"), "plain text");
        assert_eq!(html_escape(""), "");
    }

    #[test]
    fn test_html_escape_multiple_special() {
        assert_eq!(html_escape("<a href=\"x&y\">"), "&lt;a href=&quot;x&amp;y&quot;&gt;");
    }

    fn escaped_by_writer(s: &str) -> String {
        let mut buf = Vec::new();
        write_escaped(&mut buf, s).unwrap();
        String::from_utf8(buf).unwrap()
    }

    proptest::proptest! {
        #[test]
        fn escaping_into_a_writer_matches_html_escape(s in "(?s)[&<>\"'a-z é你\\t\\n]{0,40}|.{0,40}") {
            proptest::prop_assert_eq!(escaped_by_writer(&s), html_escape(&s));
        }

        #[test]
        fn a_token_span_written_out_matches_the_string_form(
            s in "[&<>\"'a-z ]{0,12}",
            r in 0u8..=255, g in 0u8..=255, b in 0u8..=255,
        ) {
            for color in [Color::Rgb(r, g, b), Color::Rgb(200, 200, 200), Color::Reset] {
                let mut buf = Vec::new();
                write_runs(&mut buf, &[s.as_str()], |_| RunStyle::of_token(color), |t| t).unwrap();
                proptest::prop_assert_eq!(String::from_utf8(buf).unwrap(), token_span(&s, color));
            }
        }
    }

    #[test]
    fn test_source_line_class_all_variants() {
        assert_eq!(source_line_class(LineSource::Base), "line-base");
        assert_eq!(source_line_class(LineSource::Committed), "line-committed");
        assert_eq!(source_line_class(LineSource::Staged), "line-staged");
        assert_eq!(source_line_class(LineSource::Unstaged), "line-unstaged");
        assert_eq!(source_line_class(LineSource::DeletedBase), "line-del-base");
        assert_eq!(source_line_class(LineSource::DeletedCommitted), "line-del-committed");
        assert_eq!(source_line_class(LineSource::DeletedStaged), "line-del-staged");
        assert_eq!(source_line_class(LineSource::CanceledCommitted), "line-canceled");
        assert_eq!(source_line_class(LineSource::CanceledStaged), "line-canceled");
        assert_eq!(source_line_class(LineSource::FileHeader), "line-header");
        assert_eq!(source_line_class(LineSource::Elided), "line-elided");
    }

    #[test]
    fn test_source_highlight_class_all_variants() {
        assert_eq!(source_highlight_class(LineSource::Committed), "hl-committed");
        assert_eq!(source_highlight_class(LineSource::Staged), "hl-staged");
        assert_eq!(source_highlight_class(LineSource::Unstaged), "hl-unstaged");
        assert_eq!(source_highlight_class(LineSource::DeletedBase), "hl-del-base");
        assert_eq!(source_highlight_class(LineSource::DeletedCommitted), "hl-del-committed");
        assert_eq!(source_highlight_class(LineSource::DeletedStaged), "hl-del-staged");
        assert_eq!(source_highlight_class(LineSource::CanceledCommitted), "hl-canceled");
        assert_eq!(source_highlight_class(LineSource::CanceledStaged), "hl-canceled");
        assert_eq!(source_highlight_class(LineSource::Base), "");
        assert_eq!(source_highlight_class(LineSource::FileHeader), "");
    }

    #[test]
    fn test_color_to_css() {
        assert_eq!(color_to_css(Color::Rgb(255, 0, 128)), Some("rgb(255,0,128)".to_string()));
        assert_eq!(color_to_css(Color::Reset), None);
    }

    #[test]
    fn a_token_span_takes_its_class_and_an_unknown_colour_keeps_its_inline_style() {
        assert_eq!(
            token_span("fn", Color::Rgb(180, 140, 200)),
            "<span class=\"tk-keyword\">fn</span>"
        );
        assert_eq!(
            token_span("x", Color::Rgb(255, 0, 128)),
            "<span style=\"color:rgb(255,0,128)\">x</span>"
        );
        assert_eq!(token_span("plain", Color::Rgb(200, 200, 200)), "plain");
        assert_eq!(token_span("reset", Color::Reset), "reset");
    }

    #[test]
    fn a_rendered_rust_keyword_carries_a_token_class_not_an_inline_colour() {
        syntax::reset_highlight_state();
        let mut buf = Vec::new();
        write_syntax_highlighted(&mut buf, "fn main() {}", Some("test.rs")).unwrap();
        let html = String::from_utf8(buf).unwrap();
        assert!(html.contains("class=\"tk-keyword\""), "{html}");
        assert!(!html.contains("color:rgb(180,140,200)"), "{html}");
    }

    #[test]
    fn test_write_inline_spans_output() {
        let mut line = DiffLine::new(LineSource::Committed, "hello world".into(), '+', Some(1));
        line.inline_spans = vec![
            InlineSpan { text: "hello".into(), source: None, is_deletion: false },
            InlineSpan {
                text: " world".into(),
                source: Some(LineSource::Committed),
                is_deletion: false,
            },
        ];
        let mut buf = Vec::new();
        write_inline_spans(&mut buf, &line).unwrap();
        let html = String::from_utf8(buf).unwrap();
        assert!(html.contains("hello"));
        assert!(html.contains("<span class=\"hl-committed\"> world</span>"));
    }

    #[test]
    fn test_write_inline_spans_with_deletion() {
        let mut line = DiffLine::new(LineSource::DeletedBase, "old text".into(), '-', Some(1));
        line.inline_spans = vec![
            InlineSpan {
                text: "old".into(),
                source: Some(LineSource::DeletedBase),
                is_deletion: true,
            },
            InlineSpan { text: " text".into(), source: None, is_deletion: false },
        ];
        let mut buf = Vec::new();
        write_inline_spans(&mut buf, &line).unwrap();
        let html = String::from_utf8(buf).unwrap();
        assert!(html.contains("class=\"hl-del-base del\""));
    }

    #[test]
    fn test_write_inline_spans_escapes_html() {
        let mut line = DiffLine::new(LineSource::Committed, "<script>".into(), '+', Some(1));
        line.inline_spans = vec![InlineSpan {
            text: "<script>".into(),
            source: Some(LineSource::Committed),
            is_deletion: false,
        }];
        let mut buf = Vec::new();
        write_inline_spans(&mut buf, &line).unwrap();
        let html = String::from_utf8(buf).unwrap();
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
    }

    fn page(refresh: Refresh) -> String {
        let data = OutputData {
            repo_name: "r".into(),
            to_label: "a".into(),
            from_label: "b".into(),
            files: vec![],
            total_additions: 0,
            total_deletions: 0,
        };
        let mut buf = Vec::new();
        write_header(&mut buf, &data).unwrap();
        write_footer(&mut buf, refresh).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn a_live_page_polls_and_a_static_page_does_not() {
        assert!(page(Refresh::Live).contains("setInterval"));
        let off = page(Refresh::Off);
        assert!(!off.contains("setInterval") && !off.contains("fetch("));
        assert!(off.contains("bd-theme"), "the toggle script stays");
    }

    #[test]
    fn the_stored_theme_is_applied_in_the_head_before_the_body_paints() {
        for refresh in [Refresh::Live, Refresh::Off] {
            let html = page(refresh);
            let head_end = html.find("</head>").expect("head closes");
            let head = &html[..head_end];
            let apply = head.find("dataset.theme=").expect("the head script sets the theme");
            assert!(head[apply..].contains("localStorage.getItem('bd-theme')"), "{refresh:?}");
            assert!(apply < html.find("<body>").unwrap());
        }
    }

    #[test]
    fn every_page_carries_the_script_less_fallback_and_the_light_palette_once_in_the_rule() {
        for refresh in [Refresh::Live, Refresh::Off] {
            let html = page(refresh);
            assert!(html.contains("<noscript><style>"));
            assert!(html.contains("@media (prefers-color-scheme: light)"));
            assert_eq!(html.matches("[data-theme=\"light\"] {").count(), 1);
            assert_eq!(
                html.matches("#1a7f37").count(),
                2,
                "the rule and the fallback share one palette"
            );
            assert!(
                !html.contains("brightness(") && !html.contains("saturate("),
                "a theme is a palette, not a filter"
            );
        }
    }
    #[test]
    fn test_render_html_produces_valid_structure() {
        let data = OutputData {
            repo_name: "test".to_string(),
            to_label: "feature".to_string(),
            from_label: "main".to_string(),
            files: vec![OutputFile {
                path: "test.rs".to_string(),
                lines: vec![
                    DiffLine::file_header("test.rs"),
                    DiffLine::new(LineSource::Base, "unchanged".into(), ' ', Some(1)),
                    DiffLine::new(LineSource::Committed, "added".into(), '+', Some(2)),
                    DiffLine::new(LineSource::DeletedBase, "removed".into(), '-', Some(3)),
                ],
                additions: 1,
                deletions: 1,
                collapsed: false,
                comments: Default::default(),
            }],
            total_additions: 1,
            total_deletions: 1,
        };
        let images = ImageCache::new();

        let mut buf = Vec::new();
        render_html(&mut buf, &data, &images, Refresh::Live).unwrap();

        let html = String::from_utf8(buf).unwrap();

        assert!(html.starts_with("<!DOCTYPE html>"), "should start with the doctype");
        assert!(html.ends_with("</body></html>\n"), "the whole page reaches the writer");
        assert!(html.contains("feature vs main"), "should have branch info");
        assert!(html.contains("<details open"), "should have collapsible files");
        assert!(html.contains("test.rs"), "should have file name");
        assert!(html.contains("line-committed"), "should have committed class");
        assert!(html.contains("line-del-base"), "should have deletion class");
        assert!(html.contains("</html>"), "should close html");
        assert!(html.contains("+1"), "should show addition count");
        assert!(html.contains("-1"), "should show deletion count");
    }

    #[test]
    fn each_section_names_its_file_by_path_beside_its_position_id() {
        let mut buf = Vec::new();
        write_file(
            &mut buf,
            7,
            &rust_file("a\"b&c.rs", &["x"]),
            &ImageCache::new(),
            Gutter::default(),
        )
        .unwrap();
        let html = String::from_utf8(buf).unwrap();
        assert!(
            html.contains("<details open id=\"file-7\" data-path=\"a&quot;b&amp;c.rs\">"),
            "{html}"
        );
    }

    fn edited_file() -> OutputFile {
        let lines = vec![
            DiffLine::file_header("a.rs"),
            DiffLine::new(LineSource::Base, "same".into(), ' ', Some(9)).with_old_line_number(8),
            DiffLine::new(LineSource::DeletedBase, "gone".into(), '-', None)
                .with_old_line_number(9),
            DiffLine::new(LineSource::Committed, "fresh".into(), '+', Some(10)),
        ];
        OutputFile {
            path: "a.rs".into(),
            lines,
            additions: 1,
            deletions: 1,
            collapsed: false,
            comments: Default::default(),
        }
    }

    #[test]
    fn old_and_new_numbers_get_a_column_each() {
        let file = edited_file();
        let mut buf = Vec::new();
        let gutter = Gutter::measure(&file.lines, None);
        write_file(&mut buf, 0, &file, &ImageCache::new(), gutter).unwrap();
        let html = String::from_utf8(buf).unwrap();
        let cells = |text: &str| {
            let at = html.find(&format!("<td class=\"content\">{text}")).unwrap();
            let row = &html[html[..at].rfind("<tr").unwrap()..at];
            row.replace("<td class=\"gutter\">", "|")
        };
        assert!(
            cells("same").contains("<td class=\"ln ln-old\">8</td><td class=\"ln ln-new\">9</td>"),
            "{html}"
        );
        assert!(
            cells("gone").contains("<td class=\"ln ln-old\">9</td><td class=\"ln ln-new\"></td>"),
            "{html}"
        );
        assert!(
            cells("fresh").contains("<td class=\"ln ln-old\"></td><td class=\"ln ln-new\">10</td>"),
            "{html}"
        );
    }

    #[test]
    fn a_diff_without_old_numbers_keeps_one_number_cell() {
        let file = rust_file("a.rs", &["x"]);
        let mut buf = Vec::new();
        let gutter = Gutter::measure(&file.lines, None);
        write_file(&mut buf, 0, &file, &ImageCache::new(), gutter).unwrap();
        let html = String::from_utf8(buf).unwrap();
        assert!(html.contains("<td class=\"ln\">1</td><td class=\"gutter\">+</td>"), "{html}");
        assert!(!html.contains("ln-old"), "{html}");
    }

    #[test]
    fn an_image_not_yet_loaded_spans_every_column_as_a_placeholder() {
        let file = OutputFile {
            path: "logo.png".into(),
            lines: vec![DiffLine::file_header("logo.png"), DiffLine::image_marker("logo.png")],
            additions: 0,
            deletions: 0,
            collapsed: false,
            comments: Default::default(),
        };
        let mut buf = Vec::new();
        let gutter = Gutter::measure(&file.lines, None);
        let columns = gutter.ln_cells() + 2;
        write_file(&mut buf, 0, &file, &ImageCache::new(), gutter).unwrap();
        let html = String::from_utf8(buf).unwrap();
        let placeholder =
            format!("<tr><td colspan=\"{columns}\" class=\"content\">[image: logo.png]</td></tr>");
        assert!(html.contains(&placeholder), "{html}");
    }

    fn rust_file(path: &str, body: &[&str]) -> OutputFile {
        let mut lines = vec![DiffLine::file_header(path)];
        for (n, text) in body.iter().enumerate() {
            lines.push(DiffLine::new(LineSource::Committed, (*text).into(), '+', Some(n + 1)));
        }
        OutputFile {
            path: path.into(),
            lines,
            additions: body.len(),
            deletions: 0,
            collapsed: false,
            comments: Default::default(),
        }
    }

    #[test]
    fn html_comments_escape_tags_and_scripts_in_body_and_author() {
        use crate::comments::{CommentSide, ReviewComment, anchor_comments};

        let mut file = rust_file("src/xss.rs", &["let ok = true;"]);
        file.lines[1].file_path = Some("src/xss.rs".into());
        file.comments = anchor_comments(
            &file.lines,
            &[ReviewComment {
                path: "src/xss.rs".into(),
                start_line: Some(1),
                end_line: Some(1),
                side: CommentSide::New,
                body: "<script>alert(\"xss\")</script> & <img onerror='boom'>".into(),
                author: Some("evil<script>".into()),
                suggestion: None,
                outdated: false,
            }],
        );
        let data = OutputData {
            repo_name: "r".into(),
            to_label: "a".into(),
            from_label: "b".into(),
            files: vec![file],
            total_additions: 1,
            total_deletions: 0,
        };
        let mut buf = Vec::new();
        render_html(&mut buf, &data, &ImageCache::new(), Refresh::Off).unwrap();
        let html = String::from_utf8(buf).unwrap();

        assert!(html.contains("&lt;script&gt;alert(&quot;xss&quot;)&lt;/script&gt;"), "{html}");
        assert!(html.contains("&amp; &lt;img onerror=&#39;boom&#39;&gt;"), "{html}");
        assert!(html.contains("@evil&lt;script&gt;"), "{html}");
        assert!(!html.contains("<script>alert("), "{html}");
        assert!(!html.contains("<img onerror="), "{html}");
    }

    /// The page as one serial walk over the files wrote it, with a single reset
    /// before the first file.
    fn serial_page(data: &OutputData) -> String {
        let images = ImageCache::new();
        let mut buf = Vec::new();
        write_header(&mut buf, data).unwrap();
        syntax::reset_highlight_state();
        for (i, file) in data.files.iter().enumerate() {
            write_file(
                &mut buf,
                i,
                file,
                &images,
                Gutter::measure(data.files.iter().flat_map(|f| &f.lines), None),
            )
            .unwrap();
        }
        write_footer(&mut buf, Refresh::Off).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn files_rendered_in_parallel_match_one_serial_walk_byte_for_byte() {
        // Each file opens a block comment it never closes, so parser state left
        // over from an earlier render would colour the file as comment.
        let files: Vec<OutputFile> = (0..12)
            .map(|i| {
                let path = format!("src/f{i}.rs");
                rust_file(&path, &["fn a() { let x = \"s\"; }", "/* open", "still comment"])
            })
            .collect();
        let data = OutputData {
            repo_name: "r".into(),
            to_label: "a".into(),
            from_label: "b".into(),
            files,
            total_additions: 36,
            total_deletions: 0,
        };
        let expected = serial_page(&data);
        // Every render leaves pool threads mid-comment in the file they did last,
        // so later renders of the same page see that state unless it is reset.
        let mut page = String::new();
        for _ in 0..8 {
            let mut buf = Vec::new();
            render_html(&mut buf, &data, &ImageCache::new(), Refresh::Off).unwrap();
            page = String::from_utf8(buf).unwrap();
            assert_eq!(page, expected);
        }
        assert_eq!(page.matches("<span class=\"tk-keyword\">fn</span>").count(), 12);
    }

    /// Counts the write calls it receives, as stdout would turn them into
    /// system calls.
    struct CountingWriter {
        bytes: Vec<u8>,
        calls: usize,
    }

    impl Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_buffered_page_is_the_same_bytes_in_far_fewer_writes() {
        let data = OutputData {
            repo_name: "r".into(),
            to_label: "a".into(),
            from_label: "b".into(),
            files: vec![rust_file("src/a.rs", &["fn a() {}", "let x = 1;"])],
            total_additions: 2,
            total_deletions: 0,
        };
        let images = ImageCache::new();
        let mut unbuffered = CountingWriter { bytes: Vec::new(), calls: 0 };
        render_html(&mut unbuffered, &data, &images, Refresh::Off).unwrap();
        let mut buffered = CountingWriter { bytes: Vec::new(), calls: 0 };
        render_html_buffered(&mut buffered, &data, &images, Refresh::Off).unwrap();

        assert_eq!(buffered.bytes, unbuffered.bytes);
        assert!(!buffered.bytes.is_empty());
        assert!(
            buffered.calls < unbuffered.calls / 4,
            "{} vs {}",
            buffered.calls,
            unbuffered.calls
        );
    }
}
