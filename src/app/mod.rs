//! Application state and logic module for branchdiff

mod frame;
mod navigation;
mod refresh;
mod selection;
mod view_mode;

pub use frame::{DisplayableItem, FrameContext};
pub use refresh::{compute_refresh, compute_single_file_diff, RefreshResult};
pub use selection::{Position, Selection};

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// File patterns that should be collapsed by default (lock files, generated files)
const AUTO_COLLAPSE_PATTERNS: &[&str] = &[
    // Ruby/Rails
    "Gemfile.lock",
    "db/schema.rb",
    "db/structure.sql",
    // JavaScript/Node
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    // Rust
    "Cargo.lock",
    // Python
    "poetry.lock",
    "Pipfile.lock",
    "pdm.lock",
    // PHP
    "composer.lock",
    // .NET
    "packages.lock.json",
    // Go
    "go.sum",
    // Elixir
    "mix.lock",
    // Swift
    "Package.resolved",
    // Dart/Flutter
    "pubspec.lock",
];

use anyhow::Result;

use crate::diff::{DiffLine, FileDiff};
use crate::git;
use crate::gitignore::GitignoreFilter;
use crate::ui::ScreenRowInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    Full,
    #[default]
    Context,
    ChangesOnly,
}

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
    /// Whether to show the help modal
    pub show_help: bool,
    /// Current view mode (Full, Context, or ChangesOnly)
    pub view_mode: ViewMode,
    /// Current text selection (if any)
    pub selection: Option<Selection>,
    /// Anchor for word-based selection (row, word_start_col, word_end_col in screen coords)
    /// When set, dragging extends selection by whole words
    pub word_selection_anchor: Option<(usize, usize, usize)>,
    /// Content area offset (x, y) for coordinate mapping
    pub content_offset: (u16, u16),
    /// Width of line number column (for extracting content without line numbers)
    pub line_num_width: usize,
    /// Available width for content (used for wrapping calculation)
    pub content_width: usize,
    /// Warning message about merge conflicts (if any)
    pub conflict_warning: Option<String>,
    /// Performance warning (large repo or diff)
    pub performance_warning: Option<String>,
    /// Mapping from screen row index to logical line info (set during rendering)
    pub row_map: Vec<ScreenRowInfo>,
    /// Set of collapsed file paths (persists across refreshes)
    pub collapsed_files: HashSet<String>,
    /// Set of files that have been manually toggled (won't be auto-collapsed)
    pub manually_toggled: HashSet<String>,
    /// Track whether inline spans need recomputation for visible lines
    pub needs_inline_spans: bool,
    /// Timestamp when path was last copied (for flash feedback)
    pub path_copied_at: Option<std::time::Instant>,
    /// Last click position and time (for double-click detection)
    pub last_click: Option<(std::time::Instant, u16, u16)>,
    /// Gitignore filter for file change events
    pub gitignore_filter: GitignoreFilter,
}

impl App {
    /// Create an App instance for benchmarking with pre-built lines
    pub fn new_for_bench(lines: Vec<DiffLine>) -> Self {
        let repo_path = PathBuf::from("/bench");
        Self {
            gitignore_filter: GitignoreFilter::new(&repo_path),
            repo_path,
            base_branch: "main".to_string(),
            merge_base: "bench".to_string(),
            current_branch: Some("feature".to_string()),
            files: Vec::new(),
            lines,
            scroll_offset: 0,
            viewport_height: 50,
            error: None,
            show_help: false,
            view_mode: ViewMode::Full,
            selection: None,
            word_selection_anchor: None,
            content_offset: (1, 1),
            line_num_width: 4,
            content_width: 120,
            conflict_warning: None,
            performance_warning: None,
            row_map: Vec::new(),
            collapsed_files: HashSet::new(),
            manually_toggled: HashSet::new(),
            needs_inline_spans: true,
            path_copied_at: None,
            last_click: None,
        }
    }

    /// Create a new App instance
    pub fn new(repo_path: PathBuf) -> Result<Self> {
        let base_branch = git::detect_base_branch(&repo_path)
            .unwrap_or_else(|_| "main".to_string());

        let merge_base = git::get_merge_base_preferring_origin(&repo_path, &base_branch)
            .unwrap_or_default();

        let current_branch = git::get_current_branch(&repo_path)
            .unwrap_or(None);

        let gitignore_filter = GitignoreFilter::new(&repo_path);

        let mut app = Self {
            repo_path,
            base_branch,
            merge_base,
            current_branch,
            files: Vec::new(),
            lines: Vec::new(),
            scroll_offset: 0,
            viewport_height: 20,
            error: None,
            show_help: false,
            view_mode: ViewMode::Context,
            selection: None,
            word_selection_anchor: None,
            content_offset: (1, 1),
            line_num_width: 0,
            content_width: 80,
            conflict_warning: None,
            performance_warning: None,
            row_map: Vec::new(),
            collapsed_files: HashSet::new(),
            manually_toggled: HashSet::new(),
            needs_inline_spans: true,
            path_copied_at: None,
            last_click: None,
            gitignore_filter,
        };

        app.refresh()?;
        Ok(app)
    }

    /// Toggle the collapse state of a file
    pub fn toggle_file_collapsed(&mut self, path: &str) {
        self.manually_toggled.insert(path.to_string());
        if self.collapsed_files.contains(path) {
            self.collapsed_files.remove(path);
        } else {
            self.collapsed_files.insert(path.to_string());
        }
        self.needs_inline_spans = true;
    }

    /// Check if a file is collapsed
    pub fn is_file_collapsed(&self, path: &str) -> bool {
        self.collapsed_files.contains(path)
    }

    /// Check if a file path matches any auto-collapse pattern (lock files)
    fn should_auto_collapse(path: &str) -> bool {
        AUTO_COLLAPSE_PATTERNS.iter().any(|pattern| path.ends_with(pattern))
    }

    /// Check if a file header indicates a deleted file
    fn is_deleted_file(header_content: &str) -> bool {
        header_content.ends_with("(deleted)")
    }

