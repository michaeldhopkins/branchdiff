//! Shared test utilities for branchdiff tests.
//!
//! This module provides a builder pattern for creating test App instances,
//! eliminating duplication across test modules.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::Result;

use crate::app::{App, ViewMode, ViewState};
use crate::diff::{DiffLine, FileDiff, LineSource};
use crate::gitignore::GitignoreFilter;
use crate::image_diff::ImageCache;
use crate::vcs::{ComparisonContext, RefreshResult, StackPosition, VcsBackend, VcsEventType, VcsWatchPaths};

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
    stack_position: Option<StackPosition>,
    vcs_backend: VcsBackend,
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
            stack_position: None,
            vcs_backend: VcsBackend::Git,
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

    pub fn with_stack_position(mut self, pos: StackPosition) -> Self {
        self.stack_position = Some(pos);
        self
    }

    pub fn with_vcs_backend(mut self, backend: VcsBackend) -> Self {
        self.vcs_backend = backend;
        self
    }

    pub fn build(self) -> App {
        let repo_path = PathBuf::from("/tmp/test");
        let to_label = self.current_branch.unwrap_or_else(|| "HEAD".to_string());
        App {
            gitignore_filter: GitignoreFilter::new(&repo_path),
            repo_path,
            comparison: ComparisonContext {
                from_label: self.base_branch,
                to_label,
                stack_position: self.stack_position,
                vcs_backend: self.vcs_backend,
            },
            base_identifier: "abc123".to_string(),
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

/// Minimal Vcs implementation for unit tests.
///
/// Classifies events using git-style path conventions (`.git/` prefix)
/// and checks for `.git/index.lock` to determine lock state.
/// Only `classify_event`, `is_locked`, `repo_path`, `base_file_bytes`,
/// and `working_file_bytes` are implemented — other methods panic.
pub struct StubVcs {
    repo_path: PathBuf,
}

impl StubVcs {
    pub fn new(repo_path: PathBuf) -> Self {
        Self { repo_path }
    }
}

impl crate::vcs::Vcs for StubVcs {
    fn repo_path(&self) -> &Path { &self.repo_path }

    fn comparison_context(&self) -> Result<ComparisonContext> { unimplemented!() }

    fn refresh(&self, _: &Arc<AtomicBool>) -> Result<RefreshResult> { unimplemented!() }

    fn single_file_diff(&self, _: &str) -> Option<FileDiff> { unimplemented!() }

    fn base_identifier(&self) -> Result<String> { unimplemented!() }

    fn base_file_bytes(&self, _: &str) -> Result<Option<Vec<u8>>> { Ok(None) }

    fn working_file_bytes(&self, _: &str) -> Result<Option<Vec<u8>>> { Ok(None) }

    fn binary_files(&self) -> HashSet<String> { HashSet::new() }

    fn fetch(&self) -> Result<()> { unimplemented!() }

    fn has_conflicts(&self) -> Result<bool> { unimplemented!() }

    fn is_locked(&self) -> bool {
        self.repo_path.join(".git/index.lock").exists()
    }

    fn watch_paths(&self) -> VcsWatchPaths {
        VcsWatchPaths { files: vec![], recursive_dirs: vec![] }
    }

    fn classify_event(&self, path: &Path) -> VcsEventType {
        let relative = path.strip_prefix(&self.repo_path).unwrap_or(path);
        let is_vcs_path = relative.components().next()
            .is_some_and(|c| c.as_os_str() == ".git");

        if !is_vcs_path { return VcsEventType::Source; }

        // Any .lock file signals an external operation
        if relative.extension().is_some_and(|ext| ext == "lock") {
            return VcsEventType::Lock;
        }

        // Only exact .git/HEAD is a revision change
        if relative == Path::new(".git/HEAD") {
            return VcsEventType::RevisionChange;
        }

        let path_str = relative.to_string_lossy();
        if path_str.contains("refs/") {
            VcsEventType::RevisionChange
        } else {
            VcsEventType::Internal
        }
    }

    fn backend(&self) -> VcsBackend { VcsBackend::Git }

    fn current_revision_id(&self) -> Result<String> { Ok("stub_revision".to_string()) }
}
