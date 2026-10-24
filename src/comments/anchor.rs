use std::collections::HashMap;

use crate::diff::{DiffLine, FileDiff, LineSource};

use super::suggestion::has_suggestion;
use super::{AnchoredComment, AnchoredComments, CommentSide, ReviewComment};

const DIFF_CONTEXT_LINES: usize = 5;

pub fn normalize_comment_path(raw: &str) -> String {
    let replaced = raw.replace('\\', "/");
    let mut s = replaced.trim();
    loop {
        if let Some(rest) = s.strip_prefix("./") {
            s = rest.trim_start();
        } else if let Some(rest) = s.strip_prefix('/') {
            s = rest.trim_start();
        } else {
            break;
        }
    }
    s.to_string()
}

pub fn paths_match(diff_path: &str, comment_path: &str) -> bool {
    let norm_diff = normalize_comment_path(diff_path);
    let norm_comment = normalize_comment_path(comment_path);
    if norm_diff == norm_comment {
        return true;
    }
    if let Some(stripped) =
        norm_comment.strip_prefix("a/").or_else(|| norm_comment.strip_prefix("b/"))
        && stripped == norm_diff
    {
        return true;
    }
    if let Some(stripped) = norm_diff.strip_prefix("a/").or_else(|| norm_diff.strip_prefix("b/"))
        && stripped == norm_comment
    {
        return true;
    }
    false
}

pub fn file_diff_matches_path(file: &FileDiff, comment_path: &str) -> bool {
    if let Some(ref old_path) = file.old_path
        && paths_match(old_path, comment_path)
    {
        return true;
    }
    if let Some(header) = file.lines.first() {
        if let Some(ref fp) = header.file_path
            && paths_match(fp, comment_path)
        {
            return true;
        }
        if let Some((old_p, new_p)) = header.content.split_once(" → ")
            && (paths_match(old_p, comment_path) || paths_match(new_p, comment_path))
        {
            return true;
        }
        if paths_match(&header.content, comment_path) {
            return true;
        }
    }
    false
}

/// Ensure every file referenced by `comments` has a `FileDiff` entry in `files`.
/// If a commented file is not part of the current diff, a header-only `FileDiff`
/// is appended so its comments can be shown at the file level.
pub fn ensure_commented_files(files: &mut Vec<FileDiff>, comments: &[ReviewComment]) {
    for comment in comments {
        if comment.path.is_empty() {
            continue;
        }
        if !files.iter().any(|f| file_diff_matches_path(f, &comment.path)) {
            // The header shows the path, which came from the comment file; the
            // raw path stays in `file_path` so later comments still match it.
            let shown = super::sanitize::printable(&comment.path).into_owned();
            let mut header = DiffLine::new(LineSource::FileHeader, shown, ' ', None);
            header.file_path = Some(comment.path.clone());
            files.push(FileDiff::new(vec![header]));
        }
    }
}

struct FileSpan {
    header_idx: usize,
    path: String,
    old_path: Option<String>,
    line_indices: Vec<usize>,
    in_diff: Vec<bool>,
}

impl FileSpan {
    fn matches(&self, comment_path: &str) -> bool {
        if paths_match(&self.path, comment_path) {
            return true;
        }
        if let Some(ref old) = self.old_path
            && paths_match(old, comment_path)
        {
            return true;
        }
        false
    }
}

fn collect_file_spans(lines: &[DiffLine]) -> Vec<FileSpan> {
    let mut spans: Vec<FileSpan> = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        if line.source == LineSource::FileHeader {
            let path = line.file_path.clone().unwrap_or_else(|| line.content.clone());
            let old_path = line.content.split_once(" → ").map(|(old_p, _)| old_p.to_string());
            spans.push(FileSpan {
                header_idx: idx,
                path,
                old_path,
                line_indices: Vec::new(),
                in_diff: Vec::new(),
            });
        } else if let Some(current) = spans.last_mut() {
            current.line_indices.push(idx);
        } else {
            let path = line.file_path.clone().unwrap_or_default();
            spans.push(FileSpan {
                header_idx: 0,
                path,
                old_path: None,
                line_indices: vec![idx],
                in_diff: Vec::new(),
            });
        }
    }

    for span in &mut spans {
        let len = span.line_indices.len();
        let mut in_diff = vec![false; len];
        for (pos, &line_idx) in span.line_indices.iter().enumerate() {
            let line = &lines[line_idx];
            if line.source.is_change()
                || line.old_content.is_some()
                || !line.inline_spans.is_empty()
            {
                let start = pos.saturating_sub(DIFF_CONTEXT_LINES);
                let end = (pos + DIFF_CONTEXT_LINES + 1).min(len);
                for flag in &mut in_diff[start..end] {
                    *flag = true;
                }
            }
        }
        span.in_diff = in_diff;
    }

    spans
}

