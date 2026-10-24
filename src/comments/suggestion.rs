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
            // From `i` itself: the opening line holds "suggestion", so it never closes.
            && let Some(close) = (i..lines.len()).find(|&j| closes(lines[j], ticks))
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

/// Sixty-four backticks, then `suggestion`: the fences `unsplit_body` writes are
/// slices of it, so they can borrow like the rest of the text.
const FENCE: &str =
    concat!("````````````````````````````````", "````````````````````````````````", "suggestion");
const FENCE_TICKS: usize = 64;

/// The fence length GitHub would use around `s`: one more backtick than its
/// longest run, and at least three.
fn fence_ticks(s: &str) -> usize {
    let longest = s.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    (longest + 1).clamp(3, FENCE_TICKS)
}

/// `body` as written, for a comment whose lines are not in the diff: with
/// nothing to show as replaced, a suggestion would read as an insertion (or,
/// for a deletion, as an empty change). So its fence stays in the text, and an
/// explicit `suggestion`, which wins over a fence as in `split_body`, is shown
/// in place of it as the fence GitHub would have written.
pub fn unsplit_body<'a>(body: &'a str, explicit: Option<&'a str>) -> BodyParts<'a> {
    let Some(s) = explicit else {
        let mut text: Vec<&str> = body.lines().collect();
        if text.is_empty() {
            text.push("");
        }
        return BodyParts { text, suggestion: None };
    };
    let mut text = split_body(body, None).text;
    while text.last().is_some_and(|l| l.trim().is_empty()) {
        text.pop();
    }
    let ticks = fence_ticks(s);
    text.push(&FENCE[FENCE_TICKS - ticks..]);
    text.extend(s.lines());
    text.push(&FENCE[..ticks]);
    BodyParts { text, suggestion: None }
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
    fn has_suggestion_is_true_for_a_fence_or_a_field_and_false_otherwise() {
        assert!(has_suggestion("```suggestion\nx\n```", None));
        assert!(has_suggestion("plain", Some("x")));
        assert!(has_suggestion("", Some("")));
        assert!(!has_suggestion("plain", None));
        assert!(!has_suggestion("```suggestion\nunclosed", None));
    }

    #[test]
    fn rows_list_removed_lines_then_added_lines() {
        let rows = diff_rows(&["old".to_string()], &["new1", "new2"]);
        assert_eq!(rows, vec![('-', "old".into()), ('+', "new1".into()), ('+', "new2".into())]);
    }

    #[test]
    fn unsplit_keeps_the_body_as_written_and_writes_a_field_as_a_fence() {
        let fenced = "Hm.\n```suggestion\nx\n```";
        assert_eq!(
            unsplit_body(fenced, None),
            BodyParts { text: fenced.lines().collect(), suggestion: None }
        );
        assert_eq!(unsplit_body("", None).text, [""]);
        assert_eq!(
            unsplit_body("Hm.\n\n", Some("a\nb")).text,
            ["Hm.", "```suggestion", "a", "b", "```"]
        );
        assert_eq!(unsplit_body("", Some("")).text, ["```suggestion", "```"]);
    }

    #[test]
    fn unsplit_shows_the_field_in_place_of_a_fence_as_split_body_does() {
        let text = unsplit_body("Hm\n```suggestion\nfenced\n```\nthen", Some("field")).text;
        assert_eq!(text, ["Hm", "then", "```suggestion", "field", "```"]);
    }

    #[test]
    fn unsplit_fences_a_field_with_backticks_in_a_longer_fence() {
        assert_eq!(fence_ticks("a"), 3);
        assert_eq!(fence_ticks("a\n```\nb"), 4);
        assert_eq!(fence_ticks("x ````` y ``"), 6);
        assert_eq!(fence_ticks(&"`".repeat(200)), FENCE_TICKS);
        let text = unsplit_body("", Some("a\n```\nb")).text;
        assert_eq!(text, ["````suggestion", "a", "```", "b", "````"]);
    }
}
