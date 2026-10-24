use super::json::{JsonVal, parse_json_stream};
use super::{CommentError, CommentSide, ReviewComment};

pub(super) fn parse_json_comments(input: &str) -> Result<Vec<ReviewComment>, CommentError> {
    let values = parse_json_stream(input)?;
    let mut out = Vec::new();
    for val in &values {
        extract_from_value(val, &mut out)?;
    }
    Ok(out)
}

fn extract_from_value(val: &JsonVal, out: &mut Vec<ReviewComment>) -> Result<(), CommentError> {
    match val {
        JsonVal::Array(items) => {
            for item in items {
                extract_from_object(item, None, out)?;
            }
            Ok(())
        }
        JsonVal::Object(_) => {
            if let Some(diags) = val.get("diagnostics").and_then(JsonVal::as_array) {
                let default_source =
                    val.get("source").and_then(|s| s.get("name")).and_then(JsonVal::as_str);
                for diag in diags {
                    out.push(extract_reviewdog_diagnostic(diag, default_source)?);
                }
                Ok(())
            } else {
                extract_from_object(val, None, out)
            }
        }
        _ => Err(CommentError::Parse("top-level JSON value must be an object or array".into())),
    }
}

fn extract_from_object(
    val: &JsonVal,
    default_source: Option<&str>,
    out: &mut Vec<ReviewComment>,
) -> Result<(), CommentError> {
    if val.get("location").is_some() && val.get("message").is_some() {
        out.push(extract_reviewdog_diagnostic(val, default_source)?);
        return Ok(());
    }
    let path = val
        .get("path")
        .and_then(JsonVal::as_str)
        .ok_or_else(|| CommentError::Parse("comment object missing 'path'".into()))?;
    let path = super::anchor::normalize_comment_path(path);
    if path.is_empty() {
        return Err(CommentError::Parse("comment 'path' cannot be empty".into()));
    }
    let body = val
        .get("body")
        .or_else(|| val.get("message"))
        .and_then(JsonVal::as_str)
        .ok_or_else(|| CommentError::Parse("comment object missing 'body'".into()))?
        .to_string();

    let side = match val.get("side").and_then(JsonVal::as_str) {
        None => CommentSide::New,
        Some(s) if s.eq_ignore_ascii_case("new") || s.eq_ignore_ascii_case("right") => {
            CommentSide::New
        }
        Some(s) if s.eq_ignore_ascii_case("old") || s.eq_ignore_ascii_case("left") => {
            CommentSide::Old
        }
        Some(other) => {
            return Err(CommentError::Parse(format!("unknown comment side '{other}'")));
        }
    };

    let is_file_subject = val
        .get("subject_type")
        .and_then(JsonVal::as_str)
        .is_some_and(|s| s.eq_ignore_ascii_case("file"));

    let number = |key: &str| val.get(key).and_then(JsonVal::as_usize);
    let current_end = number("end_line").or_else(|| number("line"));
    // GitHub sends `line: null` once the line is gone from the PR head and keeps
    // only the numbers from the revision commented on; those never anchor.
    let outdated = !is_file_subject
        && (val.get("outdated") == Some(&JsonVal::Bool(true))
            || (current_end.is_none() && number("original_line").is_some()));
    let (start_line, end_line) = if is_file_subject {
        (None, None)
    } else {
        let end = current_end.or_else(|| number("original_line"));
        let start = if outdated {
            number("start_line").or_else(|| number("original_start_line"))
        } else {
            number("start_line")
        }
        .or(end);
        match (start, end) {
            (Some(s), Some(e)) => (Some(s.min(e)), Some(s.max(e))),
            (Some(s), None) => (Some(s), Some(s)),
            (None, Some(e)) => (Some(e), Some(e)),
            (None, None) => (None, None),
        }
    };

    let suggestion = val.get("suggestion").and_then(JsonVal::as_str).map(str::to_string);

    let author = val
        .get("author")
        .and_then(JsonVal::as_str)
        .or_else(|| val.get("user").and_then(|u| u.get("login")).and_then(JsonVal::as_str))
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    out.push(ReviewComment {
        path,
        start_line,
        end_line,
        side,
        body,
        author,
        suggestion,
        outdated,
    });
    Ok(())
}