    /// Auto-collapse files matching lock/generated file patterns and deleted files.
    /// Also uncollapse files that were previously collapsed due to deletion but are
    /// no longer deleted (unless they match auto-collapse patterns).
    /// Skips files that have been manually toggled by the user.
    fn auto_collapse_files(&mut self) {
        for file in &self.files {
            if let Some(first_line) = file.lines.first()
                && let Some(ref path) = first_line.file_path
            {
                if self.manually_toggled.contains(path) {
                    continue;
                }

                let is_pattern_match = Self::should_auto_collapse(path);
                let is_deleted = Self::is_deleted_file(&first_line.content);

                if is_pattern_match || is_deleted {
                    self.collapsed_files.insert(path.clone());
                } else if self.collapsed_files.contains(path) {
                    // File was collapsed but is no longer deleted and doesn't match
                    // auto-collapse patterns - uncollapse it
                    self.collapsed_files.remove(path);
                }
            }
        }
    }

    pub fn refresh(&mut self) -> Result<()> {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = compute_refresh(&self.repo_path, &self.base_branch, &cancel_flag)?;
        self.apply_refresh_result(result);
        Ok(())
    }

    pub fn apply_refresh_result(&mut self, result: RefreshResult) {
        self.error = None;
        self.merge_base = result.merge_base;
        self.current_branch = result.current_branch;
        self.files = result.files;
        self.lines = result.lines;
        self.auto_collapse_files();
        self.clamp_scroll();
        self.needs_inline_spans = true;
    }

    pub fn ensure_inline_spans_for_visible(&mut self, visible_height: usize) {
        // In context/changes view, the displayed lines map to different original indices
        // We need to compute inline spans for the actual lines that will be displayed
        match self.view_mode {
            ViewMode::Full => {
                let start = self.scroll_offset;
                let end = (start + visible_height).min(self.lines.len());
                for line in &mut self.lines[start..end] {
                    line.ensure_inline_spans();
                }
            }
            ViewMode::Context => {
                let (_, index_map) = self.build_context_lines_with_mapping();
                let start = self.scroll_offset;
                let end = (start + visible_height).min(index_map.len());
                for &original_idx in index_map[start..end].iter().flatten() {
                    if original_idx < self.lines.len() {
                        self.lines[original_idx].ensure_inline_spans();
                    }
                }
            }
            ViewMode::ChangesOnly => {
                // ChangesOnly only shows lines that are already marked as changes,
                // so we compute inline spans for all lines with old_content
                for line in &mut self.lines {
                    if line.old_content.is_some() {
                        line.ensure_inline_spans();
                    }
                }
            }
        }
    }

    pub fn update_single_file(&mut self, file_path: &str, new_diff: Option<FileDiff>) {
        let existing_idx = self.files.iter().position(|f| {
            f.lines.first()
                .and_then(|l| l.file_path.as_ref())
                .map(|p| p == file_path)
                .unwrap_or(false)
        });

        match (existing_idx, new_diff) {
            (Some(idx), Some(diff)) => {
                self.files[idx] = diff;
            }
            (Some(idx), None) => {
                self.files.remove(idx);
            }
            (None, Some(diff)) => {
                self.files.push(diff);
            }
            (None, None) => {
            }
        }

        self.regenerate_lines();
        self.auto_collapse_files();
        self.clamp_scroll();
        self.needs_inline_spans = true;
    }

    fn regenerate_lines(&mut self) {
        use crate::diff::LineSource;

        self.lines.clear();
        for file in &self.files {
            self.lines.extend(file.lines.iter().cloned());
            self.lines.push(DiffLine::new(LineSource::Base, String::new(), ' ', None));
        }
    }

    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    pub fn should_quit(&mut self) -> bool {
        if self.show_help {
            self.show_help = false;
            false
        } else {
            true
        }
    }

    /// Get the file path of the first visible line
    pub fn current_file(&self) -> Option<String> {
        let items = self.compute_displayable_items();
        let start = self.scroll_offset.min(items.len());
        let end = (start + self.viewport_height).min(items.len());

        for item in &items[start..end] {
            if let DisplayableItem::Line(idx) = item
                && let Some(ref path) = self.lines[*idx].file_path
            {
                return Some(path.clone());
            }
        }
        None
    }

    /// Set content area layout info (called during rendering)
    pub fn set_content_layout(&mut self, offset_x: u16, offset_y: u16, line_num_width: usize, content_width: usize) {
        if self.content_width != content_width {
            self.needs_inline_spans = true;
        }
        self.content_offset = (offset_x, offset_y);
        self.line_num_width = line_num_width;
        self.content_width = content_width;
    }

    /// Check if inline spans need recomputation
    pub fn needs_inline_spans(&self) -> bool {
        self.needs_inline_spans
    }

    /// Clear the needs_inline_spans flag after computation
    pub fn clear_needs_inline_spans(&mut self) {
        self.needs_inline_spans = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffLine, LineSource};
    use crate::test_support::{base_line, change_line, TestAppBuilder};

