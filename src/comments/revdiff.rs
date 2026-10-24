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
        suggestion: None,
        outdated: false,
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
        let a = parse_line_num(a_str)?;
        let b = parse_line_num(b_str)?;
        Some((a.min(b), a.max(b)))
    } else {
        let n = parse_line_num(spec)?;
        Some((n, n))
    }
}

fn parse_line_num(s: &str) -> Option<usize> {
    let n = s.parse::<i64>().ok().filter(|&n| n > 0)?;
    usize::try_from(n).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_double_hash_header_marks_a_revdiff_document() {
        assert!(looks_like_revdiff("## a.rs:1 (+)\nnote"));
        assert!(looks_like_revdiff("\npreamble\n## a.rs (file-level)"));
        assert!(!looks_like_revdiff(r#"{"path":"a.rs","body":"x"}"#));
        assert!(!looks_like_revdiff("#\n# a.rs:1\n##a.rs"));
        assert!(!looks_like_revdiff(""));
    }

    #[test]
    fn blank_lines_before_the_first_header_are_allowed_and_text_is_not() {
        let ok = parse_revdiff("\n  \n## a.rs:1\nnote").unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].body, "note");
        let err = parse_revdiff("stray\n## a.rs:1\nnote").unwrap_err().to_string();
        assert!(err.contains("unexpected text before first revdiff"), "{err}");
    }

    #[test]
    fn blank_lines_around_a_body_are_trimmed_and_inner_ones_kept() {
        let comments = parse_revdiff("## a.rs:1\n\n\nfirst\n\nsecond\n\n\n## b.rs:2\nx").unwrap();
        assert_eq!(comments[0].body, "first\n\nsecond");
        assert_eq!(comments[1].body, "x");
    }

    #[test]
    fn a_header_with_no_body_or_only_blank_lines_has_an_empty_body() {
        assert_eq!(parse_revdiff("## a.rs:1").unwrap()[0].body, "");
        assert_eq!(parse_revdiff("## a.rs:1\n").unwrap()[0].body, "");
        assert_eq!(parse_revdiff("## a.rs:1\n\n\n").unwrap()[0].body, "");
        let two = parse_revdiff("## a.rs:1\n\n## b.rs:2\n\n").unwrap();
        assert_eq!((two[0].body.as_str(), two[1].body.as_str()), ("", ""));
    }

    #[test]
    fn line_numbers_must_be_positive() {
        assert!(parse_revdiff("## a.rs:0\nx").is_err());
        assert!(parse_revdiff("## a.rs:0-3\nx").is_err());
        assert!(parse_revdiff("## a.rs:-1\nx").is_err());
        let ok = parse_revdiff("## a.rs:1-3 (-)\nx").unwrap();
        assert_eq!((ok[0].start_line, ok[0].end_line), (Some(1), Some(3)));
        assert_eq!(ok[0].side, CommentSide::Old);
    }
}
