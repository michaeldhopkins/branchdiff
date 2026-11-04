pub(crate) mod changed_files;
pub(crate) mod commands;
mod content;
mod fingerprint;
pub(crate) mod highlight;
mod highlight_range;
pub(crate) mod parse;
mod refresh;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};

use crate::diff::FileDiff;
use crate::vcs::{ComparisonContext, RefreshResult, Vcs, VcsBackend, VcsOptions};

pub use changed_files::{ChangedFile, get_all_changed_files, get_changed_files_between};
pub use commands::{
    GitVersion, detect_base_branch, fetch_base_branch, get_binary_files, get_binary_files_between,
    get_current_branch, get_file_bytes_at_ref, get_git_version, get_merge_base_preferring_origin,
    get_merge_base_preferring_origin_for, get_repo_root, get_working_tree_bytes,
    has_merge_conflicts, is_binary_file, is_binary_file_between, is_index_locked,
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
    /// Optional explicit `--head <REV>` as `(resolved_ref, display_label)`.
    head_target: Option<(String, String)>,
    /// Optional explicit `--highlight <REVSET>` specification.
    highlight_spec: Option<highlight::GitHighlightSpec>,
    git_version: GitVersion,
}

/// Resolve an explicit `--base` or `--head` ref the way git itself would, plus a label.
///
/// The detected-base path deliberately tries `origin/<name>` first, which is
/// right for a *guessed* trunk. It is wrong for a ref the user named: `git
/// merge-base develop HEAD` uses the local `develop`, so silently substituting
/// `origin/develop` hands back a different diff with nothing on screen to say
/// so. Prefer the exact ref, and when both exist and disagree, mark the label.
///
/// Returns `(ref_to_use, label)`. Anything that isn't a branch name — a tag, a
/// SHA, an already-qualified ref — falls through to git's own resolution.
pub(super) fn resolve_explicit_ref(
    repo_path: &Path,
    flag: &str,
    rev: &str,
) -> Result<(String, String)> {
    let local = format!("refs/heads/{rev}");
    let remote = format!("refs/remotes/origin/{rev}");

    match (commands::rev_id(repo_path, &local), commands::rev_id(repo_path, &remote)) {
        // Ambiguous: git resolves to the local branch. Say which we took.
        (Some(l), Some(r)) if l != r => Ok((local, format!("{rev} (local)"))),
        (Some(_), _) => Ok((local, rev.to_string())),
        // Only the remote-tracking branch exists. Using it is the helpful
        // reading of `<flag> develop`, but label it so it isn't mistaken for a
        // local branch of that name.
        (None, Some(_)) => Ok((remote, format!("origin/{rev}"))),
        (None, None) => {
            if commands::rev_id(repo_path, rev).is_some() {
                Ok((rev.to_string(), rev.to_string()))
            } else {
                let hint = if flag == "--highlight" {
                    "expected A..B, A...B, or a single commit/ref; "
                } else {
                    ""
                };
                anyhow::bail!(
                    "{flag} {rev:?} is not a branch, tag or commit in this repo \
                     ({hint}looked for {local}, {remote}, and {rev})"
                )
            }
        }
    }
}

struct ResolvedRefs {
    base_branch: String,
    base_label: String,
    head_target: Option<(String, String)>,
    highlight_spec: Option<highlight::GitHighlightSpec>,
}

impl GitVcs {
    /// Create a new GitVcs for the given repository.
    pub fn new(repo_path: PathBuf) -> Result<Self> {
        Self::with_options(repo_path, VcsOptions::default())
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
        Self::with_options(repo_path, VcsOptions { base, ..VcsOptions::default() })
    }

