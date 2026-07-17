mod changed_files;
mod commands;
mod refresh;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::diff::FileDiff;
use crate::vcs::{ComparisonContext, RefreshResult, Vcs, VcsBackend};

pub use changed_files::{get_all_changed_files, ChangedFile};
pub use commands::{
    detect_base_branch, fetch_base_branch, get_binary_files, get_current_branch,
    get_file_bytes_at_ref, get_git_version, get_merge_base_preferring_origin, get_repo_root,
    get_working_tree_bytes, has_merge_conflicts, is_binary_file, is_index_locked, GitVersion,
};

/// Git backend for branchdiff.
pub struct GitVcs {
    repo_path: PathBuf,
    /// The ref to compare against. For an explicit `--base` this is fully
    /// qualified (`refs/heads/x`, `refs/remotes/origin/x`), which resolves the
    /// local-vs-remote ambiguity once here rather than at every merge-base call.
    base_branch: String,
    /// What to show the user. Usually the same as `base_branch`, but says
    /// `x (local)` when a local `x` and an `origin/x` disagreed and git's rule
    /// picked the local one — otherwise nothing on screen would reveal which.
    base_label: String,
    git_version: GitVersion,
}

/// Resolve an explicit `--base` the way git itself would, plus a label.
///
/// The detected-base path deliberately tries `origin/<name>` first, which is
/// right for a *guessed* trunk. It is wrong for a ref the user named: `git
/// merge-base develop HEAD` uses the local `develop`, so silently substituting
/// `origin/develop` hands back a different diff with nothing on screen to say
/// so. Prefer the exact ref, and when both exist and disagree, mark the label.
///
/// Returns `(ref_to_use, label)`. Anything that isn't a branch name — a tag, a
/// SHA, an already-qualified ref — falls through to git's own resolution.
fn resolve_explicit_base(repo_path: &Path, base: &str) -> Result<(String, String)> {
    let local = format!("refs/heads/{base}");
    let remote = format!("refs/remotes/origin/{base}");

    match (commands::rev_id(repo_path, &local), commands::rev_id(repo_path, &remote)) {
        // Ambiguous: git resolves to the local branch. Say which we took.
        (Some(l), Some(r)) if l != r => Ok((local, format!("{base} (local)"))),
        (Some(_), _) => Ok((local, base.to_string())),
        // Only the remote-tracking branch exists. Using it is the helpful
        // reading of `--base develop`, but label it so it isn't mistaken for a
        // local branch of that name.
        (None, Some(_)) => Ok((remote, format!("origin/{base}"))),
        (None, None) => {
            if commands::rev_id(repo_path, base).is_some() {
                Ok((base.to_string(), base.to_string()))
            } else {
                anyhow::bail!(
                    "--base {base:?} is not a branch, tag or commit in this repo \
                     (looked for {local}, {remote}, and {base})"
                )
            }
        }
    }
}

impl GitVcs {
    /// Create a new GitVcs for the given repository.
    pub fn new(repo_path: PathBuf) -> Result<Self> {
        Self::with_base(repo_path, None)
    }

    /// Create a backend, optionally overriding the base branch.
    ///
    /// `base` may be a bare branch name (`develop`), a remote-qualified one
    /// (`origin/develop`) or a raw commit — [`get_merge_base_preferring_origin`]
    /// tries `origin/<base>` and falls back to `<base>`, so each spelling
    /// resolves through one path or the other.
    ///
    /// Validated by computing the merge-base up front: that is the same lookup
    /// every refresh performs, so if it fails here it would fail on every
    /// refresh — but silently, rendering as "no changes" rather than an error.
    pub fn with_base(repo_path: PathBuf, base: Option<&str>) -> Result<Self> {
        let (base_branch, base_label) = match base {
            Some(base) => {
                let resolved = resolve_explicit_base(&repo_path, base)?;
                // Prove it can actually produce a merge-base: that is the lookup
                // every refresh performs, and a failure here would otherwise
                // surface as a silent "0 files" rather than an error.
                get_merge_base_preferring_origin(&repo_path, &resolved.0).with_context(|| {
                    format!("--base {base:?} has no common ancestor with HEAD")
                })?;
                resolved
            }
            // Deliberately not defaulted to "main": a base that doesn't exist
            // makes every merge-base lookup fail and renders as "0 files", so a
            // repo with no discoverable trunk silently looked clean. Fail with
            // something actionable instead.
            None => {
                let detected = detect_base_branch(&repo_path)?;
                (detected.clone(), detected)
            }
        };
        let git_version = get_git_version()
            .context("Failed to detect git version")?;
        Ok(Self { repo_path, base_branch, base_label, git_version })
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
            from_label: self.base_label.clone(),
            to_label,
            stack_position: None,
            vcs_backend: VcsBackend::Git,
            bookmark_name: None,
            divergence: None,
        })
    }

    fn refresh(&self, cancel_flag: &Arc<AtomicBool>) -> Result<RefreshResult> {
        refresh::git_compute_refresh(&self.repo_path, &self.base_branch, &self.base_label, cancel_flag)
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
        let output = vcs_runner::run_git_with_retry(
            &self.repo_path,
            &["rev-parse", "--short", "HEAD"],
            vcs_runner::is_transient_error,
        )?;
        Ok(output.stdout_lossy().trim().to_string())
    }
}
