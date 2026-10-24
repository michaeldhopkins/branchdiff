//! Suggested changes: replacement text a reviewer proposes for the lines a
//! comment covers. They come from a GitHub-style fenced block in the body or
//! from the JSON lines `suggestion` field, and are only ever shown, never applied.

/// The label above a suggestion, in every output.
pub const SUGGESTION_LABEL: &str = "Suggested change";

/// A comment body split into its prose and its suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyParts<'a> {
    /// The body without the suggestion fence; `[""]` for an empty body.
    pub text: Vec<&'a str>,
    /// The proposed lines. `Some(vec![])` proposes deleting the covered lines.
    pub suggestion: Option<Vec<&'a str>>,
}

/// The opening fence's backtick count when `line` opens a suggestion block
/// (three or more backticks, then `suggestion`, optionally `:...` as older
/// GitHub text had).
fn opening_ticks(line: &str) -> Option<usize> {
    let trimmed = line.trim();
    let ticks = trimmed.len() - trimmed.trim_start_matches('`').len();
    let rest = trimmed[ticks..].trim();
    let is_suggestion = rest == "suggestion" || rest.starts_with("suggestion:");
    (ticks >= 3 && is_suggestion).then_some(ticks)
}

fn closes(line: &str, ticks: usize) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= ticks && trimmed.bytes().all(|b| b == b'`')
}

/// Split `body` at its first closed suggestion fence. An explicit `suggestion`
/// wins over a fenced one; the fence is dropped from the text either way. An
/// unclosed fence stays in the text.
pub fn split_body<'a>(body: &'a str, explicit: Option<&'a str>) -> BodyParts<'a> {
    let lines: Vec<&str> = body.lines().collect();
    let mut text = Vec::new();
    let mut fenced: Option<Vec<&str>> = None;
    let mut i = 0;
    while i < lines.len() {
        if fenced.is_none()
            && let Some(ticks) = opening_ticks(lines[i])
            && let Some(close) = (i + 1..lines.len()).find(|&j| closes(lines[j], ticks))
        {
            fenced = Some(lines[i + 1..close].to_vec());
            i = close + 1;
            continue;
        }
        text.push(lines[i]);
        i += 1;
    }
    let suggestion = explicit.map(|s| s.lines().collect()).or(fenced);
    if suggestion.is_some() {
        while text.last().is_some_and(|l| l.trim().is_empty()) {
            text.pop();
        }
    } else if text.is_empty() {
        text.push("");
    }
    BodyParts { text, suggestion }
}

/// Whether `body`/`explicit` carry a suggestion.
pub fn has_suggestion(body: &str, explicit: Option<&str>) -> bool {
    split_body(body, explicit).suggestion.is_some()
}

/// The rows of a suggested change as a small diff: the replaced lines as
/// `-`, the proposed lines as `+`.
pub fn diff_rows(replaced: &[String], suggestion: &[&str]) -> Vec<(char, String)> {
    replaced
        .iter()
        .map(|l| ('-', l.clone()))
        .chain(suggestion.iter().map(|l| ('+', (*l).to_string())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fenced_suggestion_is_split_from_the_prose() {
        let parts = split_body(
            "Use a slice.\n\n```suggestion\nfn f(x: &[u8]) {\n    g(x)\n```\nthanks",
            None,
        );
        assert_eq!(parts.text, vec!["Use a slice.", "", "thanks"]);
        assert_eq!(parts.suggestion, Some(vec!["fn f(x: &[u8]) {", "    g(x)"]));
    }

    #[test]
    fn trailing_blank_lines_before_the_fence_are_dropped() {
        let parts = split_body("Prose\n\n```suggestion\nx\n```", None);
        assert_eq!(parts.text, vec!["Prose"]);
    }

    #[test]
    fn an_empty_fence_proposes_deleting_the_lines() {
        let parts = split_body("```suggestion\n```", None);
        assert_eq!(parts.suggestion, Some(vec![]));
        assert!(parts.text.is_empty(), "nothing but the fence leaves no prose row");
    }

    #[test]
    fn longer_fences_and_indentation_are_accepted() {
        let parts = split_body("  ````suggestion\na\n```\n````", None);
        assert_eq!(parts.suggestion, Some(vec!["a", "```"]));
    }

    #[test]
    fn an_unclosed_fence_is_ordinary_text() {
        let parts = split_body("```suggestion\nx", None);
        assert_eq!(parts.suggestion, None);
        assert_eq!(parts.text, vec!["```suggestion", "x"]);
    }

    #[test]
    fn other_fences_are_ordinary_text() {
        let parts = split_body("```rust\nlet x = 1;\n```\n```suggestionist\nz\n```", None);
        assert_eq!(parts.suggestion, None);
        assert_eq!(parts.text.len(), 6);
    }

    #[test]
    fn only_the_first_suggestion_is_taken() {
        let parts = split_body("```suggestion\na\n```\n```suggestion\nb\n```", None);
        assert_eq!(parts.suggestion, Some(vec!["a"]));
        assert_eq!(parts.text, vec!["```suggestion", "b", "```"]);
    }

    #[test]
    fn the_explicit_field_wins_and_an_empty_body_keeps_its_blank_row() {
        let parts = split_body("", Some("new\nlines"));
        assert_eq!(parts.suggestion, Some(vec!["new", "lines"]));
        let plain = split_body("", None);
        assert_eq!(plain.text, vec![""]);
        assert_eq!(split_body("x\n```suggestion\nfenced\n```", Some("")).suggestion, Some(vec![]));
    }

    #[test]
    fn rows_list_removed_lines_then_added_lines() {
        let rows = diff_rows(&["old".to_string()], &["new1", "new2"]);
        assert_eq!(rows, vec![('-', "old".into()), ('+', "new1".into()), ('+', "new2".into())]);
    }
}
