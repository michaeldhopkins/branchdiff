#![no_main]

//! STRUCTURAL-INVARIANT + ROUNDTRIP target: the four-way diff and `--diff` patch output.
//!
//! File contents are input branchdiff does not control: any bytes, CRLF, tabs,
//! huge or empty lines. `compute_four_way_diff` interleaves four versions of a file
//! (merge-base, HEAD, index, working tree) into one list of lines, and
//! `generate_patch` turns that list into the unified diff `branchdiff --diff`
//! prints for `git apply`.
//!
//! The invariant: **applying the patch to the base yields the working tree.** Every
//! base line must be accounted for exactly once (kept as context, or deleted), and
//! every working line exactly once (context, or added), in order; hunk headers must
//! state the counts their bodies hold. This catches a diff that drops or duplicates
//! a line, and a patch that misrepresents one, which a never-panics target cannot.
//!
//! Input: the four versions separated by NUL bytes, `base\0head\0index\0working`,
//! read lossily as UTF-8 the way branchdiff reads files. A missing trailing part
//! repeats the one before it, so `base\0working` is a plain two-way diff. A text
//! format rather than an `arbitrary` struct so that seeds are writable by hand and
//! byte mutation, which copies and tweaks spans, naturally yields layers that
//! share lines, which is what exercises the provenance and modification maps.
//!
//! The frame, and what it hides:
//!
//! - HEAD, index and working tree are always present. `None` at those layers means
//!   "absent", and a file deleted at one layer and present at another is a separate
//!   code path (`check_file_deletion`) not exercised here.
//! - Comparison is modulo trailing whitespace, and on `str::lines()`, because that
//!   is how branchdiff reads a file: it trims each line's end for display, and
//!   `lines()` drops a final newline and the `\r` of a `\r\n`. A patch cannot
//!   express those differences today (see AGENTS.md "Fuzzing").

use libfuzzer_sys::fuzz_target;

use branchdiff::diff::{compute_four_way_diff, DiffInput};
use branchdiff::patch::generate_patch;

fn trimmed_lines(s: &str) -> Vec<&str> {
    s.lines().map(str::trim_end).collect()
}

/// `@@ -a[,b] +c[,d] @@` → (a, b, c, d); a missing count is 1.
fn parse_hunk_header(line: &str) -> (usize, usize, usize, usize) {
    let inner = line
        .strip_prefix("@@ -")
        .and_then(|s| s.strip_suffix(" @@"))
        .unwrap_or_else(|| panic!("malformed hunk header {line:?}"));
    let (old, new) = inner.split_once(" +").expect("hunk header has no +range");
    let range = |r: &str| -> (usize, usize) {
        match r.split_once(',') {
            Some((s, c)) => (s.parse().expect("start"), c.parse().expect("count")),
            None => (r.parse().expect("start"), 1),
        }
    };
    let (a, b) = range(old);
    let (c, d) = range(new);
    (a, b, c, d)
}

/// Apply a single-file unified patch to `base`, asserting context and deletions
/// match and hunk counts are honest.
fn apply(patch: &str, base: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut pos = 0usize; // next unconsumed base line
    let mut lines = patch.split('\n').peekable();

    while let Some(line) = lines.next() {
        if !line.starts_with("@@ ") {
            continue; // diff --git / --- / +++ / trailing empty
        }
        let (old_start, old_count, new_start, new_count) = parse_hunk_header(line);
        // A zero-count range names the line *before* the hunk.
        let hunk_at = if old_count == 0 {
            old_start
        } else {
            old_start.checked_sub(1).expect("a non-empty hunk range starts at line 0")
        };
        assert!(hunk_at >= pos, "hunk at old line {old_start} overlaps the previous hunk");
        assert!(hunk_at <= base.len(), "hunk starts past the end of the base");
        out.extend(base[pos..hunk_at].iter().map(|s| s.to_string()));
        pos = hunk_at;
        // The new range names the same place in the output that the old range
        // names in the base: the line after everything emitted so far.
        let expected_new_start = if new_count == 0 { out.len() } else { out.len() + 1 };
        assert_eq!(new_start, expected_new_start, "hunk header new start is wrong");

        let (mut seen_old, mut seen_new) = (0, 0);
        while let Some(body) = lines.peek() {
            let Some(prefix) = body.chars().next() else { break };
            if !matches!(prefix, ' ' | '-' | '+') {
                break;
            }
            let body = lines.next().expect("peeked");
            let text = &body[1..];
            match prefix {
                ' ' | '-' => {
                    assert!(pos < base.len(), "hunk reads past the end of the base");
                    assert_eq!(base[pos], text, "context/deletion does not match base line {}", pos + 1);
                    pos += 1;
                    seen_old += 1;
                    if prefix == ' ' {
                        out.push(text.to_string());
                        seen_new += 1;
                    }
                }
                _ => {
                    out.push(text.to_string());
                    seen_new += 1;
                }
            }
        }
        assert_eq!((seen_old, seen_new), (old_count, new_count), "hunk header counts disagree with its body");
    }
    out.extend(base[pos..].iter().map(|s| s.to_string()));
    out
}

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let mut parts = text.splitn(4, '\0');
    let base = parts.next().unwrap_or("");
    let head = parts.next().unwrap_or(base);
    let index = parts.next().unwrap_or(head);
    let working = parts.next().unwrap_or(index);

    let diff = compute_four_way_diff(DiffInput {
        path: "f.rs",
        base: Some(base),
        head: Some(head),
        index: Some(index),
        working: Some(working),
        old_path: None,
    });
    let patch = generate_patch(&diff.lines);

    let base_lines = trimmed_lines(base);
    let applied = apply(&patch, &base_lines);
    assert_eq!(applied, trimmed_lines(working), "patch applied to base does not give the working tree\n{patch}");
});
