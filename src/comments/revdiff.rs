use super::{CommentError, CommentSide, ReviewComment};

struct HeaderSpec {
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
    side: CommentSide,
}

pub(super) fn looks_like_revdiff(input: &str) -> bool {
    input.lines().any(|line| line.starts_with("## "))
}

pub(super) fn parse_revdiff(input: &str) -> Result<Vec<ReviewComment>, CommentError> {
    let mut comments = Vec::new();
    let mut current_header: Option<HeaderSpec> = None;
    let mut body_lines: Vec<&str> = Vec::new();

    for raw_line in input.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if let Some(header_rest) = line.strip_prefix("## ") {
            if let Some(spec) = current_header.take() {
                comments.push(finish_record(spec, &body_lines));
                body_lines.clear();
            }
            current_header = Some(parse_header_line(header_rest)?);
        } else if current_header.is_some() {
            let unescaped = if line.starts_with(" ##") { &line[1..] } else { line };
            body_lines.push(unescaped);
        } else if !line.trim().is_empty() {
            return Err(CommentError::Parse(format!(
                "unexpected text before first revdiff '## ' header: {line}"
            )));
        }
    }

    if let Some(spec) = current_header.take() {
        comments.push(finish_record(spec, &body_lines));
    }

    Ok(comments)
}

fn finish_record(spec: HeaderSpec, body_lines: &[&str]) -> ReviewComment {
    let mut start = 0;
    let mut end = body_lines.len();
    while start < end && body_lines[start].is_empty() {
        start += 1;
    }
    while end > start && body_lines[end - 1].is_empty() {
        end -= 1;
    }
    let body = body_lines[start..end].join("\n");
    ReviewComment {
        path: spec.path,
        start_line: spec.start_line,
        end_line: spec.end_line,
        side: spec.side,
        body,
        author: None,
    }
}

fn parse_header_line(rest: &str) -> Result<HeaderSpec, CommentError> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return Err(CommentError::Parse("empty revdiff header".into()));
    }

    if let Some(raw_path) = trimmed.strip_suffix(" (file-level)") {
        let path = super::anchor::normalize_comment_path(raw_path.trim());
        if path.is_empty() {
            return Err(CommentError::Parse("empty path in revdiff file-level header".into()));
        }
        return Ok(HeaderSpec { path, start_line: None, end_line: None, side: CommentSide::New });
    }

    let (target_part, side) = if let Some(prefix) = trimmed.strip_suffix(" (+)") {
        (prefix.trim(), CommentSide::New)
    } else if let Some(prefix) = trimmed.strip_suffix(" (-)") {
        (prefix.trim(), CommentSide::Old)
    } else {
        (trimmed, CommentSide::New)
    };

    if let Some((raw_path, line_spec)) = target_part.rsplit_once(':')
        && let Some((start, end)) = parse_line_spec(line_spec)
    {
        let path = super::anchor::normalize_comment_path(raw_path.trim());
        if path.is_empty() {
            return Err(CommentError::Parse("empty path in revdiff header".into()));
        }
        return Ok(HeaderSpec { path, start_line: Some(start), end_line: Some(end), side });
    }

    Err(CommentError::Parse(format!("invalid revdiff header: '## {trimmed}'")))
}

fn parse_line_spec(spec: &str) -> Option<(usize, usize)> {
    if let Some((a_str, b_str)) = spec.split_once('-') {
        let a = a_str.parse::<usize>().ok().filter(|&n| n > 0)?;
        let b = b_str.parse::<usize>().ok().filter(|&n| n > 0)?;
        Some((a.min(b), a.max(b)))
    } else {
        let n = spec.parse::<usize>().ok().filter(|&n| n > 0)?;
        Some((n, n))
    }
}
