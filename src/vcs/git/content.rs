//! File content and binary-file queries for the git backend.

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::Result;
use vcs_runner::{is_transient_error as vcs_is_transient, run_git, run_git_with_retry};

use super::parse::{parse_numstat_binaries, read_cat_file_batch};

/// Get file content at a specific ref (commit, branch, or index).
/// Use `:path` for staged content (index).
pub(super) fn get_file_at_ref(
    repo_path: &Path,
    file_path: &str,
    git_ref: &str,
) -> Result<Option<String>> {
    let ref_path =
        if git_ref.is_empty() { format!(":{file_path}") } else { format!("{git_ref}:{file_path}") };

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
        .map(
            |path| {
                if git_ref.is_empty() { format!(":{path}") } else { format!("{git_ref}:{path}") }
            },
        )
        .collect();

    let writer_handle = std::thread::spawn(move || {
        let mut writer = BufWriter::new(child_stdin);
        for spec in &specs {
            let _ = writeln!(writer, "{spec}");
        }
        let _ = writer.flush();
    });

    let results = read_cat_file_batch(&mut BufReader::new(child_stdout), file_paths);

    let _ = writer_handle.join();
    let _ = child.wait();

    results
}

/// Get working tree file content.
pub(super) fn get_working_tree_file(repo_path: &Path, file_path: &str) -> Result<Option<String>> {
    crate::vcs::shared::read_working_file(repo_path, file_path)
}

/// Get file content as raw bytes at a specific ref (for binary files like images).
/// Use `:path` for staged content (index).
pub fn get_file_bytes_at_ref(
    repo_path: &Path,
    file_path: &str,
    git_ref: &str,
) -> Result<Option<Vec<u8>>> {
    let ref_path =
        if git_ref.is_empty() { format!(":{file_path}") } else { format!("{git_ref}:{file_path}") };

    match run_git_with_retry(repo_path, &["show", &ref_path], vcs_is_transient) {
        Ok(output) => Ok(Some(output.stdout)),
        Err(e) if e.is_non_zero_exit() => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Get working tree file content as raw bytes (for binary files like images).
pub fn get_working_tree_bytes(repo_path: &Path, file_path: &str) -> Result<Option<Vec<u8>>> {
    crate::vcs::shared::read_working_file_bytes(repo_path, file_path)
}

/// Check if a file is binary (single file check - prefer `get_binary_files` for batch operations).
pub fn is_binary_file(repo_path: &Path, file_path: &str) -> bool {
    run_git(repo_path, &["diff", "--numstat", "--", file_path])
        .map(|o| o.stdout_lossy().starts_with("-\t-\t"))
        .unwrap_or(false)
}

/// Git's well-known SHA-1 for the empty tree (`git hash-object -t tree /dev/null`).
const EMPTY_TREE_HASH: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

fn or_empty_tree(git_ref: &str) -> &str {
    if git_ref.is_empty() { EMPTY_TREE_HASH } else { git_ref }
}

/// Check if a file is binary between two committed refs.
pub fn is_binary_file_between(
    repo_path: &Path,
    file_path: &str,
    from_ref: &str,
    to_ref: &str,
) -> bool {
    let base_ref = or_empty_tree(from_ref);
    run_git(repo_path, &["diff", "--numstat", base_ref, to_ref, "--", file_path])
        .map(|o| o.stdout_lossy().starts_with("-\t-\t"))
        .unwrap_or(false)
}

/// Get all binary files in the diff between `merge_base` and working tree.
pub fn get_binary_files(repo_path: &Path, merge_base: &str) -> HashSet<String> {
    let base_ref = or_empty_tree(merge_base);

    run_git(repo_path, &["diff", "--numstat", base_ref])
        .map(|output| parse_numstat_binaries(&output.stdout_lossy()))
        .unwrap_or_default()
}

/// Get all binary files in the diff between `merge_base` and `to_ref`.
pub fn get_binary_files_between(
    repo_path: &Path,
    merge_base: &str,
    to_ref: &str,
) -> HashSet<String> {
    let base_ref = or_empty_tree(merge_base);
    run_git(repo_path, &["diff", "--numstat", base_ref, to_ref])
        .map(|output| parse_numstat_binaries(&output.stdout_lossy()))
        .unwrap_or_default()
}