    /// Create a backend with explicit comparison options (`--base`, `--head`).
    pub fn with_options(repo_path: PathBuf, options: VcsOptions<'_>) -> Result<Self> {
        // `git --version` is independent of the refs, so it runs alongside the
        // lookups that resolve them.
        let (resolved, git_version) = std::thread::scope(|scope| {
            let version = scope.spawn(get_git_version);
            let resolved = Self::resolve_refs(&repo_path, options);
            (resolved, version.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)))
        });
        let ResolvedRefs { base_branch, base_label, head_target, highlight_spec } = resolved?;
        let git_version = git_version.context("Failed to detect git version")?;
        Ok(Self { repo_path, base_branch, base_label, head_target, highlight_spec, git_version })
    }

    /// Resolve `--base` (or detected trunk), optional `--head`, and optional `--highlight`.
    fn resolve_refs(repo_path: &Path, options: VcsOptions<'_>) -> Result<ResolvedRefs> {
        let head_target = match options.head {
            Some(head) => Some(resolve_explicit_ref(repo_path, "--head", head)?),
            None => None,
        };
        let highlight_spec = match options.highlight {
            Some(spec) => Some(highlight::resolve_highlight_spec(repo_path, spec)?),
            None => None,
        };
        let head_ref = head_target.as_ref().map_or("HEAD", |(r, _)| r.as_str());
        let (base_branch, base_label) = match options.base {
            Some(base) => {
                let resolved = resolve_explicit_ref(repo_path, "--base", base)?;
                // Prove it can actually produce a merge-base: that is the lookup
                // every refresh performs, and a failure here would otherwise
                // surface as a silent "0 files" rather than an error.
                get_merge_base_preferring_origin_for(repo_path, &resolved.0, head_ref)
                    .with_context(|| {
                        let target = options.head.unwrap_or("HEAD");
                        format!("--base {base:?} has no common ancestor with {target}")
                    })?;
                resolved
            }
            // Deliberately not defaulted to "main": a base that doesn't exist
            // makes every merge-base lookup fail and renders as "0 files", so a
            // repo with no discoverable trunk silently looked clean. Fail with
            // something actionable instead.
            None => {
                let detected = detect_base_branch(repo_path)?;
                if let Some(head) = options.head {
                    get_merge_base_preferring_origin_for(repo_path, &detected, head_ref)
                        .with_context(|| {
                            format!("--head {head:?} has no common ancestor with {detected}")
                        })?;
                }
                (detected.clone(), detected)
            }
        };
        Ok(ResolvedRefs { base_branch, base_label, head_target, highlight_spec })
    }

    /// The base branch name (e.g., "main" or "master").
    pub fn base_branch(&self) -> &str {
        &self.base_branch
    }

    fn head_ref(&self) -> &str {
        self.head_target.as_ref().map_or("HEAD", |(r, _)| r.as_str())
    }
}

impl Vcs for GitVcs {
    fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    fn comparison_context(&self) -> Result<ComparisonContext> {
        let to_label = match &self.head_target {
            Some((_, label)) => label.clone(),
            None => {
                get_current_branch(&self.repo_path).unwrap_or(None).unwrap_or_else(|| "HEAD".into())
            }
        };

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
        let head_target = self.head_target.as_ref().map(|(r, l)| (r.as_str(), l.as_str()));
        refresh::git_compute_refresh(
            &self.repo_path,
            &self.base_branch,
            &self.base_label,
            head_target,
            self.highlight_spec.as_ref(),
            cancel_flag,
        )
    }

    fn single_file_diff(&self, file_path: &str) -> Option<FileDiff> {
        let head_ref = self.head_ref();
        let merge_base =
            get_merge_base_preferring_origin_for(&self.repo_path, &self.base_branch, head_ref)
                .unwrap_or_default();
        let old_path = changed_files::find_rename_source_between(
            &self.repo_path, file_path, &merge_base, head_ref,
        );
        refresh::git_compute_single_file_diff(
            &self.repo_path,
            file_path,
            old_path.as_deref(),
            &merge_base,
            self.head_target.as_ref().map(|(r, _)| r.as_str()),
            self.highlight_spec.as_ref(),
        )
    }

    fn base_identifier(&self) -> Result<String> {
        get_merge_base_preferring_origin_for(&self.repo_path, &self.base_branch, self.head_ref())
    }

    fn base_file_bytes(&self, file_path: &str) -> Result<Option<Vec<u8>>> {
        let merge_base = get_merge_base_preferring_origin_for(
            &self.repo_path,
            &self.base_branch,
            self.head_ref(),
        )
        .unwrap_or_default();
        get_file_bytes_at_ref(&self.repo_path, file_path, &merge_base)
    }

    fn working_file_bytes(&self, file_path: &str) -> Result<Option<Vec<u8>>> {
        match &self.head_target {
            Some((head_ref, _)) => get_file_bytes_at_ref(&self.repo_path, file_path, head_ref),
            None => get_working_tree_bytes(&self.repo_path, file_path),
        }
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
        let is_git_path = relative.components().next().is_some_and(|c| c.as_os_str() == ".git");

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

    fn fingerprint_inputs(&self) -> Result<crate::fingerprint::FingerprintInputs> {
        fingerprint::inputs(
            &self.repo_path,
            &self.base_branch,
            self.head_target.as_ref().map(|(r, _)| r.as_str()),
            self.highlight_spec.as_ref(),
        )
    }

    fn backend(&self) -> VcsBackend {
        VcsBackend::Git
    }

    fn current_revision_id(&self) -> Result<String> {
        let output = vcs_runner::run_git_with_retry(
            &self.repo_path,
            &["rev-parse", "--short", self.head_ref()],
            vcs_runner::is_transient_error,
        )?;
        Ok(output.stdout_lossy().trim().to_string())
    }
}