fn extract_reviewdog_diagnostic(
    diag: &JsonVal,
    default_source: Option<&str>,
) -> Result<ReviewComment, CommentError> {
    let body = diag
        .get("message")
        .and_then(JsonVal::as_str)
        .ok_or_else(|| CommentError::Parse("reviewdog diagnostic missing 'message'".into()))?
        .to_string();
    let loc = diag
        .get("location")
        .ok_or_else(|| CommentError::Parse("reviewdog diagnostic missing 'location'".into()))?;
    let raw_path = loc
        .get("path")
        .and_then(JsonVal::as_str)
        .ok_or_else(|| CommentError::Parse("reviewdog location missing 'path'".into()))?;
    let path = super::anchor::normalize_comment_path(raw_path);
    if path.is_empty() {
        return Err(CommentError::Parse("reviewdog location 'path' cannot be empty".into()));
    }

    let range = loc.get("range");
    let start =
        range.and_then(|r| r.get("start")).and_then(|s| s.get("line")).and_then(JsonVal::as_usize);
    let end = range
        .and_then(|r| r.get("end"))
        .and_then(|e| e.get("line"))
        .and_then(JsonVal::as_usize)
        .or(start);
    let (start_line, end_line) = match (start, end) {
        (Some(s), Some(e)) => (Some(s.min(e)), Some(s.max(e))),
        (Some(s), None) => (Some(s), Some(s)),
        (None, Some(e)) => (Some(e), Some(e)),
        (None, None) => (None, None),
    };

    let author = diag
        .get("source")
        .and_then(|s| s.get("name"))
        .and_then(JsonVal::as_str)
        .or(default_source)
        .or_else(|| diag.get("code").and_then(|c| c.get("value")).and_then(JsonVal::as_str))
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    Ok(ReviewComment {
        path,
        start_line,
        end_line,
        side: CommentSide::New,
        body,
        author,
        suggestion: None,
        outdated: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Result<Vec<ReviewComment>, CommentError> {
        parse_json_comments(input)
    }

    #[test]
    fn a_message_without_a_location_is_a_plain_comment_body() {
        let comments = parse(r#"{"path":"a.rs","line":2,"message":"hi"}"#).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].body, "hi");
        assert_eq!(comments[0].start_line, Some(2));
    }

    #[test]
    fn a_location_without_a_message_is_not_a_diagnostic() {
        let comments = parse(r#"{"path":"a.rs","body":"hi","location":{"path":"b.rs"}}"#).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].path, "a.rs");
        assert_eq!(comments[0].body, "hi");
    }

    #[test]
    fn a_location_with_a_message_is_a_reviewdog_diagnostic() {
        let comments = parse(r#"{"message":"m","location":{"path":"b.rs"}}"#).unwrap();
        assert_eq!(comments[0].path, "b.rs");
        assert_eq!(comments[0].body, "m");
    }

    #[test]
    fn sides_are_case_insensitive_names_and_anything_else_is_an_error() {
        let side = |s: &str| {
            parse(&format!(r#"{{"path":"a.rs","body":"x","side":"{s}"}}"#)).map(|c| c[0].side)
        };
        for name in ["new", "RIGHT"] {
            assert_eq!(side(name).unwrap(), CommentSide::New, "{name}");
        }
        for name in ["old", "Left"] {
            assert_eq!(side(name).unwrap(), CommentSide::Old, "{name}");
        }
        let err = side("middle").unwrap_err().to_string();
        assert!(err.contains("unknown comment side 'middle'"), "{err}");
    }
}
