use std::collections::HashMap;

use crate::diff::{DiffLine, FileDiff, LineSource};

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

    // For a line range, anchor to the last line of the range that is within the diff context
    if target_start < target_end {
        for (pos, &line_idx) in span.line_indices.iter().enumerate().rev() {
            if span.in_diff[pos] && matches_range(&lines[line_idx]) {
                return Some(line_idx);
            }
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

        match matched_span {
            Some(span) if is_file_level => {
                by_line
                    .entry(span.header_idx)
                    .or_default()
                    .push(AnchoredComment { comment: comment.clone(), outdated: false });
            }
            Some(span) => {
                if let Some(line_idx) = find_anchor_in_span(lines, span, comment) {
                    by_line
                        .entry(line_idx)
                        .or_default()
                        .push(AnchoredComment { comment: comment.clone(), outdated: false });
                } else {
                    by_line
                        .entry(span.header_idx)
                        .or_default()
                        .push(AnchoredComment { comment: comment.clone(), outdated: true });
                }
            }
            None => {
                by_line
                    .entry(0)
                    .or_default()
                    .push(AnchoredComment { comment: comment.clone(), outdated: true });
            }
        }
    }

    AnchoredComments { by_line }
}
