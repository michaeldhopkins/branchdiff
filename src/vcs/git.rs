use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use rayon::prelude::*;

use crate::diff::{compute_four_way_diff, DiffInput, DiffLine, FileDiff, LineSource};
use crate::file_links::compute_file_links;
use crate::image_diff::is_image_file;
use crate::limits::DiffMetrics;
use crate::vcs::{ComparisonContext, RefreshResult, Vcs};

/// Maximum number of retries for transient git errors
const MAX_RETRIES: u32 = 3;

/// Base delay for exponential backoff (doubles each retry)
const BASE_RETRY_DELAY_MS: u64 = 100;

/// Git version required for merge-tree --write-tree (conflict detection)
const MERGE_TREE_MIN_VERSION: (u32, u32) = (2, 38);

/// Parsed git version
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl GitVersion {
    /// Check if this version is at least the given major.minor
    pub fn at_least(&self, major: u32, minor: u32) -> bool {
        (self.major, self.minor) >= (major, minor)
    }
}

impl std::fmt::Display for GitVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Detect the installed git version
pub fn get_git_version() -> Result<GitVersion> {
    let output = Command::new("git")
        .args(["--version"])
        .output()
        .context("Failed to run git --version")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("git --version failed: {}", stderr.trim()));
    }

    let version_str = String::from_utf8_lossy(&output.stdout);
    parse_git_version(&version_str)
}

/// Check if a git error is transient (retryable).
/// Handles lock file contention which occurs when another git process is running.
///
/// We check for ".lock" in the error message because git lock filenames (like "index.lock",
/// "HEAD.lock", "config.lock") are not localized - they're always in English regardless
/// of the user's locale. The surrounding error text may be localized, but the filename isn't.
fn is_transient_error(stderr: &str) -> bool {
    // Lock filenames are not localized, so this is safe across locales
    stderr.contains(".lock")
}

/// Check if an external process holds the git index lock.
///
/// Returns true if `.git/index.lock` exists, indicating another git process
/// (like `git rebase`, `git commit`, etc.) is currently running.
/// When locked, branchdiff should defer refresh to avoid lock collisions.
pub fn is_index_locked(repo_path: &Path) -> bool {
    repo_path.join(".git/index.lock").exists()
}