    /// Helper to get line from DisplayableItem (returns None for Elided)
    fn item_to_line<'a>(app: &'a App, item: &DisplayableItem) -> Option<&'a DiffLine> {
        match item {
            DisplayableItem::Line(idx) => Some(&app.lines[*idx]),
            DisplayableItem::Elided(_) => None,
        }
    }

    /// Helper to collect non-elided lines from displayable items
    fn collect_lines<'a>(app: &'a App, items: &[DisplayableItem]) -> Vec<&'a DiffLine> {
        items.iter().filter_map(|item| item_to_line(app, item)).collect()
    }

    /// Helper to get visible lines using FrameContext
    fn get_visible_lines(app: &App) -> Vec<&DiffLine> {
        let ctx = FrameContext::new(app);
        ctx.iter_visible_items()
            .filter_map(|item| item_to_line(app, item))
            .collect()
    }

    #[test]
    fn test_auto_collapse_lock_files() {
        let gemfile_lock = FileDiff {
            lines: vec![
                DiffLine::file_header("Gemfile.lock"),
                change_line("some lock content"),
            ],
        };
        let regular_file = FileDiff {
            lines: vec![
                DiffLine::file_header("src/main.rs"),
                change_line("some code"),
            ],
        };
        let cargo_lock = FileDiff {
            lines: vec![
                DiffLine::file_header("Cargo.lock"),
                change_line("more lock content"),
            ],
        };

        let mut app = TestAppBuilder::new().with_files(vec![gemfile_lock, regular_file, cargo_lock]).build();

        assert!(!app.is_file_collapsed("Gemfile.lock"));
        assert!(!app.is_file_collapsed("src/main.rs"));
        assert!(!app.is_file_collapsed("Cargo.lock"));

        app.auto_collapse_files();

        assert!(app.is_file_collapsed("Gemfile.lock"), "Gemfile.lock should be auto-collapsed");
        assert!(!app.is_file_collapsed("src/main.rs"), "Regular files should not be collapsed");
        assert!(app.is_file_collapsed("Cargo.lock"), "Cargo.lock should be auto-collapsed");
    }

    #[test]
    fn test_auto_collapse_deleted_files() {
        let deleted_file = FileDiff {
            lines: vec![
                DiffLine::deleted_file_header("src/old_file.rs"),
                change_line("deleted content"),
            ],
        };
        let regular_file = FileDiff {
            lines: vec![
                DiffLine::file_header("src/main.rs"),
                change_line("some code"),
            ],
        };

        let mut app = TestAppBuilder::new().with_files(vec![deleted_file, regular_file]).build();

        assert!(!app.is_file_collapsed("src/old_file.rs"));
        assert!(!app.is_file_collapsed("src/main.rs"));

        app.auto_collapse_files();

        assert!(app.is_file_collapsed("src/old_file.rs"), "deleted file should be auto-collapsed");
        assert!(!app.is_file_collapsed("src/main.rs"), "regular files should not be collapsed");
    }

    #[test]
    fn test_manually_toggled_files_not_auto_collapsed() {
        let gemfile_lock = FileDiff {
            lines: vec![
                DiffLine::file_header("Gemfile.lock"),
                change_line("some lock content"),
            ],
        };

        let mut app = TestAppBuilder::new().with_files(vec![gemfile_lock]).build();

        app.auto_collapse_files();
        assert!(app.is_file_collapsed("Gemfile.lock"), "should be auto-collapsed initially");

        app.toggle_file_collapsed("Gemfile.lock");
        assert!(!app.is_file_collapsed("Gemfile.lock"), "should be expanded after toggle");

        app.auto_collapse_files();
        assert!(!app.is_file_collapsed("Gemfile.lock"), "should stay expanded after re-running auto-collapse");
    }

    #[test]
    fn test_manually_toggled_deleted_files_not_auto_collapsed() {
        let deleted_file = FileDiff {
            lines: vec![
                DiffLine::deleted_file_header("src/old_file.rs"),
                change_line("deleted content"),
            ],
        };

        let mut app = TestAppBuilder::new().with_files(vec![deleted_file]).build();

        app.auto_collapse_files();
        assert!(app.is_file_collapsed("src/old_file.rs"), "should be auto-collapsed initially");

        app.toggle_file_collapsed("src/old_file.rs");
        assert!(!app.is_file_collapsed("src/old_file.rs"), "should be expanded after toggle");

        app.auto_collapse_files();
        assert!(!app.is_file_collapsed("src/old_file.rs"), "should stay expanded after re-running auto-collapse");
    }

    #[test]
    fn test_undeleted_file_uncollapses() {
        // Simulate a file that was deleted (auto-collapsed) then restored
        let deleted_file = FileDiff {
            lines: vec![
                DiffLine::deleted_file_header("src/restored.rs"),
                change_line("content"),
            ],
        };

        let mut app = TestAppBuilder::new().with_files(vec![deleted_file]).build();
        app.auto_collapse_files();
        assert!(app.is_file_collapsed("src/restored.rs"), "deleted file should be collapsed");

        // Simulate the file being restored (no longer deleted)
        let restored_file = FileDiff {
            lines: vec![
                DiffLine::file_header("src/restored.rs"),
                change_line("content"),
            ],
        };
        app.files = vec![restored_file];

        app.auto_collapse_files();
        assert!(!app.is_file_collapsed("src/restored.rs"), "restored file should be uncollapsed");
    }

    #[test]
    fn test_undeleted_lock_file_stays_collapsed() {
        // Lock files should stay collapsed even after being "restored"
        let deleted_lock = FileDiff {
            lines: vec![
                DiffLine::deleted_file_header("Gemfile.lock"),
                change_line("content"),
            ],
        };

        let mut app = TestAppBuilder::new().with_files(vec![deleted_lock]).build();
        app.auto_collapse_files();
        assert!(app.is_file_collapsed("Gemfile.lock"), "deleted lock file should be collapsed");

        // Simulate the lock file being restored
        let restored_lock = FileDiff {
            lines: vec![
                DiffLine::file_header("Gemfile.lock"),
                change_line("content"),
            ],
        };
        app.files = vec![restored_lock];

        app.auto_collapse_files();
        assert!(app.is_file_collapsed("Gemfile.lock"), "lock file should stay collapsed even after restore");
    }

    #[test]
    fn test_should_auto_collapse_patterns() {
        // Lock files should match
        assert!(App::should_auto_collapse("Gemfile.lock"));
        assert!(App::should_auto_collapse("package-lock.json"));
        assert!(App::should_auto_collapse("yarn.lock"));
        assert!(App::should_auto_collapse("Cargo.lock"));
        assert!(App::should_auto_collapse("poetry.lock"));
        assert!(App::should_auto_collapse("go.sum"));

        // Nested paths should also match
        assert!(App::should_auto_collapse("some/path/to/Gemfile.lock"));
        assert!(App::should_auto_collapse("frontend/package-lock.json"));

        // Regular files should not match
        assert!(!App::should_auto_collapse("src/main.rs"));
        assert!(!App::should_auto_collapse("Gemfile"));
        assert!(!App::should_auto_collapse("package.json"));
        assert!(!App::should_auto_collapse("Cargo.toml"));
    }

    #[test]
    fn test_changed_line_count() {
        let lines = vec![
            DiffLine::file_header("test.rs"),
            base_line("context line 1"),
            DiffLine::new(LineSource::Committed, "committed".to_string(), '+', Some(1)),
            DiffLine::new(LineSource::Staged, "staged".to_string(), '+', Some(2)),
            DiffLine::new(LineSource::Unstaged, "unstaged".to_string(), '+', Some(3)),
            base_line("context line 2"),
            DiffLine::new(LineSource::DeletedBase, "deleted from base".to_string(), '-', None),
            DiffLine::new(LineSource::DeletedCommitted, "deleted committed".to_string(), '-', None),
            DiffLine::new(LineSource::DeletedStaged, "deleted staged".to_string(), '-', None),
            base_line("context line 3"),
        ];
        let app = TestAppBuilder::new().with_lines(lines).build();
        assert_eq!(app.changed_line_count(), 6);
    }

    #[test]
    fn test_changes_only_view_filters_base_lines() {
        let lines = vec![
            DiffLine::file_header("test.rs"),
            base_line("context line 1"),
            DiffLine::new(LineSource::Committed, "committed".to_string(), '+', Some(1)),
            base_line("context line 2"),
            DiffLine::new(LineSource::Unstaged, "unstaged".to_string(), '+', Some(2)),
            base_line("context line 3"),
        ];
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view_mode = ViewMode::ChangesOnly;
        let items = app.compute_displayable_items();
        let displayed = collect_lines(&app, &items);
        assert_eq!(displayed.len(), 3);
        assert_eq!(displayed[0].source, LineSource::FileHeader);
        assert_eq!(displayed[1].source, LineSource::Committed);
        assert_eq!(displayed[2].source, LineSource::Unstaged);
    }

    #[test]
    fn test_should_quit_dismisses_help_first() {
        let mut app = TestAppBuilder::new().build();
        assert!(!app.show_help);
        assert!(app.should_quit());

        app.show_help = true;
        assert!(!app.should_quit());
        assert!(!app.show_help);

        assert!(app.should_quit());
    }

    #[test]
    fn test_cycle_view_mode_empty_lines() {
        let mut app = TestAppBuilder::new().build();
        app.cycle_view_mode();
        assert_eq!(app.view_mode, ViewMode::Context);
        assert_eq!(app.scroll_offset, 0);
        app.cycle_view_mode();
        assert_eq!(app.view_mode, ViewMode::ChangesOnly);
        app.cycle_view_mode();
        assert_eq!(app.view_mode, ViewMode::Full);
    }

    #[test]
    fn test_cycle_view_mode_few_lines() {
        let lines = vec![
            base_line("line1"),
            change_line("changed"),
            base_line("line3"),
        ];
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 10;

        app.cycle_view_mode();
        assert_eq!(app.view_mode, ViewMode::Context);
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_toggle_context_anchors_on_middle_line() {
        // Create 30 lines: 10 base, 1 change, 19 base
        // In context mode, only lines around the change are shown
        let mut lines = Vec::new();
        for i in 0..10 {
            lines.push(base_line(&format!("before{}", i)));
        }
        lines.push(change_line("THE CHANGE"));
        for i in 0..19 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 10;

        // Scroll to middle of file (around line 15)
        app.scroll_offset = 10;

        // The middle of viewport is at offset 5, so line 15 in original
        // Toggle to context mode
        app.cycle_view_mode();

        // Should still be showing content near line 15
        // The change is at original index 10, context shows 5 lines around it
        // So visible in context: indices 5-15 of original (lines before5..after4)
        assert_eq!(app.view_mode, ViewMode::Context);
        // Scroll should be adjusted to keep similar content visible
    }

    #[test]
    fn test_toggle_context_when_middle_is_elided() {
        // Create lines where the middle will be elided in context mode
        // 50 base lines, then 1 change at the end
        let mut lines = Vec::new();
        for i in 0..50 {
            lines.push(base_line(&format!("base{}", i)));
        }
        lines.push(change_line("change at end"));

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 10;

        // Scroll to line 20 (far from the change at 50)
        app.scroll_offset = 20;

        // Toggle to context mode - line 25 (middle) will be elided
        app.cycle_view_mode();

        // Should find closest visible line and anchor there
        assert_eq!(app.view_mode, ViewMode::Context);
        // The only visible content is around line 50, so scroll should jump there
    }

    #[test]
    fn test_toggle_context_round_trip_near_change() {
        // Toggling twice should return to approximately the same position
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(base_line(&format!("before{}", i)));
        }
        lines.push(change_line("THE CHANGE"));
        for i in 0..20 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 10;

        // Position so the change is visible (change is at index 20)
        app.scroll_offset = 16; // Middle at 21, close to change

        // Cycle through all three modes back to Full
        app.cycle_view_mode(); // Full -> Context
        assert_eq!(app.view_mode, ViewMode::Context);
        app.cycle_view_mode(); // Context -> ChangesOnly
        assert_eq!(app.view_mode, ViewMode::ChangesOnly);
        app.cycle_view_mode(); // ChangesOnly -> Full
        assert_eq!(app.view_mode, ViewMode::Full);
    }

    #[test]
    fn test_toggle_context_at_top() {
        let mut lines = Vec::new();
        lines.push(change_line("change at top"));
        for i in 0..30 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 10;
        app.scroll_offset = 0;

        app.cycle_view_mode();

        // Should stay near top since change is at top
        assert_eq!(app.view_mode, ViewMode::Context);
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_toggle_context_at_bottom() {
        let mut lines = Vec::new();
        for i in 0..30 {
            lines.push(base_line(&format!("before{}", i)));
        }
        lines.push(change_line("change at bottom"));

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 10;

        // Scroll to bottom
        app.go_to_bottom();

        app.cycle_view_mode();

        // Should stay near bottom content
        assert_eq!(app.view_mode, ViewMode::Context);
    }

    #[test]
    fn test_find_position_for_visible_line() {
        let mut lines = Vec::new();
        for i in 0..5 {
            lines.push(base_line(&format!("before{}", i)));
        }
        lines.push(change_line("change"));
        for i in 0..5 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view_mode = ViewMode::Context;

        // The change is at original index 5
        // In context mode with 5 lines of context, indices 0-10 are visible
        let pos = app.find_position_for_original_index(5);

        // Position should be valid and map back to the change
        let (_, index_map) = app.build_context_lines_with_mapping();
        assert!(pos < index_map.len());
        assert_eq!(index_map[pos], Some(5));
    }

    #[test]
    fn test_find_position_for_elided_line() {
        // Create scenario where some lines are elided
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(base_line(&format!("start{}", i)));
        }
        lines.push(change_line("change"));
        for i in 0..20 {
            lines.push(base_line(&format!("end{}", i)));
        }

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view_mode = ViewMode::Context;

        // Original index 0 is far from change at 20, so it's elided
        // Should find closest visible line
        let pos = app.find_position_for_original_index(0);

        // Should return a valid position
        let (filtered, _) = app.build_context_lines_with_mapping();
        assert!(pos < filtered.len());
    }

    #[test]
    fn test_context_view_shows_lines_with_inline_spans() {
        // REGRESSION TEST: Lines with inline spans (merged modifications) should be
        // visible in context view, even if their source is Base.
        //
        // A merged modification line has source=Base but contains inline_spans
        // showing what changed. These should be treated as "interesting" lines.
        use crate::diff::InlineSpan;

        let mut lines = Vec::new();

        // Add many base lines before
        for i in 0..20 {
            lines.push(base_line(&format!("before{}", i)));
        }

        // Add a line with inline spans (merged modification)
        // This simulates: "commercial_renewal.name" -> "bond.name"
        let mut merged_line = DiffLine::new(
            LineSource::Base,  // Source is Base for merged lines
            "bond.name".to_string(),
            ' ',
            Some(21),
        );
        merged_line.inline_spans = vec![
            InlineSpan {
                text: "commercial_renewal".to_string(),
                source: Some(LineSource::DeletedBase),
                is_deletion: true,
            },
            InlineSpan {
                text: "bond".to_string(),
                source: Some(LineSource::Committed),
                is_deletion: false,
            },
            InlineSpan {
                text: ".name".to_string(),
                source: None,
                is_deletion: false,
            },
        ];
        lines.push(merged_line);

        // Add many base lines after
        for i in 0..20 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view_mode = ViewMode::Context;

        // Get the filtered items in context mode
        let items = app.compute_displayable_items();
        let filtered = collect_lines(&app, &items);

        // The line with inline spans should be visible
        let has_merged_line = filtered.iter().any(|l| l.content == "bond.name");
        assert!(has_merged_line,
            "Line with inline spans should be visible in context view. \
             Filtered lines: {:?}",
            filtered.iter().map(|l| &l.content).collect::<Vec<_>>());

        // Should also have context lines around it (not just the change)
        assert!(filtered.len() > 1,
            "Should have context lines around the merged line");
    }

    #[test]
    fn test_context_view_shows_trailing_base_lines_after_change() {
        // REGRESSION TEST: Trailing base lines after a change should be visible
        // in context view as trailing context.
        //
        // Scenario:
        // - Many base lines before
        // - A committed change (addition)
        // - 2 base lines after (end, end)
        //
        // The trailing base lines should appear as context.

        let mut lines = Vec::new();

        // Add many base lines before
        for i in 0..20 {
            lines.push(base_line(&format!("before{}", i)));
        }

        // Add a committed change
        lines.push(DiffLine::new(
            LineSource::Committed,
            "new_line".to_string(),
            '+',
            Some(21),
        ));

        // Add trailing base lines (these should show as context)
        lines.push(base_line("end"));
        lines.push(base_line("end"));

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view_mode = ViewMode::Context;

        let items = app.compute_displayable_items();
        let filtered = collect_lines(&app, &items);

        // The change should be visible
        let has_change = filtered.iter().any(|l| l.content == "new_line");
        assert!(has_change, "The change should be visible");

        // The trailing "end" lines should be visible as context
        let trailing_ends = filtered.iter()
            .filter(|l| l.content == "end" && l.source == LineSource::Base)
            .count();
        assert_eq!(trailing_ends, 2,
            "Both trailing 'end' lines should be visible. Found {} of 2. \
             Filtered: {:?}",
            trailing_ends,
            filtered.iter().map(|l| &l.content).collect::<Vec<_>>());
    }

    #[test]
    fn test_context_mode_last_file_trailing_lines() {
        // REGRESSION TEST: This simulates the exact scenario from the bug:
        // Multiple files, and the LAST file has trailing lines after additions.
        //
        // File structure (simulated):
        // - File 1: some content (file header + lines)
        // - Empty separator line
        // - File 2 (last): base lines, then committed additions, then base trailing lines
        //
        // In context mode, the trailing base lines of the last file should be visible.

        let mut lines = Vec::new();

        // ===== FILE 1 =====
        lines.push(DiffLine::file_header("file1.rb"));
        for i in 0..10 {
            lines.push(base_line(&format!("file1_line{}", i)));
        }
        // One change in file 1
        lines.push(change_line("file1_change"));
        for i in 0..10 {
            lines.push(base_line(&format!("file1_after{}", i)));
        }
        // Empty separator between files
        lines.push(base_line(""));

        // ===== FILE 2 (last file) =====
        lines.push(DiffLine::file_header("file2.rb"));
        // Many base lines
        for i in 0..50 {
            lines.push(base_line(&format!("file2_base{}", i)));
        }
        // Block of additions at position ~50
        lines.push(DiffLine::new(LineSource::Committed, "added_line_1".to_string(), '+', Some(51)));
        lines.push(DiffLine::new(LineSource::Committed, "added_line_2".to_string(), '+', Some(52)));
        lines.push(DiffLine::new(LineSource::Committed, "added_line_3".to_string(), '+', Some(53)));
        lines.push(DiffLine::new(LineSource::Committed, "  end".to_string(), '+', Some(54)));  // The "+ end" from bug
        // Trailing base lines
        lines.push(base_line("end"));   // These are the missing lines
        lines.push(base_line("end"));

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view_mode = ViewMode::Context;

        let items = app.compute_displayable_items();
        let filtered = collect_lines(&app, &items);

        // The "+ end" line should be visible (it's Committed)
        let has_added_end = filtered.iter().any(|l| l.content == "  end" && l.source == LineSource::Committed);
        assert!(has_added_end, "The '+ end' addition should be visible");

        // The trailing base "end" lines should be visible as context
        let trailing_base_ends = filtered.iter()
            .filter(|l| l.content == "end" && l.source == LineSource::Base)
            .count();
        assert_eq!(trailing_base_ends, 2,
            "Both trailing base 'end' lines should be visible as context. Found {}",
            trailing_base_ends);
    }

    #[test]
    fn test_context_mode_scroll_to_bottom_shows_trailing() {
        // Test that scrolling to the bottom in context mode shows trailing lines

        let mut lines = Vec::new();

        // File header
        lines.push(DiffLine::file_header("test.rb"));

        // Many base lines at the start
        for i in 0..100 {
            lines.push(base_line(&format!("base_line_{}", i)));
        }

        // Some committed changes near the end
        for i in 0..5 {
            lines.push(DiffLine::new(
                LineSource::Committed,
                format!("added_{}", i),
                '+',
                Some(101 + i),
            ));
        }

        // Trailing base lines (like "end" "end")
        lines.push(base_line("trailing_1"));
        lines.push(base_line("trailing_2"));
        lines.push(base_line("trailing_3"));

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view_mode = ViewMode::Context;
        app.viewport_height = 20;

        // Scroll to bottom
        app.go_to_bottom();

        let visible = get_visible_lines(&app);

        // The last visible line should be trailing_3
        let last_visible = visible.last().unwrap();
        assert_eq!(last_visible.content, "trailing_3",
            "Last visible line should be 'trailing_3', got '{}'", last_visible.content);

        // All three trailing lines should be visible
        let has_trailing_1 = visible.iter().any(|l| l.content == "trailing_1");
        let has_trailing_2 = visible.iter().any(|l| l.content == "trailing_2");
        let has_trailing_3 = visible.iter().any(|l| l.content == "trailing_3");

        assert!(has_trailing_1, "trailing_1 should be visible when scrolled to bottom");
        assert!(has_trailing_2, "trailing_2 should be visible when scrolled to bottom");
        assert!(has_trailing_3, "trailing_3 should be visible when scrolled to bottom");
    }

    #[test]
    fn test_context_mode_large_file_scroll_to_bottom() {
        // Test with multiple change regions so context mode has more lines than viewport

        let mut lines = Vec::new();

        // File header
        lines.push(DiffLine::file_header("test.rb"));

        // Create several change regions spread throughout the file
        // Each region: base lines, then changes, then more base lines

        // Region 1 at the start
        for i in 0..10 { lines.push(base_line(&format!("region1_base_{}", i))); }
        for i in 0..3 { lines.push(change_line(&format!("region1_change_{}", i))); }

        // Large gap of base lines
        for i in 0..50 { lines.push(base_line(&format!("gap1_base_{}", i))); }

        // Region 2 in the middle
        for i in 0..3 { lines.push(change_line(&format!("region2_change_{}", i))); }
        for i in 0..20 { lines.push(base_line(&format!("region2_after_{}", i))); }

        // Large gap of base lines
        for i in 0..50 { lines.push(base_line(&format!("gap2_base_{}", i))); }

        // Region 3 near the end (the one we care about)
        for i in 0..5 { lines.push(change_line(&format!("region3_change_{}", i))); }

        // Trailing base lines
        lines.push(base_line("final_end_1"));
        lines.push(base_line("final_end_2"));

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view_mode = ViewMode::Context;
        app.viewport_height = 15; // Small viewport so we need to scroll

        // Check all displayable items before scrolling
        {
            let items = app.compute_displayable_items();
            let all_displayable = collect_lines(&app, &items);
            let last_displayable = all_displayable.last().unwrap();
            assert_eq!(last_displayable.content, "final_end_2",
                "Last displayable line should be final_end_2, got '{}'", last_displayable.content);
        }

        // Scroll to bottom
        app.go_to_bottom();

        let visible = get_visible_lines(&app);

        // The trailing lines should be visible when scrolled to bottom
        let has_final_1 = visible.iter().any(|l| l.content == "final_end_1");
        let has_final_2 = visible.iter().any(|l| l.content == "final_end_2");

        assert!(has_final_1, "final_end_1 should be visible at bottom");
        assert!(has_final_2, "final_end_2 should be visible at bottom");
    }

    #[test]
    fn test_bug_scenario_multiple_files_last_file_trailing() {
        // This test simulates the EXACT scenario from the bug:
        // - 4 files total
        // - The LAST file has additions near the end
        // - The additions end with a Committed "end"
        // - Then 2 more Base "end" lines
        //
        // In context mode, after pressing G (go to bottom), we should see
        // ALL lines including the trailing Base "end" lines.

        use crate::diff::{DiffLine, LineSource};

        let mut lines = Vec::new();

        // ===== FILES 1-3 (with changes, to simulate "4 files") =====
        for file_num in 1..=3 {
            lines.push(DiffLine::file_header(&format!("file{}.rb", file_num)));
            for i in 0..20 {
                lines.push(DiffLine::new(LineSource::Base, format!("file{}_line{}", file_num, i), ' ', Some(i+1)));
            }
            // A change in each file
            lines.push(DiffLine::new(LineSource::Committed, format!("file{}_change", file_num), '+', Some(21)));
            for i in 0..10 {
                lines.push(DiffLine::new(LineSource::Base, format!("file{}_after{}", file_num, i), ' ', Some(22+i)));
            }
            // Separator
            lines.push(DiffLine::new(LineSource::Base, "".to_string(), ' ', None));
        }

        // ===== FILE 4 (the one with trailing context issues) =====
        lines.push(DiffLine::file_header("premium_due_notice_spec.rb"));

        // Many base lines (simulating lines 1-101)
        for i in 1..=101 {
            lines.push(DiffLine::new(
                LineSource::Base,
                format!("    it {{ spec line {} }}", i),
                ' ',
                Some(i),
            ));
        }

        // The added test block (lines 102-105)
        lines.push(DiffLine::new(LineSource::Committed, "".to_string(), '+', Some(102)));  // empty line
        lines.push(DiffLine::new(LineSource::Committed, "    it \"calculates total_due\" do".to_string(), '+', Some(103)));
        lines.push(DiffLine::new(LineSource::Committed, "      expect(letter.send(:total_due)).to eq(...)".to_string(), '+', Some(104)));
        lines.push(DiffLine::new(LineSource::Committed, "    end".to_string(), '+', Some(105)));  // THIS IS THE + end

        // Trailing base lines (lines 106-107) - THESE ARE MISSING IN THE BUG
        lines.push(DiffLine::new(LineSource::Base, "  end".to_string(), ' ', Some(106)));
        lines.push(DiffLine::new(LineSource::Base, "end".to_string(), ' ', Some(107)));

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.view_mode = ViewMode::Context;
        app.viewport_height = 20;

        // Check ALL displayable items before scrolling
        {
            let items = app.compute_displayable_items();
            let all_displayable = collect_lines(&app, &items);

            // 1. The "    end" (Committed) should be in displayable items
            let has_committed_end = all_displayable.iter().any(|l| l.content == "    end" && l.source == LineSource::Committed);
            assert!(has_committed_end, "Should have Committed '    end' in displayable lines");

            // 2. The "  end" (Base) should be in displayable items
            let has_base_end_indented = all_displayable.iter().any(|l| l.content == "  end" && l.source == LineSource::Base);
            assert!(has_base_end_indented, "Should have Base '  end' in displayable lines");

            // 3. The "end" (Base) should be in displayable items
            let has_base_end = all_displayable.iter().any(|l| l.content == "end" && l.source == LineSource::Base);
            assert!(has_base_end, "Should have Base 'end' in displayable lines");
        }

        // Now scroll to bottom
        app.go_to_bottom();
        let visible = get_visible_lines(&app);

        // 4. When scrolled to bottom, the last visible line should be "end" (Base)
        let last_visible = visible.last().unwrap();
        assert_eq!(last_visible.content, "end", "Last visible should be 'end'");
        assert_eq!(last_visible.source, LineSource::Base, "Last visible should be Base");
    }

    #[test]
    fn test_compute_refresh_returns_valid_result() {
        use std::process::Command;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path().to_path_buf();

        Command::new("git")
            .args(["init"])
            .current_dir(&repo_path)
            .output()
            .expect("failed to init git repo");

        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&repo_path)
            .output()
            .expect("failed to set git email");

        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&repo_path)
            .output()
            .expect("failed to set git name");

        std::fs::write(repo_path.join("test.txt"), "initial content\n").unwrap();

        Command::new("git")
            .args(["add", "."])
            .current_dir(&repo_path)
            .output()
            .expect("failed to add files");

        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&repo_path)
            .output()
            .expect("failed to commit");

        Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(&repo_path)
            .output()
            .expect("failed to rename branch");

        std::fs::write(repo_path.join("test.txt"), "modified content\n").unwrap();

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = compute_refresh(&repo_path, "main", &cancel_flag);

        assert!(result.is_ok(), "compute_refresh should succeed");
        let refresh_result = result.unwrap();

        assert!(!refresh_result.lines.is_empty(), "should have some diff lines");
        assert!(
            refresh_result.lines.iter().any(|l| l.content.contains("modified")),
            "should contain the modified content"
        );
    }

    #[test]
    fn test_refresh_result_can_be_applied_to_app() {
        let mut app = TestAppBuilder::new().with_lines(vec![base_line("old content")]).build();

        let new_lines = vec![
            DiffLine::file_header("new_file.txt"),
            base_line("new line 1"),
            change_line("new line 2"),
        ];

        let result = RefreshResult {
            files: vec![],
            lines: new_lines.clone(),
            merge_base: "newbase123".to_string(),
            current_branch: Some("new-branch".to_string()),
            metrics: crate::limits::DiffMetrics::default(),
        };

        app.apply_refresh_result(result);

        assert_eq!(app.merge_base, "newbase123");
        assert_eq!(app.lines.len(), 3);
        assert_eq!(app.lines[0].content, "new_file.txt");
        assert_eq!(app.lines[1].content, "new line 1");
        assert_eq!(app.lines[2].content, "new line 2");
    }

    #[test]
    fn test_lines_appended_to_end_of_file_show_as_unstaged() {
        use crate::diff::compute_file_diff_v2;

        let base = "line1\nline2\nline3\n";
        let working = "line1\nline2\nline3\nline4\nline5\n";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working), None);

        let unstaged: Vec<_> = diff.lines.iter()
            .filter(|l| matches!(l.source, LineSource::Unstaged))
            .collect();

        assert_eq!(unstaged.len(), 2);
        assert_eq!(unstaged[0].content, "line4");
        assert_eq!(unstaged[1].content, "line5");
    }

    #[test]
    fn test_middle_insertion_plus_appends_at_end() {
        use crate::diff::compute_file_diff_v2;

        let base = "line1\nline2\nline3\nline4\nline5\n";
        let working = "line1\nINSERTED\nline2\nline3\nline4\nline5\nAPPEND1\nAPPEND2\n";

        let diff = compute_file_diff_v2("test.txt", Some(base), Some(base), Some(base), Some(working), None);

        let unstaged: Vec<_> = diff.lines.iter()
            .filter(|l| matches!(l.source, LineSource::Unstaged))
            .collect();

        assert!(unstaged.iter().any(|l| l.content == "INSERTED"));
        assert!(unstaged.iter().any(|l| l.content == "APPEND1"));
        assert!(unstaged.iter().any(|l| l.content == "APPEND2"));
    }

    #[test]
    fn test_refresh_channel_communication() {
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;
        use tempfile::TempDir;
        use std::process::Command;

        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path().to_path_buf();

        Command::new("git")
            .args(["init"])
            .current_dir(&repo_path)
            .output()
            .expect("failed to init git repo");

        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&repo_path)
            .output()
            .expect("failed to set git email");

        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&repo_path)
            .output()
            .expect("failed to set git name");

        std::fs::write(repo_path.join("file.txt"), "content\n").unwrap();

        Command::new("git")
            .args(["add", "."])
            .current_dir(&repo_path)
            .output()
            .expect("failed to add files");

        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&repo_path)
            .output()
            .expect("failed to commit");

        Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(&repo_path)
            .output()
            .expect("failed to rename branch");

        let (tx, rx) = mpsc::channel::<RefreshResult>();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let repo_clone = repo_path.clone();
        let cancel_clone = cancel_flag.clone();

        thread::spawn(move || {
            if let Ok(result) = compute_refresh(&repo_clone, "main", &cancel_clone) {
                let _ = tx.send(result);
            }
        });

        let result = rx.recv_timeout(Duration::from_secs(5));
        assert!(result.is_ok(), "should receive result within timeout");

        let refresh_result = result.unwrap();
        assert!(refresh_result.lines.is_empty() || !refresh_result.merge_base.is_empty());
    }

    // === Tests for needs_inline_spans dirty flag ===

    #[test]
    fn test_initial_state_needs_inline_spans() {
        let app = TestAppBuilder::new().build();
        assert!(app.needs_inline_spans(), "New app should need inline spans");
    }

    #[test]
    fn test_clear_needs_inline_spans() {
        let mut app = TestAppBuilder::new().build();
        assert!(app.needs_inline_spans());
        app.clear_needs_inline_spans();
        assert!(!app.needs_inline_spans());
    }

    #[test]
    fn test_scroll_marks_needs_inline_spans() {
        let lines: Vec<DiffLine> = (0..50).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 10;
        app.clear_needs_inline_spans();

        app.scroll_down(5);
        assert!(app.needs_inline_spans(), "scroll_down should mark dirty");

        app.clear_needs_inline_spans();
        app.scroll_up(2);
        assert!(app.needs_inline_spans(), "scroll_up should mark dirty");
    }

    #[test]
    fn test_page_navigation_marks_needs_inline_spans() {
        let lines: Vec<DiffLine> = (0..50).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 10;
        app.clear_needs_inline_spans();

        app.page_down();
        assert!(app.needs_inline_spans(), "page_down should mark dirty");

        app.clear_needs_inline_spans();
        app.page_up();
        assert!(app.needs_inline_spans(), "page_up should mark dirty");
    }

    #[test]
    fn test_go_to_extremes_marks_needs_inline_spans() {
        let lines: Vec<DiffLine> = (0..50).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 10;
        app.clear_needs_inline_spans();

        app.go_to_bottom();
        assert!(app.needs_inline_spans(), "go_to_bottom should mark dirty");

        app.clear_needs_inline_spans();
        app.go_to_top();
        assert!(app.needs_inline_spans(), "go_to_top should mark dirty");
    }

    #[test]
    fn test_view_mode_change_marks_needs_inline_spans() {
        let lines = vec![base_line("context"), change_line("change")];
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.clear_needs_inline_spans();

        app.cycle_view_mode();
        assert!(app.needs_inline_spans(), "cycle_view_mode should mark dirty");
    }

    #[test]
    fn test_file_collapse_marks_needs_inline_spans() {
        let lines = vec![DiffLine::file_header("test.rs"), change_line("change")];
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.clear_needs_inline_spans();

        app.toggle_file_collapsed("test.rs");
        assert!(app.needs_inline_spans(), "toggle_file_collapsed should mark dirty");
    }

    #[test]
    fn test_content_refresh_marks_needs_inline_spans() {
        let mut app = TestAppBuilder::new().build();
        app.clear_needs_inline_spans();

        let result = RefreshResult {
            files: vec![],
            lines: vec![change_line("new")],
            merge_base: "abc".to_string(),
            current_branch: Some("feature".to_string()),
            metrics: crate::limits::DiffMetrics::default(),
        };
        app.apply_refresh_result(result);
        assert!(app.needs_inline_spans(), "apply_refresh_result should mark dirty");
    }

    #[test]
    fn test_viewport_change_marks_needs_inline_spans() {
        let mut app = TestAppBuilder::new().build();
        app.clear_needs_inline_spans();

        app.set_viewport_height(30);
        assert!(app.needs_inline_spans(), "set_viewport_height should mark dirty");
    }

    // === Tests for operations that should NOT mark dirty ===

    #[test]
    fn test_toggle_help_does_not_mark_dirty() {
        let mut app = TestAppBuilder::new().build();
        app.clear_needs_inline_spans();

        app.toggle_help();
        assert!(!app.needs_inline_spans(), "toggle_help should not mark dirty");
    }

    #[test]
    fn test_scroll_at_top_does_not_mark_dirty() {
        let lines: Vec<DiffLine> = (0..20).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.scroll_offset = 0;
        app.clear_needs_inline_spans();

        app.scroll_up(5);
        assert!(!app.needs_inline_spans(), "scroll_up at top should not mark dirty");
    }

    #[test]
    fn test_scroll_at_bottom_does_not_mark_dirty() {
        let lines: Vec<DiffLine> = (0..20).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.viewport_height = 10;
        app.go_to_bottom();
        app.clear_needs_inline_spans();

        app.scroll_down(100);
        assert!(!app.needs_inline_spans(), "scroll_down at bottom should not mark dirty");
    }

    #[test]
    fn test_go_to_top_when_at_top_does_not_mark_dirty() {
        let lines: Vec<DiffLine> = (0..20).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.scroll_offset = 0;
        app.clear_needs_inline_spans();

        app.go_to_top();
        assert!(!app.needs_inline_spans(), "go_to_top when at top should not mark dirty");
    }

    #[test]
    fn test_same_viewport_height_does_not_mark_dirty() {
        let mut app = TestAppBuilder::new().build();
        app.viewport_height = 20;
        app.clear_needs_inline_spans();

        app.set_viewport_height(20);
        assert!(!app.needs_inline_spans(), "setting same viewport height should not mark dirty");
    }

    #[test]
    fn test_cycle_view_mode_with_empty_lines_still_marks_dirty() {
        let mut app = TestAppBuilder::new().build();
        app.clear_needs_inline_spans();

        app.cycle_view_mode();
        assert!(app.needs_inline_spans(), "cycle_view_mode should mark dirty even if empty");
    }

    #[test]
    fn test_format_diff_for_copy_basic() {
        let lines = vec![
            DiffLine::file_header("test.rs"),
            base_line("unchanged"),
            change_line("added line"),
        ];
        let app = TestAppBuilder::new().with_lines(lines).build();

        let output = app.format_diff_for_copy();

        assert!(output.contains("── test.rs ──"));
        assert!(output.contains("  unchanged"));
        assert!(output.contains("+ added line"));
    }

    #[test]
    fn test_format_diff_for_copy_respects_collapsed_files() {
        let mut lines = vec![
            DiffLine::file_header("collapsed.rs"),
            base_line("hidden line"),
            DiffLine::file_header("visible.rs"),
            change_line("visible line"),
        ];
        // Set file_path on non-header lines
        lines[1].file_path = Some("collapsed.rs".to_string());
        lines[3].file_path = Some("visible.rs".to_string());

        let mut app = TestAppBuilder::new().with_lines(lines).build();
        app.collapsed_files.insert("collapsed.rs".to_string());

        let output = app.format_diff_for_copy();

        // Header for collapsed file should still appear
        assert!(output.contains("── collapsed.rs ──"));
        // But content should be hidden
        assert!(!output.contains("hidden line"));
        // Visible file content should appear
        assert!(output.contains("visible line"));
    }

    #[test]
    fn test_format_diff_for_copy_empty() {
        let app = TestAppBuilder::new().build();
        let output = app.format_diff_for_copy();
        assert!(output.is_empty());
    }
}
