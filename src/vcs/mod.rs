pub mod git;
pub mod jj;
pub mod types;

pub use types::{ComparisonContext, RefreshResult, StackPosition, VcsEventType, VcsWatchPaths};

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use rayon::ThreadPoolBuilder;

use crate::diff::FileDiff;

const PARALLEL_THRESHOLD: usize = 4;
const MAX_VCS_THREADS: usize = 16;

static VCS_POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();

/// Shared thread pool for parallel file processing across VCS backends.
/// Only one backend is active at a time, so a single pool suffices.
pub(crate) fn vcs_thread_pool() -> &'static rayon::ThreadPool {
    VCS_POOL.get_or_init(|| {
        let num_threads = std::thread::available_parallelism()
            .map(|n| n.get().min(MAX_VCS_THREADS))
            .unwrap_or(4);
        ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .build()
            .expect("failed to build VCS thread pool")
    })
}

/// Detect the VCS backend for a given path.
///
/// Checks jj first (takes precedence in colocated repos where both
/// .jj/ and .git/ exist), then falls back to git.
pub fn detect(path: &Path) -> Result<Box<dyn Vcs>> {
    // jj first — in colocated repos, jj is the primary VCS.
    // Check the path itself, then walk up parent dirs.
    if path.join(".jj").is_dir()
        && let Ok(root) = jj::get_repo_root(path)
    {
        return Ok(Box::new(jj::JjVcs::new(root)?));
    }
    if let Some(ancestor) = path.ancestors().find(|p| p.join(".jj").is_dir())
        && let Ok(root) = jj::get_repo_root(ancestor)
    {
        return Ok(Box::new(jj::JjVcs::new(root)?));
    }
    // Fall back to git
    if let Ok(root) = git::get_repo_root(path) {
        return Ok(Box::new(git::GitVcs::new(root)?));
    }
    anyhow::bail!("Not a git or jj repository")
}

/// Trait for version control system backends.
///
/// Provides the operations branchdiff needs to compute and display diffs.
/// Implemented by `GitVcs` for git repos; future backends (jj) implement the same trait.
pub trait Vcs: Send + Sync {
    /// Root path of the repository.
    fn repo_path(&self) -> &Path;

    /// Get the comparison context (labels and resolved base reference).
    fn comparison_context(&self) -> Result<ComparisonContext>;

    /// Compute a full refresh of all diffs.
    fn refresh(&self, cancel_flag: &Arc<AtomicBool>) -> Result<RefreshResult>;

    /// Compute the diff for a single file (for incremental updates).
    fn single_file_diff(&self, file_path: &str) -> Option<FileDiff>;

    /// Get the base identifier (merge-base SHA, change ID, etc.) for the current comparison.
    fn base_identifier(&self) -> Result<String>;

    /// Get file bytes at the comparison base (for image diffs).
    fn base_file_bytes(&self, file_path: &str) -> Result<Option<Vec<u8>>>;

    /// Get file bytes from the working tree (for image diffs).
    fn working_file_bytes(&self, file_path: &str) -> Result<Option<Vec<u8>>>;

    /// Get the set of binary files in the current diff.
    fn binary_files(&self) -> HashSet<String>;

    /// Fetch updates from remote (e.g., git fetch).
    fn fetch(&self) -> Result<()>;

    /// Check if there are merge conflicts with the remote base.
    fn has_conflicts(&self) -> Result<bool>;

    /// Check if an external VCS operation holds a lock (e.g., .git/index.lock).
    fn is_locked(&self) -> bool;

    /// Paths to watch for VCS state changes.
    fn watch_paths(&self) -> VcsWatchPaths;

    /// Classify a file event for differentiated debouncing.
    fn classify_event(&self, path: &Path) -> VcsEventType;

    /// Human-readable VCS name (e.g., "git", "jj").
    fn vcs_name(&self) -> &str;

    /// Current working revision identifier, without triggering side effects.
    ///
    /// For jj: `@`'s change_id (uses `--ignore-working-copy` to avoid auto-snapshot).
    /// For git: short HEAD SHA.
    /// Used for post-refresh staleness checks — detects external VCS operations
    /// (e.g., `jj new`) that happened during an active refresh.
    fn current_revision_id(&self) -> Result<String>;
}
