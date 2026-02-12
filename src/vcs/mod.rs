pub mod git;
pub mod types;

pub use types::{ComparisonContext, RefreshResult};

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::Result;

use crate::diff::FileDiff;

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
}
