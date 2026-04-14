use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

use vcs_runner::{run_cmd, run_git, run_git_with_retry, run_git_with_timeout, is_transient_error as vcs_is_transient};
use crate::vcs::UpstreamDivergence;

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
    let output = run_cmd("git", &["--version"])?;
    parse_git_version(&output.stdout_lossy())
}

/// Check if an external process holds the git index lock.
///
/// Returns true if `.git/index.lock` exists, indicating another git process
/// (like `git rebase`, `git commit`, etc.) is currently running.
/// When locked, branchdiff should defer refresh to avoid lock collisions.
pub fn is_index_locked(repo_path: &Path) -> bool {
    repo_path.join(".git/index.lock").exists()
}

/// Parse git version from "git version X.Y.Z" string
pub(super) fn parse_git_version(s: &str) -> Result<GitVersion> {
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
pub fn get_repo_root(path: &Path) -> Result<PathBuf> {
    let output = run_git(path, &["rev-parse", "--show-toplevel"])?;
    let root = output.stdout_lossy().trim().to_string();
    Ok(PathBuf::from(root))
}

/// Check whether a git ref exists in the repository.
fn ref_exists(repo_path: &Path, git_ref: &str) -> bool {
    run_git(repo_path, &["rev-parse", "--verify", git_ref]).is_ok()
}

/// Detect whether the base branch is 'main' or 'master'.
///
/// Prefers origin remote-tracking refs so that a local branch tracking a
/// non-origin remote (e.g. heroku) doesn't win over origin.
pub fn detect_base_branch(repo_path: &Path) -> Result<String> {
    // Prefer origin remote-tracking refs
    for branch in &["main", "master"] {
        if ref_exists(repo_path, &format!("origin/{}", branch)) {
            return Ok(branch.to_string());
        }
    }

    // Fall back to local branches (repos without an origin remote)
    for branch in &["main", "master"] {
        if ref_exists(repo_path, branch) {
            return Ok(branch.to_string());
        }
    }

    Err(anyhow!("Could not find 'main' or 'master' branch"))
}

/// Get the merge-base between HEAD and the base branch, preferring origin
pub fn get_merge_base_preferring_origin(repo_path: &Path, base_branch: &str) -> Result<String> {
    let remote_ref = format!("origin/{}", base_branch);
    get_merge_base(repo_path, &remote_ref)
        .or_else(|_| get_merge_base(repo_path, base_branch))
}

/// Get the merge-base between the base branch and HEAD
pub(super) fn get_merge_base(repo_path: &Path, base_branch: &str) -> Result<String> {
    let output = run_git(repo_path, &["merge-base", base_branch, "HEAD"])?;
    Ok(output.stdout_lossy().trim().to_string())
}

/// Compute upstream divergence: how many commits and which files have changed
/// on origin/{base_branch} since the merge-base.
pub fn compute_upstream_divergence(
    repo_path: &Path,
    merge_base: &str,
    base_branch: &str,
) -> Option<UpstreamDivergence> {
    if merge_base.is_empty() {
        return None;
    }

    let remote_ref = format!("origin/{}", base_branch);
    if !ref_exists(repo_path, &remote_ref) {
        return None;
    }

    let behind_count = rev_list_count(repo_path, merge_base, &remote_ref).unwrap_or(0);
    if behind_count == 0 {
        return None;
    }

    let upstream_files = upstream_changed_files(repo_path, merge_base, &remote_ref)
        .unwrap_or_default();

    Some(UpstreamDivergence {
        behind_count,
        upstream_files,
    })
}

/// Count commits reachable from `to` but not from `from`.
fn rev_list_count(repo_path: &Path, from: &str, to: &str) -> Result<usize> {
    let range = format!("{}..{}", from, to);
    let output = run_git(repo_path, &["rev-list", "--count", &range])?;
    output.stdout_lossy()
        .trim()
        .parse::<usize>()
        .context("Failed to parse rev-list count")
}

/// Get files changed between two refs.
fn upstream_changed_files(
    repo_path: &Path,
    from: &str,
    to: &str,
) -> Result<HashSet<String>> {
    let range = format!("{}..{}", from, to);
    let output = run_git(repo_path, &["diff", "--name-only", &range])?;
    Ok(output.stdout_lossy()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect())
}

/// Get file content at a specific ref (commit, branch, or index)
/// Use `:path` for staged content (index)
pub(super) fn get_file_at_ref(repo_path: &Path, file_path: &str, git_ref: &str) -> Result<Option<String>> {
    let ref_path = if git_ref.is_empty() {
        // Empty ref means index (staged)
        format!(":{}", file_path)
    } else {
        format!("{}:{}", git_ref, file_path)
    };

    match run_git_with_retry(repo_path, &["show", &ref_path], vcs_is_transient) {
        Ok(output) => Ok(Some(output.stdout_lossy().into_owned())),
        Err(e) if e.is_non_zero_exit() => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Fetch content of multiple files at a given ref using `git cat-file --batch`.
///
/// Spawns a single `git cat-file --batch` process rather than N `git show` calls.
/// For index (staged) content, pass an empty string as `git_ref`.
/// Returns a map from file path to content for files that exist at the ref.
///
/// Responses arrive in the same order as specs, one per spec:
/// - Blob: `<sha> blob <size>\n<content>\n`
/// - Non-blob (tree/commit/tag): `<sha> <type> <size>\n<content>\n` — skipped
/// - Missing/ambiguous/submodule: `<spec> missing\n` (2-field) — skipped
pub(super) fn batch_file_contents(
    repo_path: &Path,
    file_paths: &[&str],
    git_ref: &str,
) -> HashMap<String, String> {
    if file_paths.is_empty() {
        return HashMap::new();
    }

    let mut child = match Command::new("git")
        .args(["cat-file", "--batch"])
        .current_dir(repo_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return HashMap::new(),
    };

    let child_stdin = child.stdin.take().expect("stdin was piped");
    let child_stdout = child.stdout.take().expect("stdout was piped");

    let specs: Vec<String> = file_paths
        .iter()
        .map(|path| {
            if git_ref.is_empty() {
                format!(":{path}")
            } else {
                format!("{git_ref}:{path}")
            }
        })
        .collect();

    // Write all specs on a background thread to avoid deadlock when stdout
    // buffer fills before we've finished writing to stdin.
    let writer_handle = std::thread::spawn(move || {
        let mut writer = BufWriter::new(child_stdin);
        for spec in &specs {
            let _ = writeln!(writer, "{spec}");
        }
        let _ = writer.flush();
    });

    let mut reader = BufReader::new(child_stdout);
    let mut results = HashMap::with_capacity(file_paths.len());
    let mut header_line = String::new();

    // Responses arrive in the same order as specs — one response per spec.
    for &path in file_paths {
        header_line.clear();
        if reader.read_line(&mut header_line).unwrap_or(0) == 0 {
            break;
        }
        let header = header_line.trim_end();

        // Success format: "<sha> <type> <size>" (3 fields).
        // Error formats have 2 fields: "<spec> missing", "<spec> ambiguous", etc.
        // Split into exactly 3 fields to distinguish success from error.
        let parts: Vec<&str> = header.splitn(4, ' ').collect();
        if parts.len() < 3 {
            // 2-field response: missing, ambiguous, submodule, excluded — skip
            continue;
        }

        let obj_type = parts[1];
        let size: usize = match parts[2].parse() {
            Ok(n) => n,
            Err(_) => continue,
        };

        // Read the content bytes + trailing LF regardless of object type,
        // to keep the stream in sync for subsequent responses.
        let mut content_buf = vec![0u8; size];
        if reader.read_exact(&mut content_buf).is_err() {
            break;
        }
        let mut trailing = [0u8; 1];
        let _ = reader.read_exact(&mut trailing);

        // Only collect blob content; skip trees, commits, tags.
        if obj_type == "blob" {
            results.insert(
                path.to_string(),
                String::from_utf8_lossy(&content_buf).into_owned(),
            );
        }
    }

    let _ = writer_handle.join();
    let _ = child.wait();

    results
}

/// Get working tree file content
pub(super) fn get_working_tree_file(repo_path: &Path, file_path: &str) -> Result<Option<String>> {
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

    match run_git_with_retry(repo_path, &["show", &ref_path], vcs_is_transient) {
        Ok(output) => Ok(Some(output.stdout)),
        Err(e) if e.is_non_zero_exit() => Ok(None),
        Err(e) => Err(e.into()),
    }
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

/// Check if a file is binary (single file check - prefer get_binary_files for batch operations)
pub fn is_binary_file(repo_path: &Path, file_path: &str) -> bool {
    run_git(repo_path, &["diff", "--numstat", "--", file_path])
        .map(|o| o.stdout_lossy().starts_with("-\t-\t"))
        .unwrap_or(false)
}

/// Get all binary files in the diff between merge_base and working tree.
/// Returns a HashSet of file paths that are binary.
/// This is more efficient than calling is_binary_file() for each file.
pub fn get_binary_files(repo_path: &Path, merge_base: &str) -> HashSet<String> {
    let mut binaries = HashSet::new();

    // Compare merge_base to working tree (covers committed + staged + unstaged changes)
    // If merge_base is empty (new repo), check against empty tree
    let base_ref = if merge_base.is_empty() {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904" // git's empty tree SHA
    } else {
        merge_base
    };

    // git diff --numstat <ref> (with no second ref) compares ref to working tree
    if let Ok(output) = run_git(repo_path, &["diff", "--numstat", base_ref]) {
        let s = output.stdout_lossy();
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
    let current = get_current_branch(repo_path).ok().flatten();
    let on_base_branch = current.as_deref() == Some(base_branch);

    let refspec = format!("{}:{}", base_branch, base_branch);
    let fetch_arg = if on_base_branch { base_branch } else { &refspec };

    run_git_with_timeout(
        repo_path,
        &["-c", "gc.auto=0", "fetch", "--no-tags", "origin", fetch_arg],
        std::time::Duration::from_secs(30),
    )?;
    Ok(())
}

/// Check for merge conflicts using git merge-tree.
/// Requires Git 2.38+ for --write-tree flag; returns Ok(false) on older versions.
pub fn has_merge_conflicts(repo_path: &Path, base_branch: &str, git_version: &GitVersion) -> Result<bool> {
    // merge-tree --write-tree requires Git 2.38+
    if !git_version.at_least(MERGE_TREE_MIN_VERSION.0, MERGE_TREE_MIN_VERSION.1) {
        return Ok(false);
    }

    let remote_ref = format!("origin/{}", base_branch);

    if !ref_exists(repo_path, &remote_ref) {
        return Ok(false);
    }

    // git merge-tree exits non-zero iff conflicts are detected. Distinguish
    // "conflict reported" (non-zero exit) from "command couldn't run" (spawn).
    match run_git(repo_path, &["merge-tree", "--write-tree", &remote_ref, "HEAD"]) {
        Ok(_) => Ok(false),
        Err(e) if e.is_non_zero_exit() => Ok(true),
        Err(e) => Err(e.into()),
    }
}

/// Get the current branch name
pub fn get_current_branch(repo_path: &Path) -> Result<Option<String>> {
    let output = match run_git(repo_path, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(o) => o,
        Err(_) => return Ok(None),
    };

    let branch = output.stdout_lossy().trim().to_string();
    if branch == "HEAD" {
        Ok(None)
    } else {
        Ok(Some(branch))
    }
}
