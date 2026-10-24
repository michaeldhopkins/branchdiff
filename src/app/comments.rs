use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::comments::{
    CommentError, CommentsSource, ReviewComment, anchor_comments, detect_comments_file,
    ensure_commented_files, is_default_comment_path, load_comments,
};

use super::App;

fn comments_source_for(explicit: Option<&Path>, repo_path: &Path) -> CommentsSource {
    match explicit {
        Some(p) if p == Path::new("-") => CommentsSource::Stdin,
        Some(p) => CommentsSource::Explicit(p.to_path_buf()),
        None => CommentsSource::Auto(detect_comments_file(repo_path)),
    }
}

impl App {
    /// Install review comments and anchor them against the current diff.
    pub fn set_comments(&mut self, comments: Vec<ReviewComment>) {
        self.view.raw_comments = comments;
        self.sync_commented_files_and_anchor();
    }

    /// Load review comments from `explicit` (`"-"` for stdin) or the default
    /// repository comments location, anchoring them to the current diff.
    pub fn load_comments_from(&mut self, explicit: Option<&Path>) -> Result<(), CommentError> {
        self.view.comments_source = comments_source_for(explicit, &self.repo_path);
        let comments = load_comments(explicit, &self.repo_path)?;
        self.set_comments(comments);
        Ok(())
    }

    /// Reload comments from disk if a comments file is active or newly created.
    /// Returns `true` when the loaded comments changed.
    pub fn reload_comments_from_disk(&mut self) -> bool {
        let next_path = match &self.view.comments_source {
            CommentsSource::Stdin => return false,
            CommentsSource::Explicit(path) => Some(path.clone()),
            CommentsSource::Auto(_) => {
                let detected = detect_comments_file(&self.repo_path);
                self.view.comments_source = CommentsSource::Auto(detected.clone());
                detected
            }
        };

        let next_comments = match next_path {
            Some(path) => match crate::comments::read_comments_file(&path)
                .ok()
                .and_then(|text| crate::comments::parse_comments(&text).ok())
            {
                Some(parsed) => parsed,
                None => return false,
            },
            None => Vec::new(),
        };

        if next_comments == self.view.raw_comments {
            return false;
        }
        self.set_comments(next_comments);
        true
    }

    /// The `--diff` patch, with each file's review comments before its patch.
    pub fn patch_with_comments(&self) -> String {
        crate::comments::patch::generate_patch_with_comments(self.lines(), &self.view.comments)
    }

    /// Check whether any changed filesystem path touches the configured or default
    /// comments file.
    pub fn should_reload_comments_for(&self, paths: &HashSet<&PathBuf>) -> bool {
        for path in paths {
            if let Some(src) = self.view.comments_source.watch_path()
                && (*path == src || self.repo_path.join(src) == **path)
            {
                return true;
            }
            if matches!(self.view.comments_source, CommentsSource::Auto(_))
                && is_default_comment_path(&self.repo_path, path)
            {
                return true;
            }
        }
        false
    }

