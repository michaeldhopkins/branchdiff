//! Review comments in `--diff` output.
//!
//! A unified patch has no place for comments inside a hunk, but `git apply`
//! and `patch` skip any text before a file's `diff --git` header. So each
//! file's comments are written as `#` lines just before that file's patch,
//! and the patch still applies.

use std::fmt::Write as _;
use std::ops::Range;

use crate::diff::{DiffLine, LineSource};
use crate::patch::generate_patch;

use super::AnchoredComments;
use super::render::body_rows;
use super::sanitize::printable;

/// The `--diff` patch for `lines`, with the comments anchored to each file
/// written before that file's patch. Without comments it is `generate_patch`.
pub fn generate_patch_with_comments(lines: &[DiffLine], comments: &AnchoredComments) -> String {
    if comments.is_empty() {
        return generate_patch(lines);
    }
    let mut out = String::new();
    for range in file_ranges(lines) {
        write_comment_block(&mut out, lines, comments, range.clone());
        out.push_str(&generate_patch(&lines[range]));
    }
    out
}

/// Index ranges of `lines`, one per file, each starting at its header.
fn file_ranges(lines: &[DiffLine]) -> Vec<Range<usize>> {
    let mut starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.source == LineSource::FileHeader)
        .map(|(idx, _)| idx)
        .collect();
    if starts.first() != Some(&0) {
        starts.insert(0, 0);
    }
    let ends = starts.iter().skip(1).copied().chain(std::iter::once(lines.len()));
    starts
        .iter()
        .copied()
        .zip(ends)
        .map(|(start, end)| start..end)
        .filter(|r| !r.is_empty())
        .collect()
}

fn write_comment_block(
    out: &mut String,
    lines: &[DiffLine],
    comments: &AnchoredComments,
    range: Range<usize>,
) {
    let header = &lines[range.start];
    let path = header.file_path.as_deref().unwrap_or(&header.content);
    for idx in range {
        let Some(list) = comments.for_line(idx) else { continue };
        for anchored in list {
            let _ =
                writeln!(out, "# 💬 {} · {}", printable(path), printable(&anchored.header_label()));
            for row in body_rows(anchored) {
                let _ = writeln!(out, "# │ {row}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comments::{CommentSide, ReviewComment, anchor_comments, ensure_commented_files};
    use crate::diff::{DiffInput, FileDiff, compute_four_way_diff};

    fn comment(path: &str, line: Option<usize>, side: CommentSide, body: &str) -> ReviewComment {
        ReviewComment {
            path: path.into(),
            start_line: line,
            end_line: line,
            side,
            body: body.into(),
            author: None,
            suggestion: None,
            outdated: false,
        }
    }

    fn two_file_lines(comments: &[ReviewComment]) -> Vec<DiffLine> {
        let mut files: Vec<FileDiff> = ["a.txt", "b.txt"]
            .iter()
            .map(|path| {
                compute_four_way_diff(DiffInput {
                    path,
                    base: Some("one\ntwo\nthree\n"),
                    head: Some("one\ntwo\nthree\n"),
                    index: Some("one\ntwo\nthree\n"),
                    working: Some("one\nTWO\nthree\n"),
                    old_path: None,
                })
            })
            .collect();
        ensure_commented_files(&mut files, comments);
        files.into_iter().flat_map(|f| f.lines).collect()
    }

    fn without_comment_lines(patch: &str) -> String {
        patch.lines().filter(|l| !l.starts_with("# ")).map(|l| format!("{l}\n")).collect()
    }

    #[test]
    fn comments_go_before_their_files_patch_and_leave_the_patch_unchanged() {
        let comments = vec![
            comment("b.txt", Some(2), CommentSide::New, "on b\nsecond line"),
            comment("a.txt", Some(2), CommentSide::Old, "on a"),
            comment("c.txt", Some(9), CommentSide::New, "untouched"),
        ];
        let lines = two_file_lines(&comments);
        let anchored = anchor_comments(&lines, &comments);
        let patch = generate_patch_with_comments(&lines, &anchored);

        assert_eq!(without_comment_lines(&patch), generate_patch(&lines), "{patch}");
        let a_comment = patch.find("# 💬 a.txt · old line 2").expect(&patch);
        let a_diff = patch.find("diff --git a/a.txt").expect(&patch);
        let b_comment = patch.find("# 💬 b.txt · new line 2").expect(&patch);
        let b_diff = patch.find("diff --git a/b.txt").expect(&patch);
        assert!(a_comment < a_diff && a_diff < b_comment && b_comment < b_diff, "{patch}");
        assert!(patch.contains("# │ on b\n# │ second line\n"), "{patch}");
        assert!(
            patch.contains("# 💬 c.txt · new line 9, outside diff\n# │ untouched\n"),
            "{patch}"
        );
    }

    #[test]
    fn without_comments_the_patch_is_the_plain_patch() {
        let lines = two_file_lines(&[]);
        let patch = generate_patch_with_comments(&lines, &AnchoredComments::default());
        assert_eq!(patch, generate_patch(&lines));
        assert!(!patch.is_empty());
    }

    #[test]
    fn control_characters_in_comments_never_reach_the_patch() {
        let comments = vec![comment(
            "a.txt\x1b[2J",
            None,
            CommentSide::New,
            "x\x1b]0;t\x07\rdiff --git a/z b/z",
        )];
        let lines = two_file_lines(&comments);
        let anchored = anchor_comments(&lines, &comments);
        let patch = generate_patch_with_comments(&lines, &anchored);
        assert!(!patch.contains('\x1b') && !patch.contains('\x07') && !patch.contains('\r'));
        assert!(patch.lines().all(|l| !l.starts_with("diff --git a/z")), "{patch}");
    }

    #[test]
    fn file_ranges_split_at_headers_and_keep_leading_lines() {
        let lines = vec![
            DiffLine::new(LineSource::Base, "orphan".into(), ' ', Some(1)),
            DiffLine::file_header("a"),
            DiffLine::new(LineSource::Base, "x".into(), ' ', Some(1)),
            DiffLine::file_header("b"),
        ];
        assert_eq!(file_ranges(&lines), vec![0..1, 1..3, 3..4]);
        assert!(file_ranges(&[]).is_empty());
    }
}
