//! A deterministic `--html` input shared by `benches/performance.rs` and
//! `benches/alloc.rs`: Rust-like sources of 400 lines each, diffed in process
//! (no VCS), with the changed-line counts the HTML page is usually opened for.
//!
//! The sources are generated rather than read from `src/` so that the pins do not
//! move whenever the crate's own code does.

use std::path::PathBuf;

use branchdiff::app::{App, ViewMode};
use branchdiff::diff::{DiffInput, compute_four_way_diff};
use branchdiff::output::{OutputData, prepare};
use branchdiff::vcs::{ComparisonContext, RefreshResult, VcsBackend};

const LINES_PER_FILE: usize = 400;

/// Changed-line sizes benched, with the number of files the change spans.
pub const SIZES: [(usize, usize, &str); 3] =
    [(50, 4, "50_lines"), (200, 8, "200_lines"), (500, 8, "500_lines")];

fn source_line(i: usize) -> String {
    match i % 10 {
        0 => format!("/// Explains how item {i} is derived from its \"inputs\"."),
        1 => format!("pub fn function_{i}(value: u32, name: &str) -> Option<String> {{"),
        2 => format!("    let total_{i} = value * {i} + 7;"),
        3 => format!("    if name.starts_with(\"prefix_{i}\") && total_{i} > 0x{i:x} {{"),
        4 => format!("        return Some(format!(\"{{}}-{{}}\", name, total_{i}));"),
        5 => "    }".to_string(),
        6 => format!("    // falls through when step {i} has nothing to add"),
        7 => "    None".to_string(),
        8 => "}".to_string(),
        _ => String::new(),
    }
}

/// One file's base text and its edited text: `changed / 2` edited lines and
/// the rest added, spread evenly through the file.
fn file_pair(seed: usize, changed: usize) -> (String, String) {
    let edits = changed / 2;
    let adds = changed - edits;
    let step = (LINES_PER_FILE / (edits + 1)).max(3);
    let (mut base, mut working) = (String::new(), String::new());
    let (mut edited, mut added) = (0, 0);
    for n in 1..=LINES_PER_FILE {
        let line = source_line(seed * LINES_PER_FILE + n);
        base.push_str(&line);
        base.push('\n');
        if n % step == 0 && edited < edits && line.len() > 4 {
            working.push_str(&format!("{line} // tweak {n}\n"));
            edited += 1;
        } else {
            working.push_str(&line);
            working.push('\n');
        }
        if n % step == 1 && added < adds && n > 1 {
            for k in 0..4 {
                if added == adds {
                    break;
                }
                working.push_str(&format!("    let added_{n}_{k} = compute({k}, \"value\");\n"));
                added += 1;
            }
        }
    }
    (base, working)
}

/// The prepared page for a change of `changed` lines across `files` files,
/// exactly what `--html` hands the renderer.
pub fn output_data(changed: usize, files: usize) -> OutputData {
    let mut result = RefreshResult::empty();
    for f in 0..files {
        let (base, working) = file_pair(f, changed / files);
        let path = format!("src/module_{f}.rs");
        let diff = compute_four_way_diff(DiffInput {
            path: &path,
            base: Some(&base),
            head: Some(&working),
            index: Some(&working),
            working: Some(&working),
            old_path: None,
        });
        result.lines.extend(diff.lines.iter().cloned());
        result.files.push(diff);
    }
    let comparison = ComparisonContext {
        from_label: "main".to_string(),
        to_label: "feature".to_string(),
        stack_position: None,
        vcs_backend: VcsBackend::Git,
        bookmark_name: None,
        divergence: None,
    };
    let mut app = App::new(PathBuf::from("/bench"), comparison, result);
    app.view.view_mode = ViewMode::Context;
    prepare(&mut app)
}
