//! Entry points for the fuzz targets in `fuzz/`, compiled only under `cargo fuzz`
//! (which passes `--cfg fuzzing`) and under `cargo test`, which replays the
//! committed seeds. The parsers they wrap are crate-private; this module reaches
//! them without making them part of the published library API.

use std::collections::HashMap;

use crate::vcs::git::{changed_files, commands, parse};
use crate::vcs::jj;

/// Every parser branchdiff runs over git's or jj's stdout, with results discarded.
pub fn parse_all_vcs_output(text: &str) {
    let _ = commands::parse_git_version(text);
    for line in text.lines() {
        let _ = changed_files::parse_diff_line(line);
    }
    let _ = parse::parse_status_porcelain(text);
    let _ = parse::parse_numstat_binaries(text);

    let _ = jj::parse_rev_metadata(text);
    let _ = jj::parse_jj_summary(text);
    let _ = jj::parse_rename(text);
    let _ = jj::parse_binary_from_stat(text);
}

pub fn read_cat_file_batch(stream: &[u8], file_paths: &[&str]) -> HashMap<String, String> {
    parse::read_cat_file_batch(&mut &stream[..], file_paths)
}

/// Fuzz all review comment formats (JSONL, revdiff, reviewdog rdjson/rdjsonl, GitHub PR JSON),
/// JSONL roundtrip serialization, line anchoring, and HTML escaping.
pub fn fuzz_comments(data: &[u8]) {
    if let Ok(text) = std::str::from_utf8(data)
        && let Ok(comments) = crate::comments::parse_comments(text)
    {
        verify_parsed_comments(&comments);
    }
    let lossy = String::from_utf8_lossy(data);
    if let Ok(comments) = crate::comments::parse_comments(&lossy) {
        verify_parsed_comments(&comments);
    }
}

fn verify_parsed_comments(comments: &[crate::comments::ReviewComment]) {
    use crate::comments::anchor_comments;
    use crate::comments::render::write_html_comments;
    use crate::diff::{DiffLine, LineSource};

    let jsonl = crate::comments::to_jsonl(comments);
    let reparsed = crate::comments::parse_comments(&jsonl)
        .expect("to_jsonl output must always parse without error");
    assert_eq!(reparsed, comments, "to_jsonl roundtrip must preserve parsed comments");

    let lines = vec![
        DiffLine::file_header("src/lib.rs"),
        DiffLine::new(LineSource::DeletedBase, "old line".to_string(), '-', None)
            .with_file_path("src/lib.rs")
            .with_old_line_number(1),
        DiffLine::new(LineSource::Base, "ctx line".to_string(), ' ', Some(1))
            .with_file_path("src/lib.rs")
            .with_old_line_number(2),
        DiffLine::new(LineSource::Committed, "new line".to_string(), '+', Some(2))
            .with_file_path("src/lib.rs"),
    ];

    let anchored = anchor_comments(&lines, comments);
    let total_anchored: usize =
        (0..lines.len()).map(|i| anchored.for_line(i).map_or(0, <[_]>::len)).sum();
    assert_eq!(
        total_anchored,
        comments.len(),
        "every comment must be anchored to a diff line or file header, never dropped"
    );

    let mut html_buf = Vec::new();
    for idx in 0..lines.len() {
        write_html_comments(&mut html_buf, &anchored, idx, 1).expect("write html to vec");
    }
    let html_str = std::str::from_utf8(&html_buf).expect("HTML comments must be valid UTF-8");
    assert!(!html_str.contains("<script"), "HTML comments must escape raw <script tags");

    let mut printed = Vec::new();
    for idx in 0..lines.len() {
        crate::comments::render::write_print_comments(&mut printed, &anchored, idx, 1)
            .expect("write print to vec");
    }
    let printed = String::from_utf8(printed).expect("print comments must be valid UTF-8");
    // The renderer's own colours: cyan frame, bold, and the suggestion's red and green rows.
    let text = ["\x1b[36m", "\x1b[1m", "\x1b[31m", "\x1b[32m", "\x1b[0m"]
        .iter()
        .fold(printed, |text, code| text.replace(code, ""));
    assert!(
        text.chars().all(|c| !c.is_control() || c == '\n' || c == '\t'),
        "comment text must never reach the terminal with control characters"
    );

    let patch = crate::comments::patch::generate_patch_with_comments(&lines, &anchored);
    let without: String =
        patch.lines().filter(|l| !l.starts_with("# ")).map(|l| format!("{l}\n")).collect();
    assert_eq!(without, crate::patch::generate_patch(&lines), "comments must not change the patch");
    assert!(
        patch.chars().all(|c| !c.is_control() || c == '\n' || c == '\t'),
        "comment text must never put control characters in the patch"
    );
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// The committed seeds must pass the harness that `cargo fuzz` runs, so a
    /// change to what the renderers write cannot leave the fuzz target crashing
    /// on its own corpus with only the fuzz replay job to notice.
    #[test]
    fn every_committed_comments_seed_passes_the_harness() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus/comments");
        let mut seeds = 0;
        for entry in std::fs::read_dir(&dir).expect("read the comments corpus") {
            let path = entry.expect("corpus entry").path();
            if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("seed-")) {
                super::fuzz_comments(&std::fs::read(&path).expect("read seed"));
                seeds += 1;
            }
        }
        assert!(seeds >= 6, "found only {seeds} seeds in {}", dir.display());
    }
}
