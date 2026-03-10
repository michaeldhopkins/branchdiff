mod changed_files;
mod commands;
mod refresh;

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::diff::FileDiff;
use crate::vcs::{ComparisonContext, RefreshResult, Vcs, VcsBackend};

pub use changed_files::ChangedFile;
pub use commands::{
    detect_base_branch, fetch_base_branch, get_binary_files, get_current_branch,
    get_file_bytes_at_ref, get_git_version, get_merge_base_preferring_origin, get_repo_root,
    get_working_tree_bytes, has_merge_conflicts, is_binary_file, is_index_locked, GitVersion,
};

/// Git backend for branchdiff.
pub struct GitVcs {
    repo_path: PathBuf,
    base_branch: String,
    git_version: GitVersion,
}

impl GitVcs {
    /// Create a new GitVcs for the given repository.
    pub fn new(repo_path: PathBuf) -> Result<Self> {
        let base_branch = detect_base_branch(&repo_path)
            .unwrap_or_else(|_| "main".to_string());
        let git_version = get_git_version()
            .context("Failed to detect git version")?;
        Ok(Self { repo_path, base_branch, git_version })
    }

    /// The base branch name (e.g., "main" or "master").
    pub fn base_branch(&self) -> &str {
        &self.base_branch
    }
}

impl Vcs for GitVcs {
    fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    fn comparison_context(&self) -> Result<ComparisonContext> {
        let current_branch = get_current_branch(&self.repo_path).unwrap_or(None);
        let to_label = current_branch.unwrap_or_else(|| "HEAD".to_string());

        Ok(ComparisonContext {
            from_label: self.base_branch.clone(),
            to_label,
            stack_position: None,
            vcs_backend: VcsBackend::Git,
            bookmark_name: None,
        })
    }

    fn refresh(&self, cancel_flag: &Arc<AtomicBool>) -> Result<RefreshResult> {
        refresh::git_compute_refresh(&self.repo_path, &self.base_branch, cancel_flag)
    }

    fn single_file_diff(&self, file_path: &str) -> Option<FileDiff> {
        let merge_base = get_merge_base_preferring_origin(&self.repo_path, &self.base_branch)
            .unwrap_or_default();
        let old_path = changed_files::find_rename_source(&self.repo_path, file_path, &merge_base);
        refresh::git_compute_single_file_diff(&self.repo_path, file_path, old_path.as_deref(), &merge_base)
    }

    fn base_identifier(&self) -> Result<String> {
        get_merge_base_preferring_origin(&self.repo_path, &self.base_branch)
    }

    fn base_file_bytes(&self, file_path: &str) -> Result<Option<Vec<u8>>> {
        let merge_base = get_merge_base_preferring_origin(&self.repo_path, &self.base_branch)
            .unwrap_or_default();
        get_file_bytes_at_ref(&self.repo_path, file_path, &merge_base)
    }

    fn working_file_bytes(&self, file_path: &str) -> Result<Option<Vec<u8>>> {
        get_working_tree_bytes(&self.repo_path, file_path)
    }

    fn binary_files(&self) -> HashSet<String> {
        let merge_base = get_merge_base_preferring_origin(&self.repo_path, &self.base_branch)
            .unwrap_or_default();
        get_binary_files(&self.repo_path, &merge_base)
    }

    fn fetch(&self) -> Result<()> {
        fetch_base_branch(&self.repo_path, &self.base_branch)
    }

    fn has_conflicts(&self) -> Result<bool> {
        has_merge_conflicts(&self.repo_path, &self.base_branch, &self.git_version)
    }

    fn is_locked(&self) -> bool {
        is_index_locked(&self.repo_path)
    }

    fn watch_paths(&self) -> crate::vcs::VcsWatchPaths {
        let git_dir = self.repo_path.join(".git");
        crate::vcs::VcsWatchPaths {
            files: vec![git_dir.join("index"), git_dir.join("HEAD")],
            recursive_dirs: vec![git_dir.join("refs")],
        }
    }

    fn classify_event(&self, path: &Path) -> crate::vcs::VcsEventType {
        use crate::vcs::VcsEventType;

        let relative = path.strip_prefix(&self.repo_path).unwrap_or(path);
        let is_git_path = relative
            .components()
            .next()
            .is_some_and(|c| c.as_os_str() == ".git");

        if !is_git_path {
            return VcsEventType::Source;
        }

        // Any .lock file inside .git/ signals an external operation
        if relative.extension().is_some_and(|ext| ext == "lock") {
            return VcsEventType::Lock;
        }

        // Only exact .git/HEAD is a revision change, not FETCH_HEAD/ORIG_HEAD/MERGE_HEAD
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

    fn backend(&self) -> VcsBackend {
        VcsBackend::Git
    }

    fn current_revision_id(&self) -> Result<String> {
        let output = crate::vcs::shared::run_vcs_with_retry(
            "git", &self.repo_path,
            &["rev-parse", "--short", "HEAD"],
            commands::is_transient_error,
        )?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            anyhow::bail!("git rev-parse HEAD failed")
        }
    }
}