    /// Ensure files referenced by comments exist in `self.files` and re-anchor
    /// `self.view.comments` to `self.lines`.
    pub(crate) fn sync_commented_files_and_anchor(&mut self) {
        if self.view.raw_comments.is_empty() && self.view.comments.is_empty() {
            return;
        }
        let prev_len = self.files.len();
        ensure_commented_files(&mut self.files, &self.view.raw_comments);
        if self.files.len() != prev_len {
            self.regenerate_lines();
        }
        self.view.comments = anchor_comments(self.lines(), &self.view.raw_comments);
        self.view.needs_inline_spans = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comments::CommentSide;
    use crate::diff::{DiffLine, FileDiff, LineSource};
    use crate::test_support::TestAppBuilder;

    #[test]
    fn set_comments_anchors_and_adds_untouched_files() {
        let file = FileDiff::new(vec![
            DiffLine::file_header("src/a.rs"),
            DiffLine::new(LineSource::Committed, "let x = 1;".into(), '+', Some(1))
                .with_file_path("src/a.rs"),
        ]);
        let mut app = TestAppBuilder::new().with_files(vec![file]).build();
        app.set_comments(vec![
            ReviewComment {
                path: "src/a.rs".into(),
                start_line: Some(1),
                end_line: Some(1),
                side: CommentSide::New,
                body: "on line 1".into(),
                author: None,
                suggestion: None,
                outdated: false,
            },
            ReviewComment {
                path: "src/b.rs".into(),
                start_line: Some(10),
                end_line: Some(10),
                side: CommentSide::New,
                body: "untouched file".into(),
                author: None,
                suggestion: None,
                outdated: false,
            },
        ]);

        assert_eq!(app.files.len(), 2);
        assert!(app.view.comments.for_line(1).is_some());
    }

    #[test]
    fn reload_comments_from_disk_detects_changes() {
        let dir = tempfile::tempdir().unwrap();
        let jj_dir = dir.path().join(".jj");
        std::fs::create_dir_all(&jj_dir).unwrap();
        let comments_file = jj_dir.join("comments.jsonl");
        std::fs::write(&comments_file, r#"{"path":"a.rs","line":1,"body":"initial"}"#).unwrap();

        let file = FileDiff::new(vec![
            DiffLine::file_header("a.rs"),
            DiffLine::new(LineSource::Committed, "fn a() {}".into(), '+', Some(1))
                .with_file_path("a.rs"),
        ]);
        let mut app = TestAppBuilder::new().with_files(vec![file]).build();
        app.repo_path = dir.path().to_path_buf();
        app.load_comments_from(None).unwrap();
        assert_eq!(app.view.raw_comments.len(), 1);
        assert!(!app.reload_comments_from_disk(), "unchanged file should not reload");

        std::fs::write(&comments_file, r#"{"path":"a.rs","line":1,"body":"updated"}"#).unwrap();
        let mut paths = HashSet::new();
        paths.insert(&comments_file);
        assert!(app.should_reload_comments_for(&paths));
        assert!(app.reload_comments_from_disk(), "updated file should reload");
        assert_eq!(app.view.raw_comments[0].body, "updated");
    }

    fn review_comment(path: &str, line: usize, body: &str) -> ReviewComment {
        ReviewComment {
            path: path.into(),
            start_line: Some(line),
            end_line: Some(line),
            side: CommentSide::New,
            body: body.into(),
            author: None,
            suggestion: None,
            outdated: false,
        }
    }

    fn one_file_app() -> crate::app::App {
        let file = FileDiff::new(vec![
            DiffLine::file_header("src/a.rs"),
            DiffLine::new(LineSource::Committed, "let x = 1;".into(), '+', Some(1))
                .with_file_path("src/a.rs"),
        ]);
        TestAppBuilder::new().with_files(vec![file]).build()
    }

    #[test]
    fn comments_source_is_stdin_only_for_a_dash() {
        let repo = Path::new("/nonexistent-repo");
        assert_eq!(comments_source_for(Some(Path::new("-")), repo), CommentsSource::Stdin);
        assert_eq!(
            comments_source_for(Some(Path::new("review.jsonl")), repo),
            CommentsSource::Explicit(PathBuf::from("review.jsonl"))
        );
        assert_eq!(comments_source_for(None, repo), CommentsSource::Auto(None));
    }

    #[test]
    fn load_comments_from_a_file_records_an_explicit_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review.jsonl");
        std::fs::write(&path, r#"{"path":"src/a.rs","line":1,"body":"hi"}"#).unwrap();
        let mut app = one_file_app();
        app.load_comments_from(Some(&path)).unwrap();
        assert_eq!(app.view.comments_source, CommentsSource::Explicit(path));
        assert_eq!(app.view.raw_comments.len(), 1);
    }

    #[test]
    fn patch_with_comments_writes_comments_before_the_patch() {
        let mut app = one_file_app();
        app.set_comments(vec![review_comment("src/a.rs", 1, "check this")]);
        let patch = app.patch_with_comments();
        assert_eq!(
            patch,
            crate::comments::patch::generate_patch_with_comments(app.lines(), &app.view.comments)
        );
        let comment_at = patch.find("# │ check this").expect(&patch);
        let diff_at = patch.find("diff --git").expect(&patch);
        assert!(comment_at < diff_at, "{patch}");
    }

    #[test]
    fn should_reload_comments_for_matches_only_the_active_comments_file() {
        let repo = PathBuf::from("/repo");
        let mut app = one_file_app();
        app.repo_path = repo.clone();
        let relative = PathBuf::from("notes/review.jsonl");
        app.view.comments_source = CommentsSource::Explicit(relative.clone());

        let joined = repo.join(&relative);
        let unrelated = repo.join("src/main.rs");
        let check = |app: &crate::app::App, p: &PathBuf| {
            app.should_reload_comments_for(&HashSet::from([p]))
        };
        assert!(check(&app, &relative), "the path exactly as configured");
        assert!(check(&app, &joined), "the configured path under the repository");
        assert!(!check(&app, &unrelated), "an unrelated file");
        assert!(
            !check(&app, &repo.join(".jj/comments.jsonl")),
            "an explicit source ignores defaults"
        );

        app.view.comments_source = CommentsSource::Explicit(joined.clone());
        assert!(check(&app, &joined), "an absolute configured path");
        assert!(!check(&app, &unrelated));
    }

    #[test]
    fn should_reload_comments_for_auto_source_watches_default_locations() {
        let repo = PathBuf::from("/repo");
        let mut app = one_file_app();
        app.repo_path = repo.clone();
        app.view.comments_source = CommentsSource::Auto(None);
        let check = |app: &crate::app::App, p: &PathBuf| {
            app.should_reload_comments_for(&HashSet::from([p]))
        };
        assert!(check(&app, &repo.join(".jj/comments.jsonl")));
        assert!(!check(&app, &repo.join("src/main.rs")));

        app.view.comments_source = CommentsSource::Stdin;

        assert!(!check(&app, &repo.join(".jj/comments.jsonl")));
    }

    #[test]
    fn comments_on_untouched_files_get_a_header_line_in_the_diff() {
        let mut app = one_file_app();
        app.set_comments(vec![review_comment("src/b.rs", 10, "untouched")]);
        let header = app
            .lines()
            .iter()
            .position(|l| l.source == LineSource::FileHeader && l.content == "src/b.rs")
            .expect("header for the commented file is in the visible lines");
        let anchored = app.view.comments.for_line(header).expect("comment sits on that header");
        assert_eq!(anchored[0].comment.body, "untouched");
    }
}
