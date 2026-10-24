use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::comments::{
    CommentError, CommentsSource, ReviewComment, anchor_comments, detect_comments_file,
    ensure_commented_files, is_default_comment_path, load_comments,
};

use super::App;

impl App {
    /// Install review comments and anchor them against the current diff.
    pub fn set_comments(&mut self, comments: Vec<ReviewComment>) {
        self.view.raw_comments = comments;
        self.sync_commented_files_and_anchor();
    }

    /// Load review comments from `explicit` (`"-"` for stdin) or the default
    /// repository comments location, anchoring them to the current diff.
    pub fn load_comments_from(&mut self, explicit: Option<&Path>) -> Result<(), CommentError> {
        self.view.comments_source = match explicit {
            Some(p) if p == Path::new("-") => CommentsSource::Stdin,
            Some(p) => CommentsSource::Explicit(p.to_path_buf()),
            None => CommentsSource::Auto(detect_comments_file(&self.repo_path)),
        };
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
            },
            ReviewComment {
                path: "src/b.rs".into(),
                start_line: Some(10),
                end_line: Some(10),
                side: CommentSide::New,
                body: "untouched file".into(),
                author: None,
                suggestion: None,
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
}