fn effective_old_line(line: &DiffLine) -> Option<usize> {
    line.old_line().or_else(|| if line.source.is_deletion() { line.line_number } else { None })
}

/// The text of the lines `comment` covers on its side, for showing what a
/// suggestion replaces. Empty unless the comment carries a suggestion.
fn replaced_lines(lines: &[DiffLine], span: &FileSpan, comment: &ReviewComment) -> Vec<String> {
    if !has_suggestion(&comment.body, comment.suggestion.as_deref()) {
        return Vec::new();
    }
    let Some(last) = comment.end_line.or(comment.start_line) else {
        return Vec::new();
    };
    let first = comment.start_line.unwrap_or(last).min(last);
    let last = last.max(first);
    span.line_indices
        .iter()
        .map(|&i| &lines[i])
        .filter(|l| !matches!(l.source, LineSource::Elided | LineSource::FileHeader))
        .filter(|l| match comment.side {
            CommentSide::New => {
                !l.source.is_deletion()
                    && l.line_number.is_some_and(|n| (first..=last).contains(&n))
            }
            CommentSide::Old => effective_old_line(l).is_some_and(|n| (first..=last).contains(&n)),
        })
        .map(|l| match comment.side {
            CommentSide::Old => l.old_content.clone().unwrap_or_else(|| l.content.clone()),
            CommentSide::New => l.content.clone(),
        })
        .collect()
}

fn find_anchor_in_span(
    lines: &[DiffLine],
    span: &FileSpan,
    comment: &ReviewComment,
) -> Option<usize> {
    let target_end = comment.end_line.or(comment.start_line)?;
    let target_start = comment.start_line.unwrap_or(target_end).min(target_end);
    let target_end = target_end.max(target_start);

    let matches_target = |line: &DiffLine, target: usize| -> bool {
        if line.source == LineSource::Elided || line.source == LineSource::FileHeader {
            return false;
        }
        match comment.side {
            CommentSide::New => !line.source.is_deletion() && line.line_number == Some(target),
            CommentSide::Old => effective_old_line(line) == Some(target),
        }
    };

    let matches_range = |line: &DiffLine| -> bool {
        if line.source == LineSource::Elided || line.source == LineSource::FileHeader {
            return false;
        }
        let num_opt = match comment.side {
            CommentSide::New => {
                if line.source.is_deletion() {
                    None
                } else {
                    line.line_number
                }
            }
            CommentSide::Old => effective_old_line(line),
        };
        num_opt.is_some_and(|n| (target_start..=target_end).contains(&n))
    };

    // Prefer exact match on target_end within the diff context
    for (pos, &line_idx) in span.line_indices.iter().enumerate() {
        if span.in_diff[pos] && matches_target(&lines[line_idx], target_end) {
            return Some(line_idx);
        }
    }

    // For a line range, anchor to the last line of the range that is within the diff
    // context. For a single line this finds nothing the loop above missed.
    for (pos, &line_idx) in span.line_indices.iter().enumerate().rev() {
        if span.in_diff[pos] && matches_range(&lines[line_idx]) {
            return Some(line_idx);
        }
    }

    None
}

