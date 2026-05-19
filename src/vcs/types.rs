use std::collections::{HashMap, HashSet};
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

/// Whether to diff from the fork point or the trunk/origin tip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffBase {
    /// Diff from the merge-base / common ancestor. Stable: shows only our changes.
    #[default]
    ForkPoint,
    /// Diff from the current trunk() / origin tip. Shows full divergence.
    TrunkTip,
}

/// How far the current branch and upstream have diverged from their common ancestor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamDivergence {
    /// Commits on the upstream branch since the fork point.
    pub behind_count: usize,
    /// Files changed on the upstream branch since the fork point.
    pub upstream_files: HashSet<String>,
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
    /// Name of the current jj bookmark (for BookmarkOnly view mode label).
    pub bookmark_name: Option<String>,
    /// Upstream divergence info (behind count, upstream-changed files).
    pub divergence: Option<UpstreamDivergence>,
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
    /// Name of the current jj bookmark (for BookmarkOnly view mode).
    pub bookmark_name: Option<String>,
    /// Working revision ID at the time the refresh completed.
    /// Populated by the spawn function, not the VCS backend.
    pub revision_id: Option<String>,
    /// Upstream divergence info (behind count, upstream-changed files).
    pub divergence: Option<UpstreamDivergence>,
}

impl RefreshResult {
    /// An empty refresh result, used to seed `App` when the initial refresh
    /// fails but we still want to enter the TUI so the user can see the error
    /// banner and the file watcher can auto-recover.
    pub fn empty() -> Self {
        Self {
            files: Vec::new(),
            lines: Vec::new(),
            base_identifier: String::new(),
            base_label: None,
            current_branch: None,
            metrics: DiffMetrics::default(),
            file_links: HashMap::new(),
            stack_position: None,
            bookmark_name: None,
            revision_id: None,
            divergence: None,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_refresh_result_has_no_data_and_no_metadata() {
        // The empty constructor seeds App when the initial refresh fails so
        // the TUI can come up with an error banner. It must be safely
        // displayable — no labels, no identifiers, no files.
        let r = RefreshResult::empty();
        assert!(r.files.is_empty());
        assert!(r.lines.is_empty());
        assert!(r.base_identifier.is_empty());
        assert!(r.base_label.is_none());
        assert!(r.current_branch.is_none());
        assert!(r.file_links.is_empty());
        assert!(r.stack_position.is_none());
        assert!(r.bookmark_name.is_none());
        assert!(r.revision_id.is_none());
        assert!(r.divergence.is_none());
    }
}
