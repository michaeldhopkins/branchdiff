use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};

use vcs_runner::{run_cmd_in_with_env, run_git, run_git_with_retry, is_transient_error};

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
    let git_dir_output = match run_git(repo_path, &["rev-parse", "--git-dir"]) {
        Ok(o) => o,
        Err(_) => return Ok(Vec::new()),
    };

    let git_dir = repo_path.join(git_dir_output.stdout_lossy().trim());
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

    let env = [("GIT_INDEX_FILE", temp_index_path.as_str())];

    // Batch git add -N for all untracked files
    // Using --intent-to-add with the temp index
    let mut add_args: Vec<&str> = vec!["add", "-N", "--"];
    add_args.extend(untracked_files.iter().map(String::as_str));
    if run_cmd_in_with_env(repo_path, "git", &add_args, &env).is_err() {
        // If add fails, just return empty (don't break the whole refresh)
        return Ok(Vec::new());
    }

    // Run git diff with rename detection using the temp index
    let diff_output = match run_cmd_in_with_env(
        repo_path,
        "git",
        &["diff", "--name-status", "-M", "HEAD"],
        &env,
    ) {
        Ok(o) => o,
        Err(_) => return Ok(Vec::new()),
    };

    // Parse renames from the diff output
    let deleted_set: HashSet<&str> = deleted_files.iter().map(String::as_str).collect();
    let untracked_set: HashSet<&str> = untracked_files.iter().map(String::as_str).collect();

    let output_str = diff_output.stdout_lossy();
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
    let status_output = run_git_with_retry(
        repo_path,
        &["status", "--porcelain=v1", "-uall"],
        is_transient_error,
    )?;

    {
        let status_str = status_output.stdout_lossy();
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
pub(super) struct FileTransition {
    /// Source path (None for added files)
    pub(super) from: Option<String>,
    /// Destination path (None for deleted files)
    pub(super) to: Option<String>,
}

impl FileTransition {
    /// Get the current/relevant path for this transition.
    /// Prefers the destination path, falls back to source for deletions.
    #[cfg(test)]
    pub(super) fn current_path(&self) -> Option<&str> {
        self.to.as_deref().or(self.from.as_deref())
    }
}

/// Parse a single line of `git diff --name-status` output into a FileTransition.
/// Returns None for unrecognized formats.
pub(super) fn parse_diff_line(line: &str) -> Option<FileTransition> {
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
pub(super) fn get_diff_transitions(repo_path: &Path, from: &str, to: &str) -> Result<Vec<FileTransition>> {
    let output = run_git_with_retry(
        repo_path,
        &["diff", "--name-status", "-M", from, to],
        is_transient_error,
    )?;

    let output_str = output.stdout_lossy();
    let transitions: Vec<FileTransition> = output_str
        .lines()
        .filter_map(parse_diff_line)
        .collect();

    Ok(transitions)
}

/// Check if file_path was renamed from another path in committed changes.
pub(super) fn find_rename_source(repo_path: &Path, file_path: &str, merge_base: &str) -> Option<String> {
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
