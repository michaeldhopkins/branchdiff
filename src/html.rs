use std::io::{self, Write};

use anyhow::Result;
use base64::Engine;
use ratatui::style::Color;

use branchdiff::diff::{DiffLine, LineSource};
use branchdiff::image_diff::{CachedImage, ImageCache};
use branchdiff::output::{OutputData, OutputFile};
use branchdiff::syntax;
use branchdiff::ui::spans::coalesce_spans;

fn html_escape(s: &str) -> String {
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

fn color_to_css(color: Color) -> Option<String> {
    match color {
        Color::Rgb(r, g, b) => Some(format!("rgb({r},{g},{b})")),
        _ => None,
    }
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
    img.display_image
        .write_to(&mut buf, image::ImageFormat::Png)
        .ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(buf.into_inner()))
}

pub fn render_html(data: &OutputData, images: &ImageCache) -> Result<()> {
    let mut stdout = io::stdout().lock();

    write_header(&mut stdout, data)?;

    let line_num_width = data
        .files
        .iter()
        .flat_map(|f| &f.lines)
        .filter_map(|l| l.line_number)
        .max()
        .map(|n| n.to_string().len())
        .unwrap_or(0);

    for (i, file) in data.files.iter().enumerate() {
        write_file(&mut stdout, i, file, images, line_num_width)?;
    }

    write_footer(&mut stdout)?;
    Ok(())
}

