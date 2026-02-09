//! Shared test utilities for branchdiff tests.
//!
//! This module provides a builder pattern for creating test App instances,
//! eliminating duplication across test modules.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::app::{App, ViewMode, ViewState};
use crate::diff::{DiffLine, FileDiff, LineSource};
use crate::gitignore::GitignoreFilter;
use crate::image_diff::ImageCache;

/// Builder for creating test App instances with sensible defaults.
///
/// # Example
/// ```ignore
/// let app = TestAppBuilder::new()
///     .with_lines(vec![base_line("hello")])
///     .with_viewport_height(20)
///     .build();
/// ```
pub struct TestAppBuilder {
    lines: Vec<DiffLine>,
    files: Vec<FileDiff>,
    viewport_height: usize,
    view_mode: ViewMode,
    scroll_offset: usize,
    base_branch: String,
    current_branch: Option<String>,
}

impl Default for TestAppBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TestAppBuilder {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            files: Vec::new(),
            viewport_height: 10,
            view_mode: ViewMode::Full,
            scroll_offset: 0,
            base_branch: "main".to_string(),
            current_branch: Some("feature".to_string()),
        }
    }

    pub fn with_lines(mut self, lines: Vec<DiffLine>) -> Self {
        self.lines = lines;
        self
    }

    pub fn with_files(mut self, files: Vec<FileDiff>) -> Self {
        self.lines = files.iter().flat_map(|f| f.lines.clone()).collect();
        self.files = files;
        self
    }

    pub fn with_viewport_height(mut self, height: usize) -> Self {
        self.viewport_height = height;
        self
    }

    pub fn with_view_mode(mut self, mode: ViewMode) -> Self {
        self.view_mode = mode;
        self
    }

    pub fn with_scroll_offset(mut self, offset: usize) -> Self {
        self.scroll_offset = offset;
        self
    }

    pub fn with_base_branch(mut self, branch: &str) -> Self {
        self.base_branch = branch.to_string();
        self
    }

    pub fn with_current_branch(mut self, branch: Option<&str>) -> Self {
        self.current_branch = branch.map(|s| s.to_string());
        self
    }

    pub fn build(self) -> App {
        let repo_path = PathBuf::from("/tmp/test");
        App {
            gitignore_filter: GitignoreFilter::new(&repo_path),
            repo_path,
            base_branch: self.base_branch,
            merge_base: "abc123".to_string(),
            current_branch: self.current_branch,
            files: self.files,
            lines: self.lines,
            error: None,
            conflict_warning: None,
            performance_warning: None,
            file_links: HashMap::new(),
            image_cache: ImageCache::new(),
            image_picker: None,
            font_size: (crate::image_diff::FONT_WIDTH_PX as u16, crate::image_diff::FONT_HEIGHT_PX as u16),
            view: ViewState {
                scroll_offset: self.scroll_offset,
                viewport_height: self.viewport_height,
                view_mode: self.view_mode,
                content_offset: (1, 1),
                line_num_width: 0,
                content_width: 80,
                panel_width: 80,
                show_help: false,
                selection: None,
                word_selection_anchor: None,
                line_selection_anchor: None,
                row_map: Vec::new(),
                collapsed_files: Default::default(),
                manually_toggled: Default::default(),
                needs_inline_spans: true,
                path_copied_at: None,
                last_click: None,
            },
        }
    }
}

/// Create a base (context) line for testing.
pub fn base_line(content: &str) -> DiffLine {
    DiffLine::new(LineSource::Base, content.to_string(), ' ', None)
}

/// Create a committed change line for testing.
pub fn change_line(content: &str) -> DiffLine {
    DiffLine::new(LineSource::Committed, content.to_string(), '+', None)
}

/// Create a staged change line for testing.
pub fn staged_line(content: &str) -> DiffLine {
    DiffLine::new(LineSource::Staged, content.to_string(), '+', None)
}

/// Create an unstaged change line for testing.
pub fn unstaged_line(content: &str) -> DiffLine {
    DiffLine::new(LineSource::Unstaged, content.to_string(), '+', None)
}

/// Create a deletion line for testing.
pub fn deletion_line(content: &str) -> DiffLine {
    DiffLine::new(LineSource::DeletedBase, content.to_string(), '-', None)
}

/// Generate a sequence of base lines for padding in tests.
pub fn base_lines(count: usize) -> Vec<DiffLine> {
    (0..count).map(|i| base_line(&format!("line{}", i))).collect()
}
