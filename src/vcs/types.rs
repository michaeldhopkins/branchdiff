use std::collections::HashMap;
use std::path::PathBuf;

use crate::diff::{DiffLine, FileDiff};
use crate::limits::DiffMetrics;

/// Position of `@` within a jj commit stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StackPosition {
    /// 1-based index of `@` in the stack (1 = bottom, total = tip).
    pub current: usize,
    /// Total number of commits between trunk and the stack tip.
    pub total: usize,
    /// Number of independent heads descending from `@`. 1 = linear stack.
    pub head_count: usize,
}

/// Which VCS backend is in use (for UI dispatch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcsBackend {
    Git,
    Jj,
}

/// VCS-agnostic context describing what we're comparing.
///
/// Contains only UI labels. The base identifier (merge-base SHA, change ID)
/// lives in `RefreshResult` and `App::base_identifier`.
#[derive(Debug, Clone)]
pub struct ComparisonContext {
    /// Label for what we're comparing against (e.g., "main", "@-")
    pub from_label: String,
    /// Label for what we're comparing to (e.g., "feature", "HEAD", "@")
    pub to_label: String,
    /// Position of `@` in the jj commit stack, if applicable.
    pub stack_position: Option<StackPosition>,
    /// VCS backend for UI label customization (gutter symbols, help text).
    pub vcs_backend: VcsBackend,
}

/// Result of a full refresh from a VCS backend.
#[derive(Debug)]
pub struct RefreshResult {
    pub files: Vec<FileDiff>,
    pub lines: Vec<DiffLine>,
    pub base_identifier: String,
    pub base_label: Option<String>,
    pub current_branch: Option<String>,
    pub metrics: DiffMetrics,
    pub file_links: HashMap<String, String>,
    pub stack_position: Option<StackPosition>,
    /// Working revision ID at the time the refresh completed.
    /// Populated by the spawn function, not the VCS backend.
    pub revision_id: Option<String>,
}

/// Paths a VCS backend wants watched for change detection.
pub struct VcsWatchPaths {
    /// Individual files to watch non-recursively (e.g., .git/index, .git/HEAD)
    pub files: Vec<PathBuf>,
    /// Directories to watch recursively (e.g., .git/refs/)
    pub recursive_dirs: Vec<PathBuf>,
}

/// Classification of a file event for differentiated debouncing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcsEventType {
    /// VCS internal state change (e.g., .git/index) — triggers delayed refresh
    Internal,
    /// Branch/revision change (e.g., .git/HEAD, .git/refs/) — triggers refresh
    RevisionChange,
    /// Lock file (external operation in progress) — defer refresh
    Lock,
    /// Regular source file — triggers immediate refresh
    Source,
}