fn write_header(out: &mut impl Write, data: &OutputData) -> Result<()> {
    let branch_info = format!("{} | {} vs {}", data.repo_name, data.to_label, data.from_label);
    let file_count = data.files.len();

    write!(out, r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{branch_info} — branchdiff</title>
<style>
:root {{
  --bg:      #002b36;
  --bg-hl:   #073642;
  --fg:      #839496;
  --fg-em:   #93a1a1;
  --fg-sec:  #586e75;
  --border:  #073642;
}}
[data-theme="light"] {{
  --bg:      #fdf6e3;
  --bg-hl:   #eee8d5;
  --fg:      #657b83;
  --fg-em:   #586e75;
  --fg-sec:  #93a1a1;
  --border:  #eee8d5;
}}
* {{ margin: 0; padding: 0; box-sizing: border-box; }}
body {{
  background: var(--bg);
  color: var(--fg);
  font-family: 'SF Mono','Menlo','Consolas','Liberation Mono',monospace;
  font-size: 14px;
  line-height: 1.5;
  -webkit-text-size-adjust: 100%;
  padding: 16px;
}}
.header {{
  position: relative;
  color: #2aa198;
  padding: 12px 0;
  border-bottom: 1px solid var(--border);
  margin-bottom: 16px;
}}
.header h1 {{ font-size: 16px; font-weight: 600; }}
.header .stats {{ font-size: 13px; color: var(--fg-sec); margin-top: 4px; }}
.theme-toggle {{
  position: absolute; right: 0; top: 4px;
  background: none; border: 1px solid var(--border);
  border-radius: 6px; padding: 8px 14px;
  color: var(--fg-sec); cursor: pointer; font-size: 24px;
  line-height: 1; min-width: 44px; min-height: 44px;
}}
.theme-toggle:hover {{ color: var(--fg-em); }}
.toc {{
  margin-bottom: 20px;
  padding: 12px;
  background: var(--bg-hl);
  border-radius: 6px;
}}
.toc-title {{ font-size: 13px; color: var(--fg-sec); margin-bottom: 8px; }}
.toc a {{
  display: block;
  color: #2aa198;
  text-decoration: none;
  padding: 2px 0;
  font-size: 13px;
}}
.toc a:hover {{ text-decoration: underline; }}
.adds {{ color: #859900; }}
.dels {{ color: #dc322f; }}
details {{
  margin-bottom: 12px;
  border: 1px solid var(--border);
  border-radius: 6px;
  overflow: hidden;
}}
summary {{
  padding: 8px 12px;
  background: var(--bg-hl);
  cursor: pointer;
  font-weight: 600;
  color: var(--fg-em);
  font-size: 13px;
  user-select: none;
}}
summary:hover {{ background: var(--border); }}
summary .adds {{ font-weight: 400; }}
summary .dels {{ font-weight: 400; }}
table {{
  width: 100%;
  border-collapse: collapse;
  table-layout: fixed;
}}
td {{ vertical-align: top; white-space: pre-wrap; word-break: break-all; }}
.ln {{
  color: var(--fg-sec);
  user-select: none;
  text-align: right;
  padding: 0 8px 0 4px;
  width: 4em;
  min-width: 4em;
  white-space: nowrap;
}}
.gutter {{
  width: 1.5em;
  text-align: center;
  color: var(--fg-sec);
  user-select: none;
  padding: 0;
}}
.content {{
  padding: 0 8px;
  overflow-x: auto;
}}
.line-base {{ }}
.line-committed {{ background: rgba(42,161,152,0.08); }}
.line-staged {{ background: rgba(133,153,0,0.08); }}
.line-unstaged {{ background: rgba(181,137,0,0.08); }}
.line-del-base {{ background: rgba(220,50,47,0.08); }}
.line-del-committed {{ background: rgba(220,50,47,0.08); }}
.line-del-staged {{ background: rgba(220,50,47,0.08); }}
.line-canceled {{ background: rgba(211,54,130,0.08); }}
.line-moved {{ background: rgba(211,54,130,0.08); }}
.line-header td {{ padding: 8px 12px; color: var(--fg-em); font-weight: bold; }}
.line-elided td {{ color: var(--fg-sec); opacity: 0.7; padding: 2px 12px; font-style: italic; }}
.hl-committed {{ background: rgba(42,161,152,0.2); border-radius: 2px; }}
.hl-staged {{ background: rgba(133,153,0,0.2); border-radius: 2px; }}
.hl-unstaged {{ background: rgba(181,137,0,0.2); border-radius: 2px; }}
.hl-del-base {{ background: rgba(220,50,47,0.2); border-radius: 2px; }}
.hl-del-committed {{ background: rgba(220,50,47,0.2); border-radius: 2px; }}
.hl-del-staged {{ background: rgba(220,50,47,0.2); border-radius: 2px; }}
.hl-canceled {{ background: rgba(211,54,130,0.2); border-radius: 2px; }}
.del {{ text-decoration: line-through; opacity: 0.7; }}
.image-container {{
  display: flex;
  gap: 16px;
  padding: 12px;
  flex-wrap: wrap;
  justify-content: center;
}}
.image-panel {{ text-align: center; }}
.image-panel img {{
  max-width: 100%;
  border: 1px solid var(--border);
  border-radius: 4px;
}}
.image-panel .meta {{
  font-size: 12px;
  color: var(--fg-sec);
  margin-top: 4px;
}}
.image-label {{
  font-size: 12px;
  color: var(--fg-sec);
  margin-bottom: 4px;
  text-transform: uppercase;
  letter-spacing: 0.5px;
}}
.footer {{
  margin-top: 24px;
  padding-top: 12px;
  border-top: 1px solid var(--border);
  font-size: 12px;
  color: var(--fg-sec);
}}
.footer a {{ color: #2aa198; }}
@media (max-width: 768px) {{
  body {{ padding: 8px; font-size: 13px; }}
  .ln {{ width: 3em; min-width: 3em; font-size: 12px; }}
}}
</style>
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
    for span in display_spans {
        let class = match span.source {
            Some(source) => source_highlight_class(source),
            None => "",
        };
        let del_class = if span.is_deletion { " del" } else { "" };
        let escaped = html_escape(&span.text);
        if class.is_empty() && del_class.is_empty() {
            write!(out, "{escaped}")?;
        } else {
            write!(out, "<span class=\"{class}{del_class}\">{escaped}</span>")?;
        }
    }
    Ok(())
}

fn write_syntax_highlighted(out: &mut impl Write, content: &str, file_path: Option<&str>) -> Result<()> {
    let segments = syntax::highlight_line(content, file_path);
    if segments.is_empty() || (segments.len() == 1 && segments[0].fg_color == Color::Rgb(200, 200, 200)) {
        write!(out, "{}", html_escape(content))?;
        return Ok(());
    }
    for seg in &segments {
        let escaped = html_escape(&seg.text);
        if let Some(css_color) = color_to_css(seg.fg_color) {
            write!(out, "<span style=\"color:{css_color}\">{escaped}</span>")?;
        } else {
            write!(out, "{escaped}")?;
        }
    }
    Ok(())
}

fn write_image(out: &mut impl Write, line: &DiffLine, images: &ImageCache) -> Result<()> {
    let path = match &line.file_path {
        Some(p) => p.as_str(),
        None => return Ok(()),
    };

    let Some(state) = images.peek(path) else {
        write!(out, "<tr><td colspan=\"3\" class=\"content\">[image: {path}]</td></tr>",
            path = html_escape(path))?;
        return Ok(());
    };

    writeln!(out, "<tr><td colspan=\"3\"><div class=\"image-container\">")?;

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

fn write_file(out: &mut impl Write, file_index: usize, file: &OutputFile, images: &ImageCache, line_num_width: usize) -> Result<()> {
    let open = if file.collapsed { "" } else { " open" };
    write!(out, "<details{open} id=\"file-{file_index}\"><summary>{path}",
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

    for line in &file.lines {
        if line.source == LineSource::FileHeader {
            continue;
        }

        if line.is_image_marker() {
            write_image(out, line, images)?;
            continue;
        }

        let is_moved = line.move_target.is_some();
        let class = if is_moved { "line-moved" } else { source_line_class(line.source) };

        if line.source == LineSource::Elided {
            writeln!(out, "<tr class=\"{class}\"><td colspan=\"3\">┈┈ ⋮ {} ⋮ ┈┈</td></tr>",
                html_escape(&line.content))?;
            continue;
        }

        let ln = if let Some(num) = line.line_number {
            format!("{:>width$}", num, width = line_num_width)
        } else {
            " ".repeat(line_num_width)
        };

        let prefix = if is_moved { 'M' } else { line.prefix };
        write!(out, "<tr class=\"{class}\"><td class=\"ln\">{ln}</td><td class=\"gutter\">{prefix}</td><td class=\"content\">",
            class = class,
            ln = ln,
            prefix = prefix,
        )?;

        if !line.inline_spans.is_empty() {
            write_inline_spans(out, line)?;
        } else {
            write_syntax_highlighted(out, &line.content, file_path)?;
        }

        writeln!(out, "</td></tr>")?;
    }

    writeln!(out, "</table>")?;
    writeln!(out, "</details>")?;
    Ok(())
}

fn write_footer(out: &mut impl Write) -> Result<()> {
    writeln!(out, "<div class=\"footer\">Generated by <a href=\"https://github.com/michaeldhopkins/branchdiff\">branchdiff</a></div>")?;
    writeln!(out, r#"<script>
function applyTheme(){{
  const theme=localStorage.getItem('bd-theme')||'dark';
  document.documentElement.dataset.theme=theme;
  const btn=document.querySelector('.theme-toggle');
  if(btn)btn.textContent=theme==='light'?'☾':'☀';
}}
function bindToggle(){{
  document.querySelector('.theme-toggle')?.addEventListener('click',()=>{{
    const next=document.documentElement.dataset.theme==='light'?'dark':'light';
    localStorage.setItem('bd-theme',next);
    applyTheme();
  }});
}}
applyTheme();
bindToggle();
if(location.protocol!=='file:'){{let prev='';setInterval(()=>{{
  fetch(location.href,{{cache:'no-store'}}).then(r=>r.text()).then(h=>{{
    const d=new DOMParser().parseFromString(h,'text/html');
    const newBody=d.body.innerHTML;
    if(newBody===prev||!newBody)return;
    prev=newBody;
    const s=[window.scrollX,window.scrollY];
    document.body.innerHTML=newBody;
    window.scrollTo(...s);
    applyTheme();
    bindToggle();
  }}).catch(()=>{{}});
}},2000)}}
</script>"#)?;
    writeln!(out, "</body></html>")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use branchdiff::diff::InlineSpan;
    use branchdiff::image_diff::ImageCache;

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
        assert_eq!(
            html_escape("<a href=\"x&y\">"),
            "&lt;a href=&quot;x&amp;y&quot;&gt;"
        );
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
    fn test_write_inline_spans_output() {
        let line = DiffLine {
            source: LineSource::Committed,
            content: "hello world".into(),
            prefix: '+',
            line_number: Some(1),
            file_path: None,
            inline_spans: vec![
                InlineSpan { text: "hello".into(), source: None, is_deletion: false },
                InlineSpan { text: " world".into(), source: Some(LineSource::Committed), is_deletion: false },
            ],
            old_content: None,
            change_source: None,
            in_current_bookmark: None,
            block_idx: None,
            move_target: None,
        };
        let mut buf = Vec::new();
        write_inline_spans(&mut buf, &line).unwrap();
        let html = String::from_utf8(buf).unwrap();
        assert!(html.contains("hello"));
        assert!(html.contains("<span class=\"hl-committed\"> world</span>"));
    }

    #[test]
    fn test_write_inline_spans_with_deletion() {
        let line = DiffLine {
            source: LineSource::DeletedBase,
            content: "old text".into(),
            prefix: '-',
            line_number: Some(1),
            file_path: None,
            inline_spans: vec![
                InlineSpan { text: "old".into(), source: Some(LineSource::DeletedBase), is_deletion: true },
                InlineSpan { text: " text".into(), source: None, is_deletion: false },
            ],
            old_content: None,
            change_source: None,
            in_current_bookmark: None,
            block_idx: None,
            move_target: None,
        };
        let mut buf = Vec::new();
        write_inline_spans(&mut buf, &line).unwrap();
        let html = String::from_utf8(buf).unwrap();
        assert!(html.contains("class=\"hl-del-base del\""));
    }

    #[test]
    fn test_write_inline_spans_escapes_html() {
        let line = DiffLine {
            source: LineSource::Committed,
            content: "<script>".into(),
            prefix: '+',
            line_number: Some(1),
            file_path: None,
            inline_spans: vec![
                InlineSpan { text: "<script>".into(), source: Some(LineSource::Committed), is_deletion: false },
            ],
            old_content: None,
            change_source: None,
            in_current_bookmark: None,
            block_idx: None,
            move_target: None,
        };
        let mut buf = Vec::new();
        write_inline_spans(&mut buf, &line).unwrap();
        let html = String::from_utf8(buf).unwrap();
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
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
            }],
            total_additions: 1,
            total_deletions: 1,
        };
        let images = ImageCache::new();

        let mut buf = Vec::new();
        write_header(&mut buf, &data).unwrap();
        for (i, file) in data.files.iter().enumerate() {
            write_file(&mut buf, i, file, &images, 1).unwrap();
        }
        write_footer(&mut buf).unwrap();

        let html = String::from_utf8(buf).unwrap();

        assert!(html.contains("<!DOCTYPE html>"), "should have doctype");
        assert!(html.contains("feature vs main"), "should have branch info");
        assert!(html.contains("<details open"), "should have collapsible files");
        assert!(html.contains("test.rs"), "should have file name");
        assert!(html.contains("line-committed"), "should have committed class");
        assert!(html.contains("line-del-base"), "should have deletion class");
        assert!(html.contains("</html>"), "should close html");
        assert!(html.contains("+1"), "should show addition count");
        assert!(html.contains("-1"), "should show deletion count");
    }
}
