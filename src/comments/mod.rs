//! Review comment parsing, autodetection, anchoring, and rendering.
//!
//! Supports four comment formats detected from content:
//! 1. `branchdiff` JSON Lines (`path`, `line` or `start_line`/`end_line`, `side`, `body`, optional `author`)
//! 2. `revdiff` plain-text annotations (`## path:LINE (+)`, `## path:A-B (-)`, `## path (file-level)`)
//! 3. `reviewdog` `rdjson` / `rdjsonl` (`diagnostics[].location.path` and `range.start.line`)
//! 4. GitHub pull request review comments JSON (`gh api repos/O/R/pulls/N/comments`)

pub mod anchor;
mod extract;
mod json;
pub mod label;
pub mod patch;
pub mod render;
mod revdiff;
pub mod sanitize;
pub mod suggestion;

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

pub use anchor::{
    anchor_comments, ensure_commented_files, file_diff_matches_path, normalize_comment_path,
    paths_match,
};
pub use json::comment_to_json_line;
pub use label::LabelStyle;

/// Default candidate paths (relative to the repository root) checked when
/// `--comments` is not specified.
pub const DEFAULT_COMMENT_CANDIDATES: &[&str] = &[
    ".jj/comments.jsonl", ".jj/comments.json", ".jj/comments.txt", ".jj/comments",
    ".git/comments.jsonl", ".git/comments.json", ".git/comments.txt", ".git/comments",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentsSource {
    /// Autodetected comments file in `.jj/` or `.git/` (if any currently exists).
    Auto(Option<PathBuf>),
    /// Explicit file path passed via `--comments <PATH>`.
    Explicit(PathBuf),
    /// Standard input passed via `--comments -`.
    Stdin,
}

impl Default for CommentsSource {
    fn default() -> Self {
        Self::Auto(None)
    }
}

impl CommentsSource {
    pub fn watch_path(&self) -> Option<&Path> {
        match self {
            Self::Auto(Some(p)) | Self::Explicit(p) => Some(p.as_path()),
            Self::Auto(None) | Self::Stdin => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommentSide {
    #[default]
    New,
    Old,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewComment {
    pub path: String,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub side: CommentSide,
    pub body: String,
    pub author: Option<String>,
    /// Replacement text for the lines this comment covers (the JSON `suggestion` field).
    /// A GitHub suggestion fence in `body` means the same; see `suggestion::split_body`.
    pub suggestion: Option<String>,
    /// The line numbers refer to an older revision of the file (a GitHub comment whose
    /// `line` is null keeps only `original_line`), so they never anchor to the current
    /// diff: the comment shows under the file header, outside the diff.
    pub outdated: bool,
}

impl ReviewComment {
    pub fn to_json_line(&self) -> String {
        comment_to_json_line(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchoredComment {
    pub comment: ReviewComment,
    /// True when the comment's line anchor was not in the current diff, so it
    /// fell back to the file header with its original line number preserved.
    pub outdated: bool,
    /// Text of the lines the comment covers, kept for a suggestion to replace;
    /// empty when there is no suggestion or the lines are not in the diff.
    pub replaced: Vec<String>,
}

impl AnchoredComment {
    pub fn new(comment: &ReviewComment, outdated: bool, replaced: Vec<String>) -> Self {
        Self { comment: comment.clone(), outdated, replaced }
    }

    /// The comment text split from its suggestion. A comment that is not on
    /// lines of the diff (file-level, outdated or outside it) has nothing a
    /// suggestion could be shown replacing, so its body stays as written.
    pub fn parts(&self) -> suggestion::BodyParts<'_> {
        let explicit = self.comment.suggestion.as_deref();
        let on_lines = self.comment.end_line.or(self.comment.start_line).is_some();
        if self.outdated || !on_lines {
            return suggestion::unsplit_body(&self.comment.body, explicit);
        }
        suggestion::split_body(&self.comment.body, explicit)
    }

    /// Rows of the suggested change as a diff, or none without a suggestion.
    pub fn suggestion_rows(&self) -> Vec<(char, String)> {
        self.parts()
            .suggestion
            .map(|s| suggestion::diff_rows(&self.replaced, &s))
            .unwrap_or_default()
    }

    pub fn location_label(&self) -> String {
        self.location_label_in(label::label_style())
    }

    pub fn location_label_in(&self, style: LabelStyle) -> String {
        label::location_label(&self.comment, self.outdated, style)
    }

    pub fn header_label(&self) -> String {
        let loc = self.location_label();
        match &self.comment.author {
            Some(author) if !author.is_empty() => format!("@{author} · {loc}"),
            _ => loc,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnchoredComments {
    pub by_line: HashMap<usize, Vec<AnchoredComment>>,
}

impl AnchoredComments {
    pub fn is_empty(&self) -> bool {
        self.by_line.is_empty()
    }

    pub fn for_line(&self, line_idx: usize) -> Option<&[AnchoredComment]> {
        self.by_line.get(&line_idx).map(Vec::as_slice)
    }

    pub fn rendered_height_for_line(&self, line_idx: usize, content_width: usize) -> usize {
        if self.by_line.is_empty() {
            return 0;
        }
        render::tui_comment_rows_for_line(self, line_idx, content_width)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentError {
    Io(String),
    Parse(String),
}

impl std::fmt::Display for CommentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "failed to read comments: {msg}"),
            Self::Parse(msg) => write!(f, "failed to parse comments: {msg}"),
        }
    }
}

impl std::error::Error for CommentError {}

/// Serialize comments to `branchdiff` JSON Lines format (one JSON object per line).
pub fn to_jsonl(comments: &[ReviewComment]) -> String {
    let mut out = String::new();
    for comment in comments {
        out.push_str(&comment_to_json_line(comment));
        out.push('\n');
    }
    out
}

/// Parse review comments from `input`, auto-detecting the format from content:
/// `branchdiff` JSONL, `revdiff` plain-text annotations, `reviewdog` rdjson/rdjsonl,
/// or GitHub pull request review comments JSON.
pub fn parse_comments(input: &str) -> Result<Vec<ReviewComment>, CommentError> {
    let trimmed = input.strip_prefix('\u{feff}').unwrap_or(input).trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        extract::parse_json_comments(trimmed)
    } else if revdiff::looks_like_revdiff(trimmed) {
        revdiff::parse_revdiff(trimmed)
    } else {
        Err(CommentError::Parse(
            "unrecognized comments format (expected JSON/JSONL or revdiff '## ' headers)".into(),
        ))
    }
}

/// Find the first existing default comments file in `.jj/` or `.git/` under `repo_root`.
pub fn detect_comments_file(repo_root: &Path) -> Option<PathBuf> {
    for rel in DEFAULT_COMMENT_CANDIDATES {
        let candidate = repo_root.join(rel);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Whether `path` is one of the default autodetected comments locations in `repo_root`.
pub fn is_default_comment_path(repo_root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(repo_root) else {
        return false;
    };
    let rel_str = rel.to_string_lossy();
    DEFAULT_COMMENT_CANDIDATES.iter().any(|&cand| cand == rel_str)
}

/// Resolve the active comments file path: `explicit` when provided (unless `"-"`),
/// otherwise `detect_comments_file(repo_root)`.
pub fn resolve_comments_file(explicit: Option<&Path>, repo_root: &Path) -> Option<PathBuf> {
    match explicit {
        Some(p) if p == Path::new("-") => None,
        Some(p) => Some(p.to_path_buf()),
        None => detect_comments_file(repo_root),
    }
}

/// The most a comments source may hold. Review files are kilobytes; the cap
/// stops a wrong path or an endless pipe from filling memory.
pub const MAX_COMMENTS_BYTES: usize = 16 * 1024 * 1024;

/// Read at most `MAX_COMMENTS_BYTES` from `reader`, failing when there is more.
pub fn read_capped(reader: impl Read, what: &str) -> Result<Vec<u8>, CommentError> {
    let mut buf = Vec::new();
    reader
        .take(MAX_COMMENTS_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| CommentError::Io(format!("{what}: {e}")))?;
    if buf.len() > MAX_COMMENTS_BYTES {
        return Err(CommentError::Io(format!(
            "{what}: larger than {} MiB",
            MAX_COMMENTS_BYTES / (1024 * 1024)
        )));
    }
    Ok(buf)
}

fn capped_text(reader: impl Read, what: &str) -> Result<String, CommentError> {
    String::from_utf8(read_capped(reader, what)?)
        .map_err(|_| CommentError::Io(format!("{what}: not valid UTF-8")))
}

/// Read a comments file as text, within `MAX_COMMENTS_BYTES`.
pub fn read_comments_file(path: &Path) -> Result<String, CommentError> {
    let what = path.display().to_string();
    let file = std::fs::File::open(path).map_err(|e| CommentError::Io(format!("{what}: {e}")))?;
    capped_text(file, &what)
}

/// Read the raw bytes of the comments source for `--fingerprint` (returns `Ok(None)`
/// when no comments file is active).
pub fn read_comments_source_bytes(
    explicit: Option<&Path>,
    repo_root: &Path,
) -> Result<Option<Vec<u8>>, CommentError> {
    if explicit == Some(Path::new("-")) {
        return read_capped(std::io::stdin(), "stdin").map(Some);
    }
    let Some(path) = resolve_comments_file(explicit, repo_root) else {
        return Ok(None);
    };
    let what = path.display().to_string();
    let file = std::fs::File::open(&path).map_err(|e| CommentError::Io(format!("{what}: {e}")))?;
    read_capped(file, &what).map(Some)
}

/// Load and parse comments from `explicit` (`"-"` for stdin) or the autodetected
/// default location in `repo_root`.
pub fn load_comments(
    explicit: Option<&Path>,
    repo_root: &Path,
) -> Result<Vec<ReviewComment>, CommentError> {
    if explicit == Some(Path::new("-")) {
        return parse_comments(&capped_text(std::io::stdin(), "stdin")?);
    }
    load_comments_with_stdin(explicit, repo_root, None)
}

/// Testable comments loader that accepts pre-captured `stdin_text` when `explicit` is `"-"`.
pub fn load_comments_with_stdin(
    explicit: Option<&Path>,
    repo_root: &Path,
    stdin_text: Option<&str>,
) -> Result<Vec<ReviewComment>, CommentError> {
    if explicit == Some(Path::new("-")) {
        let text = stdin_text.unwrap_or("");
        return parse_comments(text);
    }
    let path = match explicit {
        Some(p) => p.to_path_buf(),
        None => match detect_comments_file(repo_root) {
            Some(p) => p,
            None => return Ok(Vec::new()),
        },
    };
    parse_comments(&read_comments_file(&path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffInput, DiffLine, LineSource, compute_four_way_diff};
    use proptest::prelude::*;

    fn arb_comment() -> impl Strategy<Value = ReviewComment> {
        let path = "[a-zA-Z0-9_]{1,12}(/[a-zA-Z0-9_.]{1,12}){0,3}";
        let line_spec = prop_oneof![
            Just((None, None)),
            (1usize..5000).prop_map(|n| (Some(n), Some(n))),
            (1usize..5000, 1usize..5000).prop_map(|(a, b)| (Some(a.min(b)), Some(a.max(b)))),
        ];
        let side = prop_oneof![Just(CommentSide::New), Just(CommentSide::Old)];
        let body = "[a-zA-Z0-9 _.,!?:;'\"\\\\/\\n\\t<>&é你]{0,80}";
        let author = prop::option::of("[a-zA-Z0-9_-]{1,16}");
        let suggestion = prop::option::of("[a-zA-Z0-9 \\n]{0,30}");
        let outdated = any::<bool>();

        (path, line_spec, side, body, author, suggestion, outdated).prop_map(
            |(path, (start_line, end_line), side, body, author, suggestion, outdated)| {
                ReviewComment {
                    path,
                    start_line,
                    end_line,
                    side,
                    body,
                    author,
                    suggestion,
                    outdated,
                }
            },
        )
    }

    proptest! {
        #[test]
        fn jsonl_roundtrip_preserves_comments(
            comments in prop::collection::vec(arb_comment(), 0..16)
        ) {
            let encoded = to_jsonl(&comments);
            let decoded = parse_comments(&encoded).expect("valid JSONL should parse");
            prop_assert_eq!(decoded, comments);
        }
    }

    #[test]
    fn test_parse_branchdiff_jsonl_fixture() {
        let input = r#"{"path":"src/lib.rs","line":14,"body":"Consider returning a Result here","author":"alice"}
{"path":"src/lib.rs","start_line":20,"end_line":25,"side":"old","body":"Why was this block removed?"}
{"path":"README.md","body":"Update example flags"}
"#;
        let comments = parse_comments(input).unwrap();
        assert_eq!(comments.len(), 3);
        assert_eq!(
            comments[0],
            ReviewComment {
                path: "src/lib.rs".into(),
                start_line: Some(14),
                end_line: Some(14),
                side: CommentSide::New,
                body: "Consider returning a Result here".into(),
                author: Some("alice".into()),
                suggestion: None,
                outdated: false,
            }
        );
        assert_eq!(
            comments[1],
            ReviewComment {
                path: "src/lib.rs".into(),
                start_line: Some(20),
                end_line: Some(25),
                side: CommentSide::Old,
                body: "Why was this block removed?".into(),
                author: None,
                suggestion: None,
                outdated: false,
            }
        );
        assert_eq!(
            comments[2],
            ReviewComment {
                path: "README.md".into(),
                start_line: None,
                end_line: None,
                side: CommentSide::New,
                body: "Update example flags".into(),
                author: None,
                suggestion: None,
                outdated: false,
            }
        );
    }

    #[test]
    fn test_parse_revdiff_fixture_with_escaped_hash_in_body() {
        let input = "\
## src/main.rs:42 (+)
Check for overflow here.
 ## Note: markdown heading inside comment body
Second line.

## src/main.rs:10-15 (-)
Deleted range comment.

## src/main.rs (file-level)
File-level summary note.
";
        let comments = parse_comments(input).unwrap();
        assert_eq!(comments.len(), 3);
        assert_eq!(comments[0].path, "src/main.rs");
        assert_eq!(comments[0].start_line, Some(42));
        assert_eq!(comments[0].end_line, Some(42));
        assert_eq!(comments[0].side, CommentSide::New);
        assert_eq!(
            comments[0].body,
            "Check for overflow here.\n## Note: markdown heading inside comment body\nSecond line."
        );

        assert_eq!(comments[1].path, "src/main.rs");
        assert_eq!(comments[1].start_line, Some(10));
        assert_eq!(comments[1].end_line, Some(15));
        assert_eq!(comments[1].side, CommentSide::Old);
        assert_eq!(comments[1].body, "Deleted range comment.");

        assert_eq!(comments[2].path, "src/main.rs");
        assert_eq!(comments[2].start_line, None);
        assert_eq!(comments[2].end_line, None);
        assert_eq!(comments[2].body, "File-level summary note.");
    }

    #[test]
    fn test_parse_reviewdog_rdjson_and_rdjsonl_fixtures() {
        let rdjson = r#"{
  "source": { "name": "clippy" },
  "diagnostics": [
    {
      "message": "unnecessary `.clone()` on a Copy type",
      "location": {
        "path": "./src/app/mod.rs",
        "range": {
          "start": { "line": 31, "column": 5 },
          "end": { "line": 31, "column": 18 }
        }
      },
      "severity": "WARNING"
    },
    {
      "message": "span warning across two lines",
      "location": {
        "path": "src/ui/mod.rs",
        "range": {
          "start": { "line": 10, "column": 1 },
          "end": { "line": 12, "column": 4 }
        }
      },
      "source": { "name": "rustc" }
    }
  ]
}"#;
        let comments = parse_comments(rdjson).unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].path, "src/app/mod.rs");
        assert_eq!(comments[0].start_line, Some(31));
        assert_eq!(comments[0].end_line, Some(31));
        assert_eq!(comments[0].side, CommentSide::New);
        assert_eq!(comments[0].author.as_deref(), Some("clippy"));
        assert_eq!(comments[0].body, "unnecessary `.clone()` on a Copy type");

        assert_eq!(comments[1].path, "src/ui/mod.rs");
        assert_eq!(comments[1].start_line, Some(10));
        assert_eq!(comments[1].end_line, Some(12));
        assert_eq!(comments[1].author.as_deref(), Some("rustc"));

        let rdjsonl = r#"{"message":"first diag","location":{"path":"src/a.rs","range":{"start":{"line":7}}}}
{"message":"second diag","location":{"path":"src/b.rs","range":{"start":{"line":9,"column":2}}},"code":{"value":"E001"}}
"#;
        let jsonl_comments = parse_comments(rdjsonl).unwrap();
        assert_eq!(jsonl_comments.len(), 2);
        assert_eq!(jsonl_comments[0].path, "src/a.rs");
        assert_eq!(jsonl_comments[0].start_line, Some(7));
        assert_eq!(jsonl_comments[1].author.as_deref(), Some("E001"));
    }

    #[test]
    fn test_parse_github_pr_review_comments_fixture() {
        let gh_json = r#"[
  {
    "id": 1001,
    "path": "src/diff/algorithm.rs",
    "line": 48,
    "start_line": 45,
    "side": "RIGHT",
    "body": "Prefer an iterator over indexing here.",
    "user": { "login": "octocat", "id": 1 }
  },
  {
    "id": 1002,
    "path": "src/diff/algorithm.rs",
    "line": null,
    "original_line": 19,
    "side": "LEFT",
    "body": "This deleted guard still matters for empty inputs.",
    "user": { "login": "reviewer" }
  },
  {
    "id": 1003,
    "path": "src/diff/algorithm.rs",
    "subject_type": "file",
    "line": 1,
    "side": "RIGHT",
    "body": "File-level PR review note.",
    "user": { "login": "lead" }
  }
]"#;
        let comments = parse_comments(gh_json).unwrap();
        assert_eq!(comments.len(), 3);
        assert_eq!(comments[0].path, "src/diff/algorithm.rs");
        assert_eq!(comments[0].start_line, Some(45));
        assert_eq!(comments[0].end_line, Some(48));
        assert_eq!(comments[0].side, CommentSide::New);
        assert_eq!(comments[0].author.as_deref(), Some("octocat"));

        assert_eq!(comments[1].start_line, Some(19));
        assert_eq!(comments[1].end_line, Some(19));
        assert_eq!(comments[1].side, CommentSide::Old);
        assert_eq!(comments[1].author.as_deref(), Some("reviewer"));

        assert_eq!(comments[2].start_line, None);
        assert_eq!(comments[2].end_line, None);
        assert_eq!(comments[2].author.as_deref(), Some("lead"));
    }

    #[test]
    fn test_anchoring_new_old_range_file_and_out_of_diff() {
        let base = "line1\ndeleted_old_2\nline3\nline4";
        let working = "line1\nadded_new_2\nadded_new_3\nline3\nline4";
        let diff = compute_four_way_diff(DiffInput {
            path: "src/demo.rs",
            base: Some(base),
            head: Some(base),
            index: Some(base),
            working: Some(working),
            old_path: None,
        });
        let mut files = vec![diff];

        let comments = vec![
            ReviewComment {
                path: "src/demo.rs".into(),
                start_line: Some(2),
                end_line: Some(2),
                side: CommentSide::New,
                body: "On added line 2".into(),
                author: None,
                suggestion: None,
                outdated: false,
            },
            ReviewComment {
                path: "src/demo.rs".into(),
                start_line: Some(2),
                end_line: Some(2),
                side: CommentSide::Old,
                body: "On deleted line 2".into(),
                author: None,
                suggestion: None,
                outdated: false,
            },
            ReviewComment {
                path: "src/demo.rs".into(),
                start_line: Some(2),
                end_line: Some(3),
                side: CommentSide::New,
                body: "On added range 2-3".into(),
                author: None,
                suggestion: None,
                outdated: false,
            },
            ReviewComment {
                path: "src/demo.rs".into(),
                start_line: None,
                end_line: None,
                side: CommentSide::New,
                body: "File-level note".into(),
                author: None,
                suggestion: None,
                outdated: false,
            },
            ReviewComment {
                path: "src/demo.rs".into(),
                start_line: Some(999),
                end_line: Some(999),
                side: CommentSide::New,
                body: "Out-of-diff line 999".into(),
                author: None,
                suggestion: None,
                outdated: false,
            },
            ReviewComment {
                path: "src/untouched.rs".into(),
                start_line: Some(12),
                end_line: Some(12),
                side: CommentSide::New,
                body: "Comment on file not in diff".into(),
                author: None,
                suggestion: None,
                outdated: false,
            },
        ];

        ensure_commented_files(&mut files, &comments);
        assert_eq!(files.len(), 2, "untouched file should get a header stub");

        let all_lines: Vec<DiffLine> = files.iter().flat_map(|f| f.lines.clone()).collect();
        let anchored = anchor_comments(&all_lines, &comments);

        // Header of src/demo.rs is at index 0: gets file-level + out-of-diff line 999
        let header_comments = anchored.for_line(0).unwrap();
        assert_eq!(header_comments.len(), 2);
        assert!(!header_comments[0].outdated);
        assert_eq!(header_comments[0].header_label(), "file-level");
        assert!(header_comments[1].outdated);
        assert_eq!(header_comments[1].header_label(), "new line 999, outside diff");

        // Deleted line `deleted_old_2`
        let del_idx = all_lines.iter().position(|l| l.content == "deleted_old_2").unwrap();
        let del_comments = anchored.for_line(del_idx).unwrap();
        assert_eq!(del_comments.len(), 1);
        assert!(!del_comments[0].outdated);
        assert_eq!(del_comments[0].header_label(), "old line 2");

        // Added line `added_new_2`
        let add2_idx = all_lines.iter().position(|l| l.content == "added_new_2").unwrap();
        let add2_comments = anchored.for_line(add2_idx).unwrap();
        assert_eq!(add2_comments.len(), 1);
        assert_eq!(add2_comments[0].comment.body, "On added line 2");

        // Added line `added_new_3` (end of range 2-3)
        let add3_idx = all_lines.iter().position(|l| l.content == "added_new_3").unwrap();
        let add3_comments = anchored.for_line(add3_idx).unwrap();
        assert_eq!(add3_comments.len(), 1);
        assert_eq!(add3_comments[0].header_label(), "new lines 2-3");

        // Header of src/untouched.rs
        let untouched_idx = all_lines
            .iter()
            .position(|l| l.source == LineSource::FileHeader && l.content == "src/untouched.rs")
            .unwrap();
        let untouched_comments = anchored.for_line(untouched_idx).unwrap();
        assert_eq!(untouched_comments.len(), 1);
        assert!(untouched_comments[0].outdated);
        assert_eq!(untouched_comments[0].header_label(), "new line 12, outside diff");
    }

    #[test]
    fn test_autodetect_and_explicit_override() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".jj")).unwrap();
        std::fs::write(
            root.join(".jj/comments.jsonl"),
            r#"{"path":"a.rs","line":1,"body":"from jj autodetect"}"#,
        )
        .unwrap();

        let detected = detect_comments_file(root).unwrap();
        assert_eq!(detected, root.join(".jj/comments.jsonl"));

        let auto_loaded = load_comments_with_stdin(None, root, None).unwrap();
        assert_eq!(auto_loaded.len(), 1);
        assert_eq!(auto_loaded[0].body, "from jj autodetect");

        let explicit_path = root.join("custom.txt");
        std::fs::write(&explicit_path, "## b.rs:5 (+)\nfrom explicit file\n").unwrap();
        let explicit_loaded = load_comments_with_stdin(Some(&explicit_path), root, None).unwrap();
        assert_eq!(explicit_loaded.len(), 1);
        assert_eq!(explicit_loaded[0].path, "b.rs");
        assert_eq!(explicit_loaded[0].body, "from explicit file");

        let stdin_loaded = load_comments_with_stdin(
            Some(Path::new("-")),
            root,
            Some("## c.rs (file-level)\nfrom stdin\n"),
        )
        .unwrap();
        assert_eq!(stdin_loaded.len(), 1);
        assert_eq!(stdin_loaded[0].path, "c.rs");
        assert_eq!(stdin_loaded[0].body, "from stdin");
    }

    #[test]
    fn a_source_over_the_cap_is_an_error_and_one_at_the_cap_is_read() {
        let at = vec![b' '; MAX_COMMENTS_BYTES];
        assert_eq!(read_capped(&at[..], "x").unwrap().len(), MAX_COMMENTS_BYTES);
        let over = vec![b' '; MAX_COMMENTS_BYTES + 1];
        assert_eq!(
            read_capped(&over[..], "x"),
            Err(CommentError::Io("x: larger than 16 MiB".into()))
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.txt");
        std::fs::write(&path, &over).unwrap();
        let err = load_comments_with_stdin(Some(&path), dir.path(), None).unwrap_err();
        assert!(err.to_string().ends_with("larger than 16 MiB"), "{err}");
        std::fs::write(&path, b"\xff").unwrap();
        let err = read_comments_file(&path).unwrap_err();
        assert!(err.to_string().ends_with("not valid UTF-8"), "{err}");
    }

    #[test]
    fn the_json_suggestion_field_is_read_and_written_back() {
        let input = r#"{"path":"a.rs","line":2,"body":"Use this","suggestion":"fn a() {\n}"}"#;
        let comments = parse_comments(input).unwrap();
        assert_eq!(comments[0].suggestion.as_deref(), Some("fn a() {\n}"));
        let jsonl = to_jsonl(&comments);
        assert!(jsonl.contains(r#""suggestion":"fn a() {\n}""#), "{jsonl}");
        assert_eq!(parse_comments(&jsonl).unwrap(), comments);
        let none = parse_comments(r#"{"path":"a.rs","line":2,"body":"x"}"#).unwrap();
        assert_eq!(none[0].suggestion, None);
        assert!(!to_jsonl(&none).contains("suggestion"));
    }

    #[test]
    fn an_empty_json_suggestion_proposes_deleting_the_lines() {
        let comments =
            parse_comments(r#"{"path":"a.rs","line":2,"body":"drop it","suggestion":""}"#).unwrap();
        assert_eq!(comments[0].suggestion.as_deref(), Some(""));
        assert_eq!(parse_comments(&to_jsonl(&comments)).unwrap(), comments);
    }

    #[test]
    fn a_github_comment_keeps_its_suggestion_fence_in_the_body() {
        let input = r#"[{"path":"a.rs","line":3,"side":"RIGHT","body":"Try:\n```suggestion\nlet x = 2;\n```","user":{"login":"bob"}}]"#;
        let comments = parse_comments(input).unwrap();
        assert_eq!(comments[0].suggestion, None);
        assert!(suggestion::has_suggestion(&comments[0].body, None));
    }

    /// P822: GitHub sends `line: null` for a comment whose line is gone from the PR head,
    /// keeping only `original_line`, a number from the old revision. It must not
    /// land on whatever line now has that number.
    #[test]
    fn a_github_comment_with_a_null_line_shows_under_the_file_header_outside_the_diff() {
        use crate::diff::{DiffInput, compute_four_way_diff};
        let input = r#"[
          {"path":"a.rs","line":null,"original_line":2,"side":"RIGHT","body":"stale","user":{"login":"amy"}},
          {"path":"a.rs","line":3,"original_line":9,"side":"RIGHT","body":"current","user":{"login":"amy"}}
        ]"#;
        let comments = parse_comments(input).unwrap();
        assert!(comments[0].outdated, "{comments:?}");
        assert_eq!((comments[0].start_line, comments[0].end_line), (Some(2), Some(2)));
        assert!(!comments[1].outdated);
        assert_eq!((comments[1].start_line, comments[1].end_line), (Some(3), Some(3)));

        let base = "one\ntwo\nthree\n";
        let diff = compute_four_way_diff(DiffInput {
            path: "a.rs",
            base: Some(base),
            head: Some(base),
            index: Some(base),
            working: Some("one\nTWO\nthree\n"),
            old_path: None,
        });
        let anchored = anchor_comments(&diff.lines, &comments);
        let header = anchored.for_line(0).expect("stale comment sits under the file header");
        assert_eq!(header.len(), 1);
        assert!(header[0].outdated);
        assert_eq!(header[0].header_label(), "@amy · new line 2, outside diff");
        let on_line = (1..diff.lines.len()).find_map(|i| anchored.for_line(i)).unwrap();
        assert_eq!(on_line[0].header_label(), "@amy · new line 3");
    }

    #[test]
    fn an_outdated_comment_survives_the_jsonl_roundtrip() {
        let comments = vec![ReviewComment {
            path: "a.rs".into(),
            start_line: Some(4),
            end_line: Some(6),
            side: CommentSide::Old,
            body: "was here".into(),
            author: None,
            suggestion: None,
            outdated: true,
        }];
        let jsonl = to_jsonl(&comments);
        assert!(jsonl.contains(r#""outdated":true"#), "{jsonl}");
        assert_eq!(parse_comments(&jsonl).unwrap(), comments);
        assert!(
            !to_jsonl(&[ReviewComment { outdated: false, ..comments[0].clone() }])
                .contains("outdated")
        );
    }

    #[test]
    fn only_a_comments_file_is_watched() {
        let path = std::path::PathBuf::from("/r/.branchdiff-comments.jsonl");
        assert_eq!(CommentsSource::Auto(Some(path.clone())).watch_path(), Some(path.as_path()));
        assert_eq!(CommentsSource::Explicit(path.clone()).watch_path(), Some(path.as_path()));
        assert_eq!(CommentsSource::Auto(None).watch_path(), None);
        assert_eq!(CommentsSource::Stdin.watch_path(), None);
    }

    #[test]
    fn to_json_line_is_the_jsonl_encoding() {
        let comment = ReviewComment {
            path: "a.rs".into(),
            start_line: Some(2),
            end_line: Some(3),
            side: CommentSide::Old,
            body: "why".into(),
            author: Some("eve".into()),
            suggestion: Some("x".into()),
            outdated: false,
        };
        assert_eq!(comment.to_json_line(), comment_to_json_line(&comment));
        assert!(comment.to_json_line().contains("\"why\""), "{}", comment.to_json_line());
    }

    fn plain_comment(author: Option<&str>) -> AnchoredComment {
        AnchoredComment {
            comment: ReviewComment {
                path: "a.rs".into(),
                start_line: Some(2),
                end_line: Some(2),
                side: CommentSide::New,
                body: "why".into(),
                author: author.map(Into::into),
                suggestion: None,
                outdated: false,
            },
            outdated: false,
            replaced: Vec::new(),
        }
    }

    #[test]
    fn an_empty_author_is_left_out_of_the_header() {
        let anchored = plain_comment(Some(""));
        assert_eq!(anchored.header_label(), anchored.location_label());
        assert!(!anchored.header_label().contains('@'));
    }

    #[test]
    fn anchored_comments_know_when_they_hold_something() {
        let mut anchored = AnchoredComments::default();
        assert!(anchored.is_empty());
        assert_eq!(anchored.rendered_height_for_line(3, 80), 0);
        anchored.by_line.insert(3, vec![plain_comment(None)]);
        assert!(!anchored.is_empty());
        assert!(anchored.rendered_height_for_line(3, 80) > 0);
        assert_eq!(anchored.rendered_height_for_line(4, 80), 0);
    }

    #[test]
    fn only_the_autodetected_locations_count_as_default() {
        let root = Path::new("/repo");
        assert!(is_default_comment_path(root, Path::new("/repo/.jj/comments.jsonl")));
        assert!(is_default_comment_path(root, Path::new("/repo/.git/comments")));
        assert!(!is_default_comment_path(root, Path::new("/repo/review.jsonl")));
        assert!(!is_default_comment_path(root, Path::new("/other/.jj/comments.jsonl")));
    }

    #[test]
    fn a_dash_reads_stdin_and_any_other_path_is_used_as_given() {
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(resolve_comments_file(Some(Path::new("-")), empty.path()), None);
        assert_eq!(
            resolve_comments_file(Some(Path::new("r.jsonl")), empty.path()),
            Some(PathBuf::from("r.jsonl"))
        );
        assert_eq!(resolve_comments_file(None, empty.path()), None);
        std::fs::create_dir(empty.path().join(".jj")).unwrap();
        std::fs::write(empty.path().join(".jj/comments.jsonl"), "").unwrap();
        assert_eq!(
            resolve_comments_file(None, empty.path()),
            Some(empty.path().join(".jj/comments.jsonl"))
        );
    }
}