/// Anchor `comments` to indices in `lines`.
///
/// - File-level comments (`start_line == None && end_line == None`) attach to the
///   file's `FileHeader` line with `outdated = false`.
/// - Line or line-range comments whose target is within the current diff attach
///   to that `DiffLine` with `outdated = false`.
/// - Comments whose target line (or file) is not in the current diff attach to
///   the file's `FileHeader` line (or line 0 if the file is absent) with
///   `outdated = true`, preserving their original line number and never dropping
///   a comment silently.
pub fn anchor_comments(lines: &[DiffLine], comments: &[ReviewComment]) -> AnchoredComments {
    if comments.is_empty() {
        return AnchoredComments::default();
    }

    let spans = collect_file_spans(lines);
    let mut by_line: HashMap<usize, Vec<AnchoredComment>> = HashMap::new();

    for comment in comments {
        let matched_span = spans.iter().find(|s| s.matches(&comment.path));
        let is_file_level = comment.start_line.is_none() && comment.end_line.is_none();

        let (line_idx, outdated, replaced) = match matched_span {
            Some(span) if is_file_level => (span.header_idx, false, Vec::new()),
            Some(span) if comment.outdated => (span.header_idx, true, Vec::new()),
            Some(span) => match find_anchor_in_span(lines, span, comment) {
                Some(idx) => (idx, false, replaced_lines(lines, span, comment)),
                None => (span.header_idx, true, Vec::new()),
            },
            None => (0, true, Vec::new()),
        };
        by_line
            .entry(line_idx)
            .or_default()
            .push(AnchoredComment::new(comment, outdated, replaced));
    }

    AnchoredComments { by_line }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::InlineSpan;

    fn numbered(n: usize, source: LineSource) -> DiffLine {
        let prefix = if source.is_change() { '+' } else { ' ' };
        DiffLine::new(source, format!("line {n}"), prefix, Some(n)).with_file_path("f.rs")
    }

    /// Header at index 0, then one line per entry at index 1.. in order.
    fn file(entries: Vec<DiffLine>) -> Vec<DiffLine> {
        let mut lines = vec![DiffLine::file_header("f.rs")];
        lines.extend(entries);
        lines
    }

    fn comment(start: Option<usize>, end: Option<usize>) -> ReviewComment {
        ReviewComment {
            path: "f.rs".into(),
            start_line: start,
            end_line: end,
            side: CommentSide::New,
            body: "c".into(),
            author: None,
            suggestion: None,
            outdated: false,
        }
    }

    /// `(line index, outdated)` of the only anchored comment.
    fn anchor_of(lines: &[DiffLine], c: ReviewComment) -> (usize, bool) {
        let anchored = anchor_comments(lines, &[c]);
        assert_eq!(anchored.by_line.len(), 1);
        let (idx, list) = anchored.by_line.iter().next().unwrap();
        (*idx, list[0].outdated)
    }

    fn base_run(range: std::ops::RangeInclusive<usize>) -> Vec<DiffLine> {
        range.map(|n| numbered(n, LineSource::Base)).collect()
    }

    #[test]
    fn paths_match_ignores_a_b_prefixes_on_either_side() {
        assert!(paths_match("src/x.rs", "a/src/x.rs"));
        assert!(paths_match("src/x.rs", "b/src/x.rs"));
        assert!(paths_match("a/src/x.rs", "src/x.rs"));
        assert!(paths_match("b/src/x.rs", "src/x.rs"));
        assert!(paths_match("./src/x.rs", "/src/x.rs"));
        assert!(!paths_match("src/x.rs", "a/src/y.rs"));
        assert!(!paths_match("a/src/y.rs", "src/x.rs"));
    }

    #[test]
    fn file_diff_matches_either_side_of_a_rename_header() {
        let header = DiffLine::new(LineSource::FileHeader, "old.rs → new.rs".into(), ' ', None);
        let file = FileDiff::new(vec![header]);
        assert!(file_diff_matches_path(&file, "old.rs"));
        assert!(file_diff_matches_path(&file, "new.rs"));
        assert!(!file_diff_matches_path(&file, "other.rs"));
    }

    #[test]
    fn a_plain_change_puts_five_lines_either_side_in_the_diff() {
        // Change at line 4; lines 1..=9 are within five of it, 10 and beyond are not.
        let mut entries = base_run(1..=30);
        entries[3] = numbered(4, LineSource::Committed);
        let lines = file(entries);
        assert_eq!(anchor_of(&lines, comment(Some(9), Some(9))), (9, false));
        assert_eq!(anchor_of(&lines, comment(Some(10), Some(10))), (0, true));
        assert_eq!(anchor_of(&lines, comment(Some(14), Some(14))), (0, true));
    }

    #[test]
    fn a_line_far_from_every_change_is_outdated() {
        let lines = file(base_run(1..=30));
        assert_eq!(anchor_of(&lines, comment(Some(15), Some(15))), (0, true));
    }

    #[test]
    fn inline_spans_alone_put_nearby_lines_in_the_diff() {
        let mut entries = base_run(1..=30);
        entries[9].inline_spans =
            vec![InlineSpan { text: "x".into(), source: None, is_deletion: false }];
        let lines = file(entries);
        assert_eq!(anchor_of(&lines, comment(Some(12), Some(12))), (12, false));
        assert_eq!(anchor_of(&lines, comment(Some(25), Some(25))), (0, true));
    }

    #[test]
    fn a_comment_with_only_one_end_is_a_line_comment_not_a_file_comment() {
        let mut entries = base_run(1..=10);
        entries[3] = numbered(4, LineSource::Committed);
        let lines = file(entries);
        assert_eq!(anchor_of(&lines, comment(Some(4), None)), (4, false));
        assert_eq!(anchor_of(&lines, comment(None, Some(4))), (4, false));
        assert_eq!(anchor_of(&lines, comment(None, None)), (0, false));
    }

    #[test]
    fn single_line_comment_skips_elided_and_header_rows_carrying_its_number() {
        let mut entries = base_run(1..=6);
        entries[3] = numbered(4, LineSource::Committed);
        // An elided marker that happens to carry line number 5, placed before line 5.
        let mut elided = DiffLine::elided(3);
        elided.line_number = Some(5);
        entries.insert(4, elided);
        let lines = file(entries);
        assert_eq!(lines[5].source, LineSource::Elided);
        assert_eq!(anchor_of(&lines, comment(Some(5), Some(5))), (6, false));
    }

    #[test]
    fn range_comment_anchors_to_last_range_line_in_the_diff_when_end_is_missing() {
        // Lines 1-4 and 6-8: line 5 is absent, so the range 3..=5 settles on 4.
        let mut entries = base_run(1..=4);
        entries.extend(base_run(6..=8));
        entries[3] = numbered(4, LineSource::Committed);
        let lines = file(entries);
        assert_eq!(anchor_of(&lines, comment(Some(3), Some(5))), (4, false));
        // A range wholly outside the diff stays outdated.
        assert_eq!(anchor_of(&lines, comment(Some(20), Some(22))), (0, true));
    }

    #[test]
    fn range_comment_never_lands_on_an_elided_row() {
        let mut entries = base_run(1..=3);
        entries[1] = numbered(2, LineSource::Committed);
        let mut elided = DiffLine::elided(3);
        elided.line_number = Some(5);
        entries.push(elided);
        let lines = file(entries);
        assert_eq!(anchor_of(&lines, comment(Some(2), Some(5))), (3, false));
    }

    #[test]
    fn range_comment_prefers_the_exact_end_line_over_a_later_line_in_the_range() {
        // Rows in an unusual order: the end line (5) comes before line 4.
        let mut entries = vec![
            numbered(3, LineSource::Base),
            numbered(5, LineSource::Committed),
            numbered(4, LineSource::Base),
        ];
        entries.push(numbered(9, LineSource::Base));
        let lines = file(entries);
        assert_eq!(anchor_of(&lines, comment(Some(3), Some(5))), (2, false));
    }

    #[test]
    fn a_deleted_line_never_answers_for_a_new_side_line_number() {
        // A deletion carrying line number 3 is not new-side line 3.
        let entries = vec![
            numbered(2, LineSource::Committed),
            DiffLine::new(LineSource::DeletedCommitted, "gone".into(), '-', Some(3))
                .with_file_path("f.rs"),
            numbered(4, LineSource::Base),
        ];
        let lines = file(entries);
        assert_eq!(anchor_of(&lines, comment(Some(3), Some(3))), (0, true));
    }
}
