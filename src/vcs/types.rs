use std::collections::HashMap;
use std::path::PathBuf;

use crate::diff::{DiffLine, FileDiff};
use crate::limits::DiffMetrics;

/// VCS-agnostic context describing what we're comparing.
///
/// For git: "feature vs main" with a merge-base SHA.
/// For jj: "@ vs @-" with a change ID.
#[derive(Debug, Clone)]
pub struct ComparisonContext {
    /// Label for what we're comparing against (e.g., "main", "@-")
    pub from_label: String,
    /// Label for what we're comparing to (e.g., "feature", "HEAD", "@")
    pub to_label: String,
    /// Resolved base reference for diff computation (e.g., merge-base SHA, change ID)
    pub base_identifier: String,
}

/// Result of a full refresh from a VCS backend.
#[derive(Debug)]
pub struct RefreshResult {
    pub files: Vec<FileDiff>,
    pub lines: Vec<DiffLine>,
    pub base_identifier: String,
    pub current_branch: Option<String>,
    pub metrics: DiffMetrics,
    pub file_links: HashMap<String, String>,
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