/// Run a git command with retry logic for transient errors.
/// Uses exponential backoff: 100ms, 200ms, 400ms between retries.
///
/// Takes a closure that builds a fresh Command on each attempt, since Command
/// is consumed by output().
fn run_git_with_retry<F>(build_command: F) -> std::io::Result<Output>
where
    F: Fn() -> Command,
{
    for attempt in 0..=MAX_RETRIES {
        let output = build_command().output()?;

        if output.status.success() {
            return Ok(output);
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        if !is_transient_error(&stderr) || attempt == MAX_RETRIES {
            return Ok(output);
        }

        // Exponential backoff before retry
        let delay = Duration::from_millis(BASE_RETRY_DELAY_MS * (1 << attempt));
        thread::sleep(delay);
    }

    // This is unreachable due to the loop structure, but satisfies the compiler
    build_command().output()
}

/// Parse git version from "git version X.Y.Z" string
fn parse_git_version(s: &str) -> Result<GitVersion> {
    // Format: "git version 2.34.1" or "git version 2.50.1 (Apple Git-155)"
    let version_part = s
        .trim()
        .strip_prefix("git version ")
        .ok_or_else(|| anyhow!("Unexpected git version format: {}", s))?;

    // Take the first space-separated part (handles Apple Git suffix)
    let version_num = version_part.split_whitespace().next().unwrap_or(version_part);

    let parts: Vec<&str> = version_num.split('.').collect();
    if parts.len() < 2 {
        return Err(anyhow!("Cannot parse git version: {}", s));
    }

    let major = parts[0].parse().context("Invalid major version")?;
    let minor = parts[1].parse().context("Invalid minor version")?;
    let patch = parts.get(2).and_then(|p| p.parse().ok()).unwrap_or(0);

    Ok(GitVersion { major, minor, patch })
}


/// Get the root directory of the git repository
pub fn get_repo_root(path: &Path) -> Result<std::path::PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(path)
        .output()
        .context("Failed to run git rev-parse")?;

    if !output.status.success() {
        return Err(anyhow!(
            "Not a git repository: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let root = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_string();
    Ok(std::path::PathBuf::from(root))
}

/// Detect whether the base branch is 'main' or 'master'
pub fn detect_base_branch(repo_path: &Path) -> Result<String> {
    // Try 'main' first
    let main_exists = Command::new("git")
        .args(["rev-parse", "--verify", "main"])
        .current_dir(repo_path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if main_exists {
        return Ok("main".to_string());
    }

    // Fall back to 'master'
    let master_exists = Command::new("git")
        .args(["rev-parse", "--verify", "master"])
        .current_dir(repo_path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if master_exists {
        return Ok("master".to_string());
    }

    // Neither exists - might be a new repo or using different naming
    Err(anyhow!("Could not find 'main' or 'master' branch"))
}

/// Get the merge-base between HEAD and the base branch, preferring origin
pub fn get_merge_base_preferring_origin(repo_path: &Path, base_branch: &str) -> Result<String> {
    let remote_ref = format!("origin/{}", base_branch);
    get_merge_base(repo_path, &remote_ref)
        .or_else(|_| get_merge_base(repo_path, base_branch))
}

/// Get the merge-base between the base branch and HEAD
pub fn get_merge_base(repo_path: &Path, base_branch: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["merge-base", base_branch, "HEAD"])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git merge-base")?;

    if !output.status.success() {
        return Err(anyhow!(
            "Failed to find merge-base: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Get file content at a specific ref (commit, branch, or index)
/// Use `:path` for staged content (index)
pub fn get_file_at_ref(repo_path: &Path, file_path: &str, git_ref: &str) -> Result<Option<String>> {
    let ref_path = if git_ref.is_empty() {
        // Empty ref means index (staged)
        format!(":{}", file_path)
    } else {
        format!("{}:{}", git_ref, file_path)
    };

    let output = run_git_with_retry(|| {
        let mut cmd = Command::new("git");
        cmd.args(["show", &ref_path]).current_dir(repo_path);
        cmd
    })
    .context("Failed to run git show")?;

    if !output.status.success() {
        // File doesn't exist at this ref
        return Ok(None);
    }

    // Handle non-UTF8 content with lossy conversion
    Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
}

/// Get working tree file content
pub fn get_working_tree_file(repo_path: &Path, file_path: &str) -> Result<Option<String>> {
    let full_path = repo_path.join(file_path);
    if !full_path.exists() {
        return Ok(None);
    }

    match std::fs::read(&full_path) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Get file content as raw bytes at a specific ref (for binary files like images)
/// Use `:path` for staged content (index)
pub fn get_file_bytes_at_ref(
    repo_path: &Path,
    file_path: &str,
    git_ref: &str,
) -> Result<Option<Vec<u8>>> {
    let ref_path = if git_ref.is_empty() {
        // Empty ref means index (staged)
        format!(":{}", file_path)
    } else {
        format!("{}:{}", git_ref, file_path)
    };

    let output = run_git_with_retry(|| {
        let mut cmd = Command::new("git");
        cmd.args(["show", &ref_path]).current_dir(repo_path);
        cmd
    })
    .context("Failed to run git show")?;

    if !output.status.success() {
        // File doesn't exist at this ref
        return Ok(None);
    }

    Ok(Some(output.stdout))
}

/// Get working tree file content as raw bytes (for binary files like images)
pub fn get_working_tree_bytes(repo_path: &Path, file_path: &str) -> Result<Option<Vec<u8>>> {
    let full_path = repo_path.join(file_path);
    if !full_path.exists() {
        return Ok(None);
    }

    match std::fs::read(&full_path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// A file that has changes
#[derive(Debug, Clone)]
pub struct ChangedFile {
    pub path: String,
    /// Previous path if file was renamed/moved
    pub old_path: Option<String>,
}

/// Detect unstaged renames using a temporary index.
/// Creates a copy of the git index, adds untracked files with intent-to-add,
/// then uses git's rename detection to find matches between deleted and untracked files.
/// Returns Vec of (old_path, new_path) for detected renames.
fn detect_unstaged_renames(
    repo_path: &Path,
    deleted_files: &[String],
    untracked_files: &[String],
) -> Result<Vec<(String, String)>> {
    use std::io::Write;

    // Early exit if no potential renames
    if deleted_files.is_empty() || untracked_files.is_empty() {
        return Ok(Vec::new());
    }

    // Find the .git directory (handles both regular repos and worktrees)
    let git_dir_output = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(repo_path)
        .output()
        .context("Failed to find .git directory")?;

    if !git_dir_output.status.success() {
        return Ok(Vec::new());
    }

    let git_dir = repo_path.join(
        String::from_utf8_lossy(&git_dir_output.stdout).trim(),
    );
    let index_path = git_dir.join("index");

    // Create temp file for the index copy
    let mut temp_index = tempfile::NamedTempFile::new().context("Failed to create temp index")?;

    // Copy existing index to temp file
    if index_path.exists() {
        let index_content = std::fs::read(&index_path).context("Failed to read index")?;
        temp_index
            .write_all(&index_content)
            .context("Failed to write temp index")?;
        temp_index.flush()?;
    }

    let temp_index_path = temp_index.path().to_string_lossy().to_string();

    // Batch git add -N for all untracked files
    // Using --intent-to-add with the temp index
    let add_output = Command::new("git")
        .args(["add", "-N", "--"])
        .args(untracked_files)
        .env("GIT_INDEX_FILE", &temp_index_path)
        .current_dir(repo_path)
        .output()
        .context("Failed to run git add -N")?;

    if !add_output.status.success() {
        // If add fails, just return empty (don't break the whole refresh)
        return Ok(Vec::new());
    }

    // Run git diff with rename detection using the temp index
    let diff_output = Command::new("git")
        .args(["diff", "--name-status", "-M", "HEAD"])
        .env("GIT_INDEX_FILE", &temp_index_path)
        .current_dir(repo_path)
        .output()
        .context("Failed to run git diff with temp index")?;

    if !diff_output.status.success() {
        return Ok(Vec::new());
    }

    // Parse renames from the diff output
    let deleted_set: HashSet<&str> = deleted_files.iter().map(String::as_str).collect();
    let untracked_set: HashSet<&str> = untracked_files.iter().map(String::as_str).collect();

    let output_str = String::from_utf8_lossy(&diff_output.stdout);
    let mut renames = Vec::new();

    for line in output_str.lines() {
        if let Some(transition) = parse_diff_line(line)
            && let (Some(from), Some(to)) = (&transition.from, &transition.to)
            && from != to
            && deleted_set.contains(from.as_str())
            && untracked_set.contains(to.as_str())
        {
            renames.push((from.clone(), to.clone()));
        }
    }

    Ok(renames)
}

/// Get all files that have changes compared to merge-base, HEAD, index, or working tree
pub fn get_all_changed_files(repo_path: &Path, merge_base: &str) -> Result<Vec<ChangedFile>> {
    // Map from current path to optional old_path (for renames)
    let mut files: HashMap<String, Option<String>> = HashMap::new();

    // Track worktree-deleted and untracked files for unstaged rename detection
    let mut worktree_deleted: Vec<String> = Vec::new();
    let mut untracked: Vec<String> = Vec::new();

    // 1. Get committed changes (merge-base to HEAD) with rename detection
    // Skip if merge_base is empty (no commits yet)
    if !merge_base.is_empty()
        && let Ok(transitions) = get_diff_transitions(repo_path, merge_base, "HEAD")
    {
        for t in transitions {
            // Prefer t.to (destination path), fall back to t.from (for deletions)
            // Consume the Options directly to avoid cloning
            match (t.to, t.from) {
                (Some(to), Some(from)) if to != from => {
                    // Rename: use 'to' as path, 'from' as old_path
                    files.insert(to, Some(from));
                }
                (Some(to), _) => {
                    files.insert(to, None);
                }
                (None, Some(from)) => {
                    files.insert(from, None);
                }
                (None, None) => {}
            }
        }
    }

    // 2. Get staged changes (HEAD to index) and unstaged changes (index to working tree)
    // Use -uall to show individual files in untracked directories
    // Use retry logic to handle transient index.lock contention
    let status_output = run_git_with_retry(|| {
        let mut cmd = Command::new("git");
        cmd.args(["status", "--porcelain=v1", "-uall"])
            .current_dir(repo_path);
        cmd
    })
    .context("Failed to run git status")?;

    if status_output.status.success() {
        let status_str = String::from_utf8_lossy(&status_output.stdout);
        for line in status_str.lines() {
            if line.len() < 3 {
                continue;
            }

            let status_codes = &line[..2];
            let path_part = line[3..].to_string();

            // Track worktree-deleted files (second char is 'D') and untracked files (??)
            // for unstaged rename detection
            if status_codes.as_bytes()[1] == b'D' {
                worktree_deleted.push(path_part.clone());
            } else if status_codes == "??" {
                untracked.push(path_part.clone());
            }

            // Handle renames which have "old -> new" format (staged renames)
            let (path, old_path) = if path_part.contains(" -> ") {
                let parts: Vec<&str> = path_part.split(" -> ").collect();
                (parts[1].to_string(), Some(parts[0].to_string()))
            } else {
                (path_part, None)
            };

            // Only update old_path if we don't already have one (committed rename takes precedence)
            files.entry(path).or_insert(old_path);
        }
    }

    // 3. Detect unstaged renames (worktree mv without staging)
    // Only runs when both deleted and untracked files exist
    if !worktree_deleted.is_empty()
        && !untracked.is_empty()
        && let Ok(renames) = detect_unstaged_renames(repo_path, &worktree_deleted, &untracked)
    {
        for (old_path, new_path) in renames {
            // Update the new file to reference the old path
            files.insert(new_path, Some(old_path.clone()));
            // Remove the deleted file entry (it's now part of the rename)
            files.remove(&old_path);
        }
    }

    let mut result: Vec<ChangedFile> = files
        .into_iter()
        .map(|(path, old_path)| ChangedFile { path, old_path })
        .collect();
    result.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(result)
}

/// Represents a file transition in a git diff.
/// All git diff statuses (A/D/M/R) describe a transition from one state to another.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileTransition {
    /// Source path (None for added files)
    from: Option<String>,
    /// Destination path (None for deleted files)
    to: Option<String>,
}

impl FileTransition {
    /// Get the current/relevant path for this transition.
    /// Prefers the destination path, falls back to source for deletions.
    #[cfg(test)]
    fn current_path(&self) -> Option<&str> {
        self.to.as_deref().or(self.from.as_deref())
    }
}

/// Parse a single line of `git diff --name-status` output into a FileTransition.
/// Returns None for unrecognized formats.
fn parse_diff_line(line: &str) -> Option<FileTransition> {
    let parts: Vec<&str> = line.split('\t').collect();
    match parts.as_slice() {
        [status, path] if status.starts_with('A') => Some(FileTransition {
            from: None,
            to: Some(path.to_string()),
        }),
        [status, path] if status.starts_with('D') => Some(FileTransition {
            from: Some(path.to_string()),
            to: None,
        }),
        [status, path] if status.starts_with('M') => Some(FileTransition {
            from: Some(path.to_string()),
            to: Some(path.to_string()),
        }),
        [status, old_path, new_path] if status.starts_with('R') => Some(FileTransition {
            from: Some(old_path.to_string()),
            to: Some(new_path.to_string()),
        }),
        _ => None,
    }
}

/// Get file transitions between two refs with rename detection enabled
fn get_diff_transitions(repo_path: &Path, from: &str, to: &str) -> Result<Vec<FileTransition>> {
    let output = run_git_with_retry(|| {
        let mut cmd = Command::new("git");
        cmd.args(["diff", "--name-status", "-M", from, to])
            .current_dir(repo_path);
        cmd
    })
    .context("Failed to run git diff --name-status -M")?;

    if !output.status.success() {
        return Err(anyhow!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let output_str = String::from_utf8_lossy(&output.stdout);
    let transitions: Vec<FileTransition> = output_str
        .lines()
        .filter_map(parse_diff_line)
        .collect();

    Ok(transitions)
}

/// Check if a file is binary (single file check - prefer get_binary_files for batch operations)
pub fn is_binary_file(repo_path: &Path, file_path: &str) -> bool {
    let output = Command::new("git")
        .args(["diff", "--numstat", "--", file_path])
        .current_dir(repo_path)
        .output();

    match output {
        Ok(o) => {
            let s = String::from_utf8_lossy(&o.stdout);
            // Binary files show as "-\t-\t" in numstat
            s.starts_with("-\t-\t")
        }
        Err(_) => false,
    }
}

/// Get all binary files in the diff between merge_base and working tree.
/// Returns a HashSet of file paths that are binary.
/// This is more efficient than calling is_binary_file() for each file.
pub fn get_binary_files(repo_path: &Path, merge_base: &str) -> std::collections::HashSet<String> {
    use std::collections::HashSet;

    let mut binaries = HashSet::new();

    // Compare merge_base to working tree (covers committed + staged + unstaged changes)
    // If merge_base is empty (new repo), check against empty tree
    let base_ref = if merge_base.is_empty() {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904" // git's empty tree SHA
    } else {
        merge_base
    };

    // git diff --numstat <ref> (with no second ref) compares ref to working tree
    let output = Command::new("git")
        .args(["diff", "--numstat", base_ref])
        .current_dir(repo_path)
        .output();

    if let Ok(o) = output {
        let s = String::from_utf8_lossy(&o.stdout);
        for line in s.lines() {
            // Binary files show as "-\t-\tfilename" in numstat output
            // Renames show as "-\t-\told => new"
            if let Some(path) = line.strip_prefix("-\t-\t") {
                let actual_path = if path.contains(" => ") {
                    // Extract the new filename from "old => new" format
                    path.split(" => ").last().unwrap_or(path)
                } else {
                    path
                };
                binaries.insert(actual_path.to_string());
            }
        }
    }

    binaries
}

pub fn fetch_base_branch(repo_path: &Path, base_branch: &str) -> Result<()> {
    use std::time::Duration;
    use std::io::Read;

    let current = get_current_branch(repo_path).ok().flatten();
    let on_base_branch = current.as_deref() == Some(base_branch);

    let refspec = format!("{}:{}", base_branch, base_branch);
    let fetch_arg = if on_base_branch { base_branch } else { &refspec };

    let mut child = Command::new("git")
        .args(["-c", "gc.auto=0", "fetch", "--no-tags", "origin", fetch_arg])
        .current_dir(repo_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("Failed to spawn git fetch")?;

    let timeout = Duration::from_secs(30);
    let start = std::time::Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    let mut stderr = String::new();
                    if let Some(mut err) = child.stderr.take() {
                        let _ = err.read_to_string(&mut stderr);
                    }
                    return Err(anyhow!("git fetch failed: {}", stderr));
                }
                return Ok(());
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    return Err(anyhow!("git fetch timed out"));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(anyhow!("Error waiting for git fetch: {}", e)),
        }
    }
}

/// Check for merge conflicts using git merge-tree.
/// Requires Git 2.38+ for --write-tree flag; returns Ok(false) on older versions.
pub fn has_merge_conflicts(repo_path: &Path, base_branch: &str, git_version: &GitVersion) -> Result<bool> {
    // merge-tree --write-tree requires Git 2.38+
    if !git_version.at_least(MERGE_TREE_MIN_VERSION.0, MERGE_TREE_MIN_VERSION.1) {
        return Ok(false);
    }

    let remote_ref = format!("origin/{}", base_branch);

    let remote_exists = Command::new("git")
        .args(["rev-parse", "--verify", &remote_ref])
        .current_dir(repo_path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !remote_exists {
        return Ok(false);
    }

    let output = Command::new("git")
        .args(["merge-tree", "--write-tree", &remote_ref, "HEAD"])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git merge-tree")?;

    Ok(!output.status.success())
}

/// Get the current branch name
pub fn get_current_branch(repo_path: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(repo_path)
        .output()
        .context("Failed to get current branch")?;

    if !output.status.success() {
        return Ok(None);
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch == "HEAD" {
        Ok(None)
    } else {
        Ok(Some(branch))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Refresh pipeline (git-specific implementation)
// ─────────────────────────────────────────────────────────────────────────────

use super::{vcs_thread_pool, PARALLEL_THRESHOLD};

enum FileProcessResult {
    Diff(FileDiff),
    Binary { path: String },
    Image { path: String },
}

struct FileContents {
    base: Option<String>,
    head: Option<String>,
    index: Option<String>,
    working: Option<String>,
}

impl FileContents {
    fn fetch(repo_path: &Path, file_path: &str, old_path: Option<&str>, merge_base: &str) -> Self {
        let base_path = old_path.unwrap_or(file_path);

        let base = if merge_base.is_empty() {
            None
        } else {
            get_file_at_ref(repo_path, base_path, merge_base)
                .ok()
                .flatten()
        };

        let head = get_file_at_ref(repo_path, file_path, "HEAD")
            .ok()
            .flatten()
            .or_else(|| {
                old_path.and_then(|p| get_file_at_ref(repo_path, p, "HEAD").ok().flatten())
            });

        let index = get_file_at_ref(repo_path, file_path, "")
            .ok()
            .flatten()
            .or_else(|| {
                old_path.and_then(|p| get_file_at_ref(repo_path, p, "").ok().flatten())
            });

        Self {
            base,
            head,
            index,
            working: get_working_tree_file(repo_path, file_path)
                .ok()
                .flatten(),
        }
    }

    fn all_equal(&self) -> bool {
        self.base == self.working && self.base == self.head && self.base == self.index
    }
}

fn process_single_file(
    repo_path: &Path,
    file_path: &str,
    old_path: Option<&str>,
    merge_base: &str,
    binary_files: &HashSet<String>,
) -> FileProcessResult {
    if binary_files.contains(file_path) {
        if is_image_file(file_path) {
            return FileProcessResult::Image {
                path: file_path.to_string(),
            };
        }
        return FileProcessResult::Binary {
            path: file_path.to_string(),
        };
    }

    let contents = FileContents::fetch(repo_path, file_path, old_path, merge_base);
    let file_diff = compute_four_way_diff(DiffInput {
        path: file_path,
        base: contents.base.as_deref(),
        head: contents.head.as_deref(),
        index: contents.index.as_deref(),
        working: contents.working.as_deref(),
        old_path,
    });

    FileProcessResult::Diff(file_diff)
}

/// Check if file_path was renamed from another path in committed changes.
fn find_rename_source(repo_path: &Path, file_path: &str, merge_base: &str) -> Option<String> {
    if merge_base.is_empty() {
        return None;
    }
    let transitions = get_diff_transitions(repo_path, merge_base, "HEAD").ok()?;
    transitions.into_iter().find_map(|t| {
        if t.to.as_deref() == Some(file_path) && t.from.as_deref() != Some(file_path) {
            t.from
        } else {
            None
        }
    })
}

fn git_compute_single_file_diff(
    repo_path: &Path,
    file_path: &str,
    old_path: Option<&str>,
    merge_base: &str,
) -> Option<FileDiff> {
    if is_binary_file(repo_path, file_path) {
        return None;
    }

    let contents = FileContents::fetch(repo_path, file_path, old_path, merge_base);

    if contents.all_equal() {
        return None;
    }

    Some(compute_four_way_diff(DiffInput {
        path: file_path,
        base: contents.base.as_deref(),
        head: contents.head.as_deref(),
        index: contents.index.as_deref(),
        working: contents.working.as_deref(),
        old_path,
    }))
}

fn git_compute_refresh(
    repo_path: &Path,
    base_branch: &str,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<RefreshResult> {
    let merge_base = get_merge_base_preferring_origin(repo_path, base_branch)
        .unwrap_or_default();

    if cancel_flag.load(Ordering::Relaxed) {
        return Err(anyhow!("refresh cancelled"));
    }

    let (changed_files_result, binary_files) = std::thread::scope(|s| {
        let changed_handle = s.spawn(|| get_all_changed_files(repo_path, &merge_base));
        let binary_handle = s.spawn(|| get_binary_files(repo_path, &merge_base));

        (
            changed_handle.join().expect("changed files thread panicked"),
            binary_handle.join().expect("binary files thread panicked"),
        )
    });

    let changed_files = changed_files_result.context("Failed to get changed files")?;

    let results: Vec<FileProcessResult> = if changed_files.len() >= PARALLEL_THRESHOLD {
        vcs_thread_pool().install(|| {
            changed_files
                .par_iter()
                .map(|file| process_single_file(repo_path, &file.path, file.old_path.as_deref(), &merge_base, &binary_files))
                .collect()
        })
    } else {
        changed_files
            .iter()
            .map(|file| process_single_file(repo_path, &file.path, file.old_path.as_deref(), &merge_base, &binary_files))
            .collect()
    };

    if cancel_flag.load(Ordering::Relaxed) {
        return Err(anyhow!("refresh cancelled"));
    }

    let mut files = Vec::new();
    let mut lines = Vec::new();

    for result in results {
        match result {
            FileProcessResult::Diff(file_diff) => {
                lines.extend(file_diff.lines.iter().cloned());
                lines.push(DiffLine::new(LineSource::Base, String::new(), ' ', None));
                files.push(file_diff);
            }
            FileProcessResult::Binary { path } => {
                let header = DiffLine::file_header(&path);
                let marker = DiffLine::new(
                    LineSource::Base,
                    "[binary file]".to_string(),
                    ' ',
                    None,
                );
                lines.push(header.clone());
                lines.push(marker.clone());
                files.push(FileDiff {
                    lines: vec![header, marker],
                });
            }
            FileProcessResult::Image { path } => {
                let header = DiffLine::file_header(&path);
                let marker = DiffLine::image_marker(&path);
                lines.push(header.clone());
                lines.push(marker.clone());
                files.push(FileDiff {
                    lines: vec![header, marker],
                });
            }
        }
    }

    let current_branch = get_current_branch(repo_path).unwrap_or(None);

    let metrics = DiffMetrics {
        total_lines: lines.len(),
        file_count: files.len(),
    };

    let file_paths: Vec<&str> = files
        .iter()
        .filter_map(|f| f.lines.first())
        .filter_map(|l| l.file_path.as_deref())
        .collect();
    let file_links = compute_file_links(&file_paths);

    Ok(RefreshResult {
        files,
        lines,
        base_identifier: merge_base,
        base_label: Some(base_branch.to_string()),
        current_branch,
        metrics,
        file_links,
        stack_position: None,
    })
}

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
            vcs_name: "git".to_string(),
        })
    }

    fn refresh(&self, cancel_flag: &Arc<AtomicBool>) -> Result<RefreshResult> {
        git_compute_refresh(&self.repo_path, &self.base_branch, cancel_flag)
    }

    fn single_file_diff(&self, file_path: &str) -> Option<FileDiff> {
        let merge_base = get_merge_base_preferring_origin(&self.repo_path, &self.base_branch)
            .unwrap_or_default();
        let old_path = find_rename_source(&self.repo_path, file_path, &merge_base);
        git_compute_single_file_diff(&self.repo_path, file_path, old_path.as_deref(), &merge_base)
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

    fn vcs_name(&self) -> &str {
        "git"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::VcsEventType;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn test_parse_git_version_standard() {
        let version = parse_git_version("git version 2.34.1").unwrap();
        assert_eq!(version.major, 2);
        assert_eq!(version.minor, 34);
        assert_eq!(version.patch, 1);
    }

    #[test]
    fn test_parse_git_version_apple() {
        let version = parse_git_version("git version 2.50.1 (Apple Git-155)").unwrap();
        assert_eq!(version.major, 2);
        assert_eq!(version.minor, 50);
        assert_eq!(version.patch, 1);
    }

    #[test]
    fn test_parse_git_version_no_patch() {
        let version = parse_git_version("git version 2.38").unwrap();
        assert_eq!(version.major, 2);
        assert_eq!(version.minor, 38);
        assert_eq!(version.patch, 0);
    }

    #[test]
    fn test_parse_git_version_windows() {
        // Windows Git for Windows format
        let version = parse_git_version("git version 2.39.2.windows.1").unwrap();
        assert_eq!(version.major, 2);
        assert_eq!(version.minor, 39);
        // patch parsing stops at non-numeric suffix
        assert_eq!(version.patch, 2);
    }

    #[test]
    fn test_parse_git_version_ubuntu() {
        // Ubuntu/Debian format: "2.34.1" is the version part before any suffix
        // The split by '.' gives ["2", "34", "1", "ubuntu1"]
        // patch = "1" parses fine
        let version = parse_git_version("git version 2.34.1.ubuntu1").unwrap();
        assert_eq!(version.major, 2);
        assert_eq!(version.minor, 34);
        assert_eq!(version.patch, 1);
    }

    #[test]
    fn test_parse_git_version_with_newline() {
        // Real output includes trailing newline
        let version = parse_git_version("git version 2.34.1\n").unwrap();
        assert_eq!(version.major, 2);
        assert_eq!(version.minor, 34);
        assert_eq!(version.patch, 1);
    }

    #[test]
    fn test_parse_git_version_old_git() {
        let version = parse_git_version("git version 1.8.0").unwrap();
        assert_eq!(version.major, 1);
        assert_eq!(version.minor, 8);
        assert_eq!(version.patch, 0);
    }

    #[test]
    fn test_parse_git_version_invalid_no_prefix() {
        let result = parse_git_version("2.34.1");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_git_version_invalid_empty() {
        let result = parse_git_version("");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_git_version_invalid_no_minor() {
        let result = parse_git_version("git version 2");
        assert!(result.is_err());
    }

    #[test]
    fn test_git_version_at_least() {
        let v238 = GitVersion { major: 2, minor: 38, patch: 0 };
        assert!(v238.at_least(2, 38));
        assert!(v238.at_least(2, 37));
        assert!(v238.at_least(2, 25));
        assert!(!v238.at_least(2, 39));
        assert!(!v238.at_least(3, 0));

        // Test major version comparison
        let v3 = GitVersion { major: 3, minor: 0, patch: 0 };
        assert!(v3.at_least(2, 99));
        assert!(v3.at_least(3, 0));
        assert!(!v3.at_least(3, 1));
    }

    #[test]
    fn test_git_version_display() {
        let version = GitVersion { major: 2, minor: 38, patch: 1 };
        assert_eq!(format!("{}", version), "2.38.1");
    }

    #[test]
    fn test_get_git_version_succeeds() {
        // Should succeed on any system with git installed
        let version = get_git_version().unwrap();
        assert!(version.major >= 1);
    }

    #[test]
    fn test_parse_diff_line_added() {
        let line = "A\tpath/to/new_file.rs";
        let result = parse_diff_line(line);
        assert_eq!(result, Some(FileTransition {
            from: None,
            to: Some("path/to/new_file.rs".to_string()),
        }));
    }

    #[test]
    fn test_parse_diff_line_deleted() {
        let line = "D\tpath/to/deleted_file.rs";
        let result = parse_diff_line(line);
        assert_eq!(result, Some(FileTransition {
            from: Some("path/to/deleted_file.rs".to_string()),
            to: None,
        }));
    }

    #[test]
    fn test_parse_diff_line_modified() {
        let line = "M\tpath/to/modified_file.rs";
        let result = parse_diff_line(line);
        assert_eq!(result, Some(FileTransition {
            from: Some("path/to/modified_file.rs".to_string()),
            to: Some("path/to/modified_file.rs".to_string()),
        }));
    }

    #[test]
    fn test_parse_diff_line_renamed() {
        let line = "R100\told/path.rs\tnew/path.rs";
        let result = parse_diff_line(line);
        assert_eq!(result, Some(FileTransition {
            from: Some("old/path.rs".to_string()),
            to: Some("new/path.rs".to_string()),
        }));
    }

    #[test]
    fn test_parse_diff_line_renamed_partial_similarity() {
        let line = "R075\told/path.rs\tnew/path.rs";
        let result = parse_diff_line(line);
        assert_eq!(result, Some(FileTransition {
            from: Some("old/path.rs".to_string()),
            to: Some("new/path.rs".to_string()),
        }));
    }

    #[test]
    fn test_parse_diff_line_malformed_empty() {
        assert_eq!(parse_diff_line(""), None);
    }

    #[test]
    fn test_parse_diff_line_malformed_single_part() {
        assert_eq!(parse_diff_line("M"), None);
    }

    #[test]
    fn test_parse_diff_line_unrecognized_status() {
        // Unknown status should return None
        assert_eq!(parse_diff_line("X\tsome/file.rs"), None);
    }

    #[test]
    fn test_file_transition_current_path_added() {
        let t = FileTransition {
            from: None,
            to: Some("new_file.rs".to_string()),
        };
        assert_eq!(t.current_path(), Some("new_file.rs"));
    }

    #[test]
    fn test_file_transition_current_path_deleted() {
        let t = FileTransition {
            from: Some("deleted_file.rs".to_string()),
            to: None,
        };
        assert_eq!(t.current_path(), Some("deleted_file.rs"));
    }

    #[test]
    fn test_file_transition_current_path_modified() {
        let t = FileTransition {
            from: Some("file.rs".to_string()),
            to: Some("file.rs".to_string()),
        };
        assert_eq!(t.current_path(), Some("file.rs"));
    }

    #[test]
    fn test_file_transition_current_path_renamed() {
        let t = FileTransition {
            from: Some("old.rs".to_string()),
            to: Some("new.rs".to_string()),
        };
        // Should prefer destination (new path)
        assert_eq!(t.current_path(), Some("new.rs"));
    }

    fn git_cmd(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git command failed to execute");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn create_test_repo() -> tempfile::TempDir {
        create_test_repo_with_content("initial\n")
    }

    fn create_test_repo_with_content(content: &str) -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path();

        git_cmd(path, &["init"]);
        git_cmd(path, &["config", "user.email", "test@test.com"]);
        git_cmd(path, &["config", "user.name", "Test"]);

        fs::write(path.join("file.txt"), content).unwrap();
        git_cmd(path, &["add", "."]);
        git_cmd(path, &["commit", "-m", "initial"]);
        git_cmd(path, &["branch", "-M", "main"]);

        temp
    }

    fn create_repo_with_origin() -> (tempfile::TempDir, tempfile::TempDir) {
        let origin = create_test_repo();
        let clone_dir = tempfile::tempdir().unwrap();

        Command::new("git")
            .args(["clone", origin.path().to_str().unwrap(), "."])
            .current_dir(clone_dir.path())
            .output()
            .expect("clone failed");

        // Configure git user in clone (not inherited from origin's local config)
        git_cmd(clone_dir.path(), &["config", "user.email", "test@test.com"]);
        git_cmd(clone_dir.path(), &["config", "user.name", "Test"]);

        (origin, clone_dir)
    }

    #[test]
    fn test_fetch_base_branch_no_remote() {
        let temp = create_test_repo();
        let result = fetch_base_branch(temp.path(), "main");
        assert!(result.is_err());
    }

    #[test]
    fn test_fetch_base_branch_with_remote() {
        let (origin, clone) = create_repo_with_origin();

        fs::write(origin.path().join("file.txt"), "updated\n").unwrap();
        git_cmd(origin.path(), &["add", "."]);
        git_cmd(origin.path(), &["commit", "-m", "update"]);

        let result = fetch_base_branch(clone.path(), "main");
        assert!(result.is_ok());
    }

    #[test]
    fn test_has_merge_conflicts_no_remote() {
        let temp = create_test_repo();
        let version = get_git_version().unwrap();
        let result = has_merge_conflicts(temp.path(), "main", &version);
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn test_has_merge_conflicts_clean() {
        let (origin, clone) = create_repo_with_origin();

        fs::write(origin.path().join("other.txt"), "new file\n").unwrap();
        git_cmd(origin.path(), &["add", "."]);
        git_cmd(origin.path(), &["commit", "-m", "add other"]);

        fetch_base_branch(clone.path(), "main").unwrap();

        let version = get_git_version().unwrap();
        let result = has_merge_conflicts(clone.path(), "main", &version);
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn test_has_merge_conflicts_with_conflict() {
        let (origin, clone) = create_repo_with_origin();

        fs::write(origin.path().join("file.txt"), "origin change\n").unwrap();
        git_cmd(origin.path(), &["add", "."]);
        git_cmd(origin.path(), &["commit", "-m", "origin update"]);

        fs::write(clone.path().join("file.txt"), "local change\n").unwrap();
        git_cmd(clone.path(), &["add", "."]);
        git_cmd(clone.path(), &["commit", "-m", "local update"]);

        fetch_base_branch(clone.path(), "main").unwrap();

        let version = get_git_version().unwrap();
        // Skip assertion if git < 2.38 (merge-tree --write-tree not available)
        if version.at_least(2, 38) {
            let result = has_merge_conflicts(clone.path(), "main", &version);
            assert!(result.is_ok());
            assert!(result.unwrap());
        }
    }

    #[test]
    fn test_has_merge_conflicts_skips_on_old_git() {
        let temp = create_test_repo();
        // Simulate old git version
        let old_version = GitVersion { major: 2, minor: 30, patch: 0 };
        let result = has_merge_conflicts(temp.path(), "main", &old_version);
        assert!(result.is_ok());
        // Should return false (skip) on old git
        assert!(!result.unwrap());
    }

    #[test]
    fn test_has_merge_conflicts_version_boundary() {
        let temp = create_test_repo();

        // Git 2.37.x should skip (returns false)
        let v237 = GitVersion { major: 2, minor: 37, patch: 99 };
        let result = has_merge_conflicts(temp.path(), "main", &v237);
        assert!(result.is_ok());
        assert!(!result.unwrap(), "Git 2.37 should skip conflict detection");

        // Git 2.38.0 should attempt detection (returns false here because no remote)
        let v238 = GitVersion { major: 2, minor: 38, patch: 0 };
        let result = has_merge_conflicts(temp.path(), "main", &v238);
        assert!(result.is_ok());
        // Still false because no remote, but it attempted the check
        assert!(!result.unwrap());
    }

    #[test]
    fn test_get_all_changed_files_includes_files_in_new_directories() {
        let temp = create_test_repo();
        let merge_base = get_merge_base(temp.path(), "main").unwrap();

        fs::create_dir(temp.path().join("new_folder")).unwrap();
        fs::write(temp.path().join("new_folder/file1.txt"), "content1\n").unwrap();
        fs::write(temp.path().join("new_folder/file2.txt"), "content2\n").unwrap();

        let changed = get_all_changed_files(temp.path(), &merge_base).unwrap();
        let paths: Vec<&str> = changed.iter().map(|f| f.path.as_str()).collect();

        assert!(paths.contains(&"new_folder/file1.txt"));
        assert!(paths.contains(&"new_folder/file2.txt"));
    }

    #[test]
    fn test_fetch_updates_local_branch_when_not_checked_out() {
        let (origin, clone) = create_repo_with_origin();

        git_cmd(clone.path(), &["checkout", "-b", "feature"]);

        let local_before = Command::new("git")
            .args(["rev-parse", "main"])
            .current_dir(clone.path())
            .output()
            .unwrap();
        let before_sha = String::from_utf8_lossy(&local_before.stdout).trim().to_string();

        fs::write(origin.path().join("new.txt"), "origin update\n").unwrap();
        git_cmd(origin.path(), &["add", "."]);
        git_cmd(origin.path(), &["commit", "-m", "origin update"]);

        fetch_base_branch(clone.path(), "main").unwrap();

        let local_after = Command::new("git")
            .args(["rev-parse", "main"])
            .current_dir(clone.path())
            .output()
            .unwrap();
        let after_sha = String::from_utf8_lossy(&local_after.stdout).trim().to_string();

        assert_ne!(before_sha, after_sha, "local main should update after fetch when not checked out");

        let origin_sha = Command::new("git")
            .args(["rev-parse", "origin/main"])
            .current_dir(clone.path())
            .output()
            .unwrap();
        let origin_sha = String::from_utf8_lossy(&origin_sha.stdout).trim().to_string();

        assert_eq!(after_sha, origin_sha, "local main should match origin/main after fetch");
    }

    #[test]
    fn test_fetch_updates_origin_when_on_base_branch() {
        let (origin, clone) = create_repo_with_origin();

        let origin_before = Command::new("git")
            .args(["rev-parse", "origin/main"])
            .current_dir(clone.path())
            .output()
            .unwrap();
        let before_sha = String::from_utf8_lossy(&origin_before.stdout).trim().to_string();

        fs::write(origin.path().join("new.txt"), "origin update\n").unwrap();
        git_cmd(origin.path(), &["add", "."]);
        git_cmd(origin.path(), &["commit", "-m", "origin update"]);

        fetch_base_branch(clone.path(), "main").unwrap();

        let origin_after = Command::new("git")
            .args(["rev-parse", "origin/main"])
            .current_dir(clone.path())
            .output()
            .unwrap();
        let after_sha = String::from_utf8_lossy(&origin_after.stdout).trim().to_string();

        assert_ne!(before_sha, after_sha, "origin/main should update after fetch even when on main");
    }

    #[test]
    fn test_get_all_changed_files_with_empty_merge_base() {
        // Simulates a repo with no commits yet (empty merge_base)
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path();

        // Initialize empty repo
        Command::new("git")
            .args(["init"])
            .current_dir(repo_path)
            .output()
            .expect("failed to init git repo");

        // Add an untracked file
        fs::write(repo_path.join("new_file.txt"), "content\n").unwrap();

        // Should not panic with empty merge_base
        let result = get_all_changed_files(repo_path, "");
        assert!(result.is_ok());

        // Should find the untracked file via git status
        let changed = result.unwrap();
        let paths: Vec<&str> = changed.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"new_file.txt"));
    }

    #[test]
    fn test_is_transient_error_index_lock() {
        // index.lock is the most common transient error
        assert!(is_transient_error(
            "fatal: Unable to create '/path/.git/index.lock': File exists."
        ));
    }

    #[test]
    fn test_is_transient_error_other_lock() {
        // Other lock files should also be retried
        assert!(is_transient_error(
            "Unable to create '/path/.git/refs/heads/main.lock': File exists"
        ));
    }

    #[test]
    fn test_is_transient_error_not_lock() {
        // Non-lock errors should not be retried
        assert!(!is_transient_error("fatal: not a git repository"));
        assert!(!is_transient_error("fatal: pathspec 'foo' did not match any files"));
        assert!(!is_transient_error(""));
    }

    #[test]
    fn test_run_git_with_retry_succeeds_on_first_attempt() {
        // A simple git command that should succeed immediately
        let output = run_git_with_retry(|| {
            let mut cmd = Command::new("git");
            cmd.args(["--version"]);
            cmd
        })
        .unwrap();

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("git version"));
    }

    #[test]
    fn test_run_git_with_retry_returns_failure_for_permanent_error() {
        // A command that fails permanently (not transient) should return the error
        let output = run_git_with_retry(|| {
            let mut cmd = Command::new("git");
            cmd.args(["rev-parse", "--verify", "nonexistent-branch-12345"]);
            cmd
        })
        .unwrap();

        // Should fail because branch doesn't exist
        assert!(!output.status.success());
    }

    #[test]
    fn test_get_binary_files_empty_repo() {
        let temp = create_test_repo();
        let merge_base = get_merge_base(temp.path(), "main").unwrap();

        // No binary files in a clean repo
        let binaries = get_binary_files(temp.path(), &merge_base);
        assert!(binaries.is_empty());
    }

    #[test]
    fn test_get_binary_files_detects_binary() {
        let temp = create_test_repo();
        let merge_base = get_merge_base(temp.path(), "main").unwrap();

        // Add a binary file (null bytes make it binary)
        fs::write(temp.path().join("binary.bin"), &[0u8, 1, 2, 255, 254, 253]).unwrap();
        // Must be staged/tracked for git diff to see it
        git_cmd(temp.path(), &["add", "binary.bin"]);

        let binaries = get_binary_files(temp.path(), &merge_base);
        assert!(binaries.contains("binary.bin"));
    }

    #[test]
    fn test_get_binary_files_ignores_text_files() {
        let temp = create_test_repo();
        let merge_base = get_merge_base(temp.path(), "main").unwrap();

        // Modify a text file
        fs::write(temp.path().join("file.txt"), "modified content\n").unwrap();

        let binaries = get_binary_files(temp.path(), &merge_base);
        // Text files should not be in binary set
        assert!(!binaries.contains("file.txt"));
    }

    #[test]
    fn test_get_binary_files_handles_renamed_binary() {
        let temp = create_test_repo();

        // Create and commit a binary file
        fs::write(temp.path().join("original.bin"), &[0u8, 1, 2, 255]).unwrap();
        git_cmd(temp.path(), &["add", "original.bin"]);
        git_cmd(temp.path(), &["commit", "-m", "add binary"]);

        let merge_base = get_merge_base(temp.path(), "main").unwrap();

        // Rename the binary file
        fs::rename(
            temp.path().join("original.bin"),
            temp.path().join("renamed.bin"),
        )
        .unwrap();
        git_cmd(temp.path(), &["add", "."]);

        let binaries = get_binary_files(temp.path(), &merge_base);
        // Should detect the new name, not "original.bin => renamed.bin"
        assert!(binaries.contains("renamed.bin"));
        assert!(!binaries.contains("original.bin => renamed.bin"));
    }

    #[test]
    fn test_get_binary_files_with_empty_merge_base() {
        let temp = TempDir::new().unwrap();
        git_cmd(temp.path(), &["init"]);
        git_cmd(temp.path(), &["config", "user.email", "test@test.com"]);
        git_cmd(temp.path(), &["config", "user.name", "Test"]);

        // Add a binary file before first commit
        fs::write(temp.path().join("binary.bin"), &[0u8, 1, 2]).unwrap();
        git_cmd(temp.path(), &["add", "."]);
        git_cmd(temp.path(), &["commit", "-m", "initial"]);

        // Use empty merge_base (simulates new repo scenario)
        let binaries = get_binary_files(temp.path(), "");
        assert!(binaries.contains("binary.bin"));
    }

    #[test]
    fn test_detect_unstaged_rename() {
        let temp = create_test_repo();
        let merge_base = get_merge_base(temp.path(), "main").unwrap();

        // Rename file using filesystem mv (not git mv)
        fs::rename(
            temp.path().join("file.txt"),
            temp.path().join("renamed.txt"),
        )
        .unwrap();

        let changed = get_all_changed_files(temp.path(), &merge_base).unwrap();

        // Should detect as a rename, not separate delete + add
        assert_eq!(changed.len(), 1, "Should be one renamed file, not two");
        let renamed = &changed[0];
        assert_eq!(renamed.path, "renamed.txt");
        assert_eq!(renamed.old_path, Some("file.txt".to_string()));
    }

    #[test]
    fn test_detect_unstaged_rename_with_content_change() {
        let temp = TempDir::new().unwrap();
        let path = temp.path();

        git_cmd(path, &["init"]);
        git_cmd(path, &["config", "user.email", "test@test.com"]);
        git_cmd(path, &["config", "user.name", "Test"]);

        // Create a larger file so small changes stay within 50% similarity
        let original_content = "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\n";
        fs::write(path.join("file.txt"), original_content).unwrap();
        git_cmd(path, &["add", "."]);
        git_cmd(path, &["commit", "-m", "initial"]);
        git_cmd(path, &["branch", "-M", "main"]);

        let merge_base = get_merge_base(path, "main").unwrap();

        // Rename file and modify content slightly (add one line)
        fs::remove_file(path.join("file.txt")).unwrap();
        fs::write(
            path.join("renamed.txt"),
            format!("{}line 9\n", original_content),
        )
        .unwrap();

        let changed = get_all_changed_files(path, &merge_base).unwrap();

        // Git's rename detection should still match (>50% similarity)
        assert_eq!(changed.len(), 1, "Should detect as rename despite small change");
        let renamed = &changed[0];
        assert_eq!(renamed.path, "renamed.txt");
        assert_eq!(renamed.old_path, Some("file.txt".to_string()));
    }

    #[test]
    fn test_no_rename_detection_when_only_deleted() {
        let temp = create_test_repo();
        let merge_base = get_merge_base(temp.path(), "main").unwrap();

        // Only delete, no new files
        fs::remove_file(temp.path().join("file.txt")).unwrap();

        let changed = get_all_changed_files(temp.path(), &merge_base).unwrap();

        // Should be a plain deletion
        assert_eq!(changed.len(), 1);
        let deleted = &changed[0];
        assert_eq!(deleted.path, "file.txt");
        assert!(deleted.old_path.is_none());
    }

    #[test]
    fn test_no_rename_detection_when_only_new_file() {
        let temp = create_test_repo();
        let merge_base = get_merge_base(temp.path(), "main").unwrap();

        // Only add, no deletions
        fs::write(temp.path().join("new_file.txt"), "new content\n").unwrap();

        let changed = get_all_changed_files(temp.path(), &merge_base).unwrap();

        // Should be a plain addition
        assert_eq!(changed.len(), 1);
        let added = &changed[0];
        assert_eq!(added.path, "new_file.txt");
        assert!(added.old_path.is_none());
    }

    #[test]
    fn test_staged_rename_with_git_mv() {
        let temp = create_test_repo();
        let merge_base = get_merge_base(temp.path(), "main").unwrap();

        // Use git mv to rename (creates a staged rename)
        git_cmd(temp.path(), &["mv", "file.txt", "staged_rename.txt"]);

        let changed = get_all_changed_files(temp.path(), &merge_base).unwrap();

        // Should detect as a staged rename
        assert_eq!(changed.len(), 1, "Should be one renamed file");
        let renamed = &changed[0];
        assert_eq!(renamed.path, "staged_rename.txt");
        assert_eq!(renamed.old_path, Some("file.txt".to_string()));
    }

    #[test]
    fn test_unstaged_rename_in_subdirectory() {
        let temp = TempDir::new().unwrap();
        let path = temp.path();

        git_cmd(path, &["init"]);
        git_cmd(path, &["config", "user.email", "test@test.com"]);
        git_cmd(path, &["config", "user.name", "Test"]);

        // Create file in subdirectory
        fs::create_dir(path.join("subdir")).unwrap();
        fs::write(path.join("subdir/file.txt"), "content\n").unwrap();
        git_cmd(path, &["add", "."]);
        git_cmd(path, &["commit", "-m", "initial"]);
        git_cmd(path, &["branch", "-M", "main"]);

        let merge_base = get_merge_base(path, "main").unwrap();

        // Rename within subdirectory using filesystem mv
        fs::rename(
            path.join("subdir/file.txt"),
            path.join("subdir/renamed.txt"),
        )
        .unwrap();

        let changed = get_all_changed_files(path, &merge_base).unwrap();

        // Should detect as a rename
        assert_eq!(changed.len(), 1, "Should be one renamed file");
        let renamed = &changed[0];
        assert_eq!(renamed.path, "subdir/renamed.txt");
        assert_eq!(renamed.old_path, Some("subdir/file.txt".to_string()));
    }

    // ---- is_index_locked tests ----

    #[test]
    fn test_is_index_locked_no_lock() {
        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();

        assert!(!is_index_locked(temp.path()));
    }

    #[test]
    fn test_is_index_locked_with_lock() {
        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("index.lock"), "").unwrap();

        assert!(is_index_locked(temp.path()));
    }

    #[test]
    fn test_is_index_locked_no_git_dir() {
        let temp = TempDir::new().unwrap();
        // No .git directory at all
        assert!(!is_index_locked(temp.path()));
    }

    // ---- GitVcs tests ----

    #[test]
    fn test_git_vcs_new_detects_base_branch() {
        let temp = create_test_repo();
        let vcs = GitVcs::new(temp.path().to_path_buf()).unwrap();
        assert_eq!(vcs.base_branch(), "main");
        assert_eq!(vcs.repo_path(), temp.path());
    }

    #[test]
    fn test_git_vcs_comparison_context() {
        let temp = create_test_repo();
        git_cmd(temp.path(), &["checkout", "-b", "feature"]);

        let vcs = GitVcs::new(temp.path().to_path_buf()).unwrap();
        let ctx = vcs.comparison_context().unwrap();
        assert_eq!(ctx.from_label, "main");
        assert_eq!(ctx.to_label, "feature");
    }

    #[test]
    fn test_git_vcs_comparison_context_detached_head() {
        let temp = create_test_repo();
        let sha = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(temp.path())
            .output()
            .unwrap();
        let sha = String::from_utf8_lossy(&sha.stdout).trim().to_string();
        git_cmd(temp.path(), &["checkout", "--detach", &sha]);

        let vcs = GitVcs::new(temp.path().to_path_buf()).unwrap();
        let ctx = vcs.comparison_context().unwrap();
        assert_eq!(ctx.to_label, "HEAD");
    }

    #[test]
    fn test_git_vcs_binary_files() {
        let temp = create_test_repo();
        fs::write(temp.path().join("binary.bin"), &[0u8, 1, 2, 255]).unwrap();
        git_cmd(temp.path(), &["add", "binary.bin"]);

        let vcs = GitVcs::new(temp.path().to_path_buf()).unwrap();
        let binaries = vcs.binary_files();
        assert!(binaries.contains("binary.bin"));
    }

    #[test]
    fn test_git_vcs_base_file_bytes() {
        let temp = create_test_repo();
        let vcs = GitVcs::new(temp.path().to_path_buf()).unwrap();

        let bytes = vcs.base_file_bytes("file.txt").unwrap();
        assert!(bytes.is_some());
        assert_eq!(bytes.unwrap(), b"initial\n");
    }

    #[test]
    fn test_git_vcs_working_file_bytes() {
        let temp = create_test_repo();
        fs::write(temp.path().join("file.txt"), "modified\n").unwrap();

        let vcs = GitVcs::new(temp.path().to_path_buf()).unwrap();
        let bytes = vcs.working_file_bytes("file.txt").unwrap();
        assert!(bytes.is_some());
        assert_eq!(bytes.unwrap(), b"modified\n");
    }

    #[test]
    fn test_git_vcs_through_dyn_trait() {
        let temp = create_test_repo();
        git_cmd(temp.path(), &["checkout", "-b", "feature"]);
        fs::write(temp.path().join("file.txt"), "changed\n").unwrap();

        let vcs: Box<dyn Vcs> = Box::new(GitVcs::new(temp.path().to_path_buf()).unwrap());

        assert_eq!(vcs.repo_path(), temp.path());

        let ctx = vcs.comparison_context().unwrap();
        assert_eq!(ctx.from_label, "main");
        assert_eq!(ctx.to_label, "feature");

        let base_id = vcs.base_identifier().unwrap();
        assert!(!base_id.is_empty());

        let base_bytes = vcs.base_file_bytes("file.txt").unwrap();
        assert_eq!(base_bytes.unwrap(), b"initial\n");

        let working_bytes = vcs.working_file_bytes("file.txt").unwrap();
        assert_eq!(working_bytes.unwrap(), b"changed\n");

        assert!(vcs.binary_files().is_empty());
    }

    // === rename support tests ===

    #[test]
    fn test_find_rename_source_detects_committed_rename() {
        let temp = create_test_repo_with_content("line1\nline2\nline3\nline4\n");
        git_cmd(temp.path(), &["checkout", "-b", "feature"]);
        git_cmd(temp.path(), &["mv", "file.txt", "renamed.txt"]);
        git_cmd(temp.path(), &["commit", "-m", "rename"]);

        let merge_base = get_merge_base_preferring_origin(temp.path(), "main").unwrap();
        let old = find_rename_source(temp.path(), "renamed.txt", &merge_base);
        assert_eq!(old.as_deref(), Some("file.txt"));
    }

    #[test]
    fn test_find_rename_source_returns_none_for_non_rename() {
        let temp = create_test_repo();
        git_cmd(temp.path(), &["checkout", "-b", "feature"]);
        fs::write(temp.path().join("file.txt"), "changed\n").unwrap();
        git_cmd(temp.path(), &["add", "file.txt"]);
        git_cmd(temp.path(), &["commit", "-m", "modify"]);

        let merge_base = get_merge_base_preferring_origin(temp.path(), "main").unwrap();
        let old = find_rename_source(temp.path(), "file.txt", &merge_base);
        assert!(old.is_none());
    }

    #[test]
    fn test_find_rename_source_empty_merge_base() {
        let old = find_rename_source(Path::new("/tmp"), "file.txt", "");
        assert!(old.is_none());
    }

    #[test]
    fn test_single_file_diff_returns_diff_for_modified_file() {
        let temp = create_test_repo();
        git_cmd(temp.path(), &["checkout", "-b", "feature"]);
        fs::write(temp.path().join("file.txt"), "modified\n").unwrap();
        git_cmd(temp.path(), &["add", "file.txt"]);
        git_cmd(temp.path(), &["commit", "-m", "modify"]);

        let vcs = GitVcs::new(temp.path().to_path_buf()).unwrap();
        let diff = vcs.single_file_diff("file.txt");
        assert!(diff.is_some(), "should produce a diff for modified file");

        let diff = diff.unwrap();
        let header = &diff.lines[0];
        assert_eq!(header.source, LineSource::FileHeader);
        assert!(!header.content.contains("(deleted)"), "should not be a deletion header");
        assert!(!header.content.contains("→"), "should not be a rename header");
    }

    #[test]
    fn test_single_file_diff_handles_rename() {
        // Use multi-line content so git's rename detection (>50% similarity) works
        let temp = create_test_repo_with_content("line1\nline2\nline3\nline4\n");
        git_cmd(temp.path(), &["checkout", "-b", "feature"]);
        git_cmd(temp.path(), &["mv", "file.txt", "renamed.txt"]);
        fs::write(temp.path().join("renamed.txt"), "line1\nline2\nline3\nmodified\n").unwrap();
        git_cmd(temp.path(), &["add", "renamed.txt"]);
        git_cmd(temp.path(), &["commit", "-m", "rename and modify"]);

        let vcs = GitVcs::new(temp.path().to_path_buf()).unwrap();
        let diff = vcs.single_file_diff("renamed.txt");
        assert!(diff.is_some(), "should produce a diff for renamed file");

        let diff = diff.unwrap();
        let header = &diff.lines[0];
        assert!(
            header.content.contains("file.txt"),
            "rename header should reference old filename, got: {}",
            header.content
        );
    }

    // === classify_event tests ===

    fn classify(repo_path: &Path, relative: &str) -> VcsEventType {
        use crate::vcs::Vcs;
        let vcs = GitVcs {
            repo_path: repo_path.to_path_buf(),
            base_branch: "main".to_string(),
            git_version: GitVersion { major: 2, minor: 40, patch: 0 },
        };
        vcs.classify_event(&repo_path.join(relative))
    }

    #[test]
    fn test_classify_source_file() {
        let repo = Path::new("/repo");
        assert_eq!(classify(repo, "src/main.rs"), VcsEventType::Source);
    }

    #[test]
    fn test_classify_source_file_at_root() {
        let repo = Path::new("/repo");
        assert_eq!(classify(repo, "Cargo.toml"), VcsEventType::Source);
    }

    #[test]
    fn test_classify_gitignore_is_source() {
        let repo = Path::new("/repo");
        assert_eq!(classify(repo, ".gitignore"), VcsEventType::Source);
    }

    #[test]
    fn test_classify_git_index() {
        let repo = Path::new("/repo");
        assert_eq!(classify(repo, ".git/index"), VcsEventType::Internal);
    }

    #[test]
    fn test_classify_git_config() {
        let repo = Path::new("/repo");
        assert_eq!(classify(repo, ".git/config"), VcsEventType::Internal);
    }

    #[test]
    fn test_classify_git_head() {
        let repo = Path::new("/repo");
        assert_eq!(classify(repo, ".git/HEAD"), VcsEventType::RevisionChange);
    }

    #[test]
    fn test_classify_git_refs() {
        let repo = Path::new("/repo");
        assert_eq!(classify(repo, ".git/refs/heads/main"), VcsEventType::RevisionChange);
    }

    #[test]
    fn test_classify_git_index_lock() {
        let repo = Path::new("/repo");
        assert_eq!(classify(repo, ".git/index.lock"), VcsEventType::Lock);
    }

    #[test]
    fn test_classify_git_head_lock() {
        let repo = Path::new("/repo");
        assert_eq!(classify(repo, ".git/HEAD.lock"), VcsEventType::Lock);
    }

    #[test]
    fn test_classify_git_refs_lock() {
        let repo = Path::new("/repo");
        assert_eq!(classify(repo, ".git/refs/heads/main.lock"), VcsEventType::Lock);
    }

    #[test]
    fn test_classify_fetch_head_is_internal() {
        // FETCH_HEAD is not a revision change — it's written on every fetch
        let repo = Path::new("/repo");
        assert_eq!(classify(repo, ".git/FETCH_HEAD"), VcsEventType::Internal);
    }

    #[test]
    fn test_classify_orig_head_is_internal() {
        let repo = Path::new("/repo");
        assert_eq!(classify(repo, ".git/ORIG_HEAD"), VcsEventType::Internal);
    }

    #[test]
    fn test_classify_merge_head_is_internal() {
        let repo = Path::new("/repo");
        assert_eq!(classify(repo, ".git/MERGE_HEAD"), VcsEventType::Internal);
    }

    #[test]
    fn test_classify_nested_worktree_lock() {
        let repo = Path::new("/repo");
        assert_eq!(classify(repo, ".git/worktrees/foo/index.lock"), VcsEventType::Lock);
    }

    #[test]
    fn test_classify_path_outside_repo() {
        let repo = Path::new("/repo");
        let vcs = GitVcs {
            repo_path: repo.to_path_buf(),
            base_branch: "main".to_string(),
            git_version: GitVersion { major: 2, minor: 40, patch: 0 },
        };
        // Path outside repo — strip_prefix fails, treated as source
        assert_eq!(vcs.classify_event(Path::new("/other/file.rs")), VcsEventType::Source);
    }

    // === watch_paths tests ===

    #[test]
    fn test_watch_paths_includes_index_and_head() {
        use crate::vcs::Vcs;
        let repo = Path::new("/repo");
        let vcs = GitVcs {
            repo_path: repo.to_path_buf(),
            base_branch: "main".to_string(),
            git_version: GitVersion { major: 2, minor: 40, patch: 0 },
        };
        let paths = vcs.watch_paths();
        assert!(paths.files.contains(&repo.join(".git/index")));
        assert!(paths.files.contains(&repo.join(".git/HEAD")));
    }

    #[test]
    fn test_watch_paths_includes_refs_dir() {
        use crate::vcs::Vcs;
        let repo = Path::new("/repo");
        let vcs = GitVcs {
            repo_path: repo.to_path_buf(),
            base_branch: "main".to_string(),
            git_version: GitVersion { major: 2, minor: 40, patch: 0 },
        };
        let paths = vcs.watch_paths();
        assert!(paths.recursive_dirs.contains(&repo.join(".git/refs")));
    }
}
