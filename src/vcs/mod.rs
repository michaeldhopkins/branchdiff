pub mod git;
pub mod jj;
pub(crate) mod shared;
pub mod types;

pub use types::{ComparisonContext, DiffBase, RefreshResult, StackPosition, UpstreamDivergence, VcsBackend, VcsEventType, VcsWatchPaths};

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use rayon::ThreadPoolBuilder;

use crate::diff::FileDiff;

pub(crate) const PARALLEL_THRESHOLD: usize = 4;
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

/// Check for VCS directory existence without running external commands.
///
/// Returns the VCS type name and repo root path if found. Unlike [`detect`],
/// this never spawns subprocesses so it works even when `jj`/`git` aren't in
/// PATH (e.g. Zellij command panes that don't source a shell profile).
pub fn detect_repo_dir(path: &Path) -> Option<(&'static str, PathBuf)> {
    if path.join(".jj").is_dir() {
        return Some(("jj", path.to_path_buf()));
    }
    if let Some(ancestor) = path.ancestors().find(|p| p.join(".jj").is_dir()) {
        return Some(("jj", ancestor.to_path_buf()));
    }
    if path.join(".git").exists() {
        return Some(("git", path.to_path_buf()));
    }
    if let Some(ancestor) = path.ancestors().find(|p| p.join(".git").exists()) {
        return Some(("git", ancestor.to_path_buf()));
    }
    None
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

    /// Which VCS backend this is (for UI dispatch).
    fn backend(&self) -> VcsBackend;

    /// Current working revision identifier, without triggering side effects.
    ///
    /// For jj: `@`'s change_id (uses `--ignore-working-copy` to avoid auto-snapshot).
    /// For git: short HEAD SHA.
    /// Used for post-refresh staleness checks — detects external VCS operations
    /// (e.g., `jj new`) that happened during an active refresh.
    fn current_revision_id(&self) -> Result<String>;

    /// Set the diff base mode (fork point vs trunk tip).
    /// Only meaningful for jj — git always uses merge-base (fork point).
    fn set_diff_base(&self, _base: DiffBase) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn detect_repo_dir_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(detect_repo_dir(tmp.path()).is_none());
    }

    #[test]
    fn detect_repo_dir_jj() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join(".jj")).expect("mkdir .jj");
        let result = detect_repo_dir(tmp.path());
        assert_eq!(result, Some(("jj", tmp.path().to_path_buf())));
    }

    #[test]
    fn detect_repo_dir_git() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");
        let result = detect_repo_dir(tmp.path());
        assert_eq!(result, Some(("git", tmp.path().to_path_buf())));
    }

    #[test]
    fn detect_repo_dir_jj_takes_precedence() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join(".jj")).expect("mkdir .jj");
        fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");
        let (vcs_type, _) = detect_repo_dir(tmp.path()).expect("should detect");
        assert_eq!(vcs_type, "jj");
    }

    #[test]
    fn detect_repo_dir_git_worktree_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Git worktrees use a .git file (not directory) pointing to the main repo.
        fs::write(tmp.path().join(".git"), "gitdir: /some/other/repo/.git/worktrees/wt")
            .expect("write .git file");
        let result = detect_repo_dir(tmp.path());
        assert_eq!(result, Some(("git", tmp.path().to_path_buf())));
    }

    #[test]
    fn detect_repo_dir_ancestor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join(".jj")).expect("mkdir .jj");
        let child = tmp.path().join("subdir");
        fs::create_dir(&child).expect("mkdir subdir");
        let result = detect_repo_dir(&child);
        assert_eq!(result, Some(("jj", tmp.path().to_path_buf())));
    }
}
