use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::diff::{compute_file_diff_v2, DiffLine, FileDiff, LineSource};
use crate::git;

/// Application state
pub struct App {
    /// Path to the git repository root
    pub repo_path: PathBuf,
    /// The base branch (main or master)
    pub base_branch: String,
    /// The merge-base commit
    pub merge_base: String,
    /// Current branch name (if any)
    pub current_branch: Option<String>,
    /// All file diffs
    pub files: Vec<FileDiff>,
    /// Flattened lines for display
    pub lines: Vec<DiffLine>,
    /// Current scroll offset
    pub scroll_offset: usize,
    /// Viewport height (set during rendering)
    pub viewport_height: usize,
    /// Error message to display (if any)
    pub error: Option<String>,
}

impl App {
    /// Create a new App instance
    pub fn new(repo_path: PathBuf) -> Result<Self> {
        let base_branch = git::detect_base_branch(&repo_path)
            .unwrap_or_else(|_| "main".to_string());

        let merge_base = git::get_merge_base(&repo_path, &base_branch)
            .unwrap_or_default();

        let current_branch = git::get_current_branch(&repo_path)
            .unwrap_or(None);

        let mut app = Self {
            repo_path,
            base_branch,
            merge_base,
            current_branch,
            files: Vec::new(),
            lines: Vec::new(),
            scroll_offset: 0,
            viewport_height: 20, // Default, will be updated during render
            error: None,
        };

        app.refresh()?;
        Ok(app)
    }

    /// Refresh all diffs from git
    pub fn refresh(&mut self) -> Result<()> {
        self.error = None;

        // Update merge-base (might have changed if branch was rebased)
        self.merge_base = git::get_merge_base(&self.repo_path, &self.base_branch)
            .unwrap_or_default();

        // Get all changed files
        let changed_files = git::get_all_changed_files(&self.repo_path, &self.merge_base)
            .context("Failed to get changed files")?;

        self.files.clear();
        self.lines.clear();

        for file in changed_files {
            // Skip binary files
            if git::is_binary_file(&self.repo_path, &file.path) {
                self.lines.push(DiffLine::file_header(&file.path));
                self.lines.push(DiffLine::new(
                    LineSource::Base,
                    "[binary file]".to_string(),
                    ' ',
                    None,
                ));
                continue;
            }

            // Get content at each state
            let base_content = if self.merge_base.is_empty() {
                None
            } else {
                git::get_file_at_ref(&self.repo_path, &file.path, &self.merge_base)
                    .ok()
                    .flatten()
            };

            let head_content = git::get_file_at_ref(&self.repo_path, &file.path, "HEAD")
                .ok()
                .flatten();

            // Index content: use empty string as ref for staged content
            let index_content = git::get_file_at_ref(&self.repo_path, &file.path, "")
                .ok()
                .flatten();

            let working_content = git::get_working_tree_file(&self.repo_path, &file.path)
                .ok()
                .flatten();

            // Compute the diff
            let file_diff = compute_file_diff_v2(
                &file.path,
                base_content.as_deref(),
                head_content.as_deref(),
                index_content.as_deref(),
                working_content.as_deref(),
            );

            // Add to flattened lines
            for line in &file_diff.lines {
                self.lines.push(line.clone());
            }

            // Add empty line between files
            self.lines.push(DiffLine::new(LineSource::Base, String::new(), ' ', None));

            self.files.push(file_diff);
        }

        // Ensure scroll offset is valid
        self.clamp_scroll();

        Ok(())
    }

    /// Scroll up by n lines
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    /// Scroll down by n lines
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
        self.clamp_scroll();
    }

    /// Page up
    pub fn page_up(&mut self) {
        let page_size = self.viewport_height.saturating_sub(2);
        self.scroll_up(page_size);
    }

    /// Page down
    pub fn page_down(&mut self) {
        let page_size = self.viewport_height.saturating_sub(2);
        self.scroll_down(page_size);
    }

    /// Go to top
    pub fn go_to_top(&mut self) {
        self.scroll_offset = 0;
    }

    /// Go to bottom
    pub fn go_to_bottom(&mut self) {
        if self.lines.len() > self.viewport_height {
            self.scroll_offset = self.lines.len() - self.viewport_height;
        }
    }

    /// Set viewport height (called during rendering)
    pub fn set_viewport_height(&mut self, height: usize) {
        self.viewport_height = height;
        self.clamp_scroll();
    }

    /// Clamp scroll offset to valid range
    fn clamp_scroll(&mut self) {
        if self.lines.is_empty() {
            self.scroll_offset = 0;
        } else if self.lines.len() <= self.viewport_height {
            self.scroll_offset = 0;
        } else {
            let max_scroll = self.lines.len().saturating_sub(self.viewport_height);
            self.scroll_offset = self.scroll_offset.min(max_scroll);
        }
    }

    /// Get visible lines for current scroll position
    pub fn visible_lines(&self) -> &[DiffLine] {
        let start = self.scroll_offset;
        let end = (start + self.viewport_height).min(self.lines.len());
        &self.lines[start..end]
    }

    /// Get scroll percentage for status bar
    pub fn scroll_percentage(&self) -> u16 {
        if self.lines.is_empty() || self.lines.len() <= self.viewport_height {
            100
        } else {
            let max_scroll = self.lines.len() - self.viewport_height;
            ((self.scroll_offset as f64 / max_scroll as f64) * 100.0) as u16
        }
    }

    /// Get status text
    pub fn status_text(&self) -> String {
        let branch_info = match &self.current_branch {
            Some(b) => format!("{} vs {}", b, self.base_branch),
            None => format!("HEAD vs {}", self.base_branch),
        };

        let file_count = self.files.len();
        let line_count = self.lines.len();

        format!(
            "{} | {} file{} | {} line{} | {}%",
            branch_info,
            file_count,
            if file_count == 1 { "" } else { "s" },
            line_count,
            if line_count == 1 { "" } else { "s" },
            self.scroll_percentage()
        )
    }
}
