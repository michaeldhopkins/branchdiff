use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use rayon::prelude::*;

use super::{vcs_thread_pool, PARALLEL_THRESHOLD};

const MAX_RETRIES: u32 = 2;
const BASE_RETRY_DELAY_MS: u64 = 100;

/// Detect transient jj errors worth retrying.
/// "stale" matches "The working copy is stale" (exit code 1), which resolves
/// after jj finishes its working copy update.
fn is_transient_jj_error(stderr: &str) -> bool {
    stderr.contains("stale")
}

/// Prepend `--ignore-working-copy` to skip jj's auto-snapshot.
/// Only the first command per refresh cycle needs to snapshot; subsequent
/// commands reuse the snapshot and avoid writing to op_store/working_copy.
fn no_snapshot<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut full = Vec::with_capacity(args.len() + 1);
    full.push("--ignore-working-copy");
    full.extend_from_slice(args);
    full
}

/// Run a jj command with exponential backoff on transient errors.
fn run_jj_with_retry(repo_path: &Path, args: &[&str]) -> Result<Output> {
    for attempt in 0..=MAX_RETRIES {
        let output = Command::new("jj")
            .args(args)
            .current_dir(repo_path)
            .output()
            .context("failed to run jj")?;

        if output.status.success() {
            return Ok(output);
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        if !is_transient_jj_error(&stderr) || attempt == MAX_RETRIES {
            return Ok(output);
        }

        let delay = Duration::from_millis(BASE_RETRY_DELAY_MS * (1 << attempt));
        thread::sleep(delay);
    }

    // Unreachable due to loop structure, but satisfies the compiler
    Command::new("jj")
        .args(args)
        .current_dir(repo_path)
        .output()
        .context("failed to run jj")
}

use crate::diff::{compute_four_way_diff, DiffInput, DiffLine, FileDiff, LineSource};
use crate::image_diff::is_image_file;
use crate::limits::DiffMetrics;
use crate::vcs::{ComparisonContext, RefreshResult, VcsEventType, VcsWatchPaths};

/// Jujutsu (jj) backend for branchdiff.
pub struct JjVcs {
    repo_path: PathBuf,
    /// Revset for the base of comparison (default: "@-")
    from_rev: String,
}

impl JjVcs {
    pub fn new(repo_path: PathBuf) -> Result<Self> {
        Ok(Self {
            repo_path,
            from_rev: "@-".to_string(),
        })
    }

    fn run_jj(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("jj")
            .args(args)
            .current_dir(&self.repo_path)
            .output()
            .context("failed to run jj")?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("jj {} failed: {}", args.join(" "), stderr.trim())
        }
    }

    fn run_jj_bytes(&self, args: &[&str]) -> Result<Option<Vec<u8>>> {
        let output = Command::new("jj")
            .args(args)
            .current_dir(&self.repo_path)
            .output()
            .context("failed to run jj")?;

        if output.status.success() {
            Ok(Some(output.stdout))
        } else {
            Ok(None)
        }
    }

    /// Get changed files between from_rev and @.
    /// This is the first command per refresh and triggers the working copy
    /// auto-snapshot — all subsequent commands use `--ignore-working-copy`.
    fn get_changed_files(&self) -> Result<Vec<ChangedFile>> {
        let output = run_jj_with_retry(
            &self.repo_path,
            &["diff", "--from", &self.from_rev, "--to", "@", "--summary"],
        )?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("jj diff --summary failed: {}", stderr.trim());
        }
        Ok(parse_jj_summary(&String::from_utf8_lossy(&output.stdout)))
    }

    /// Detect binary files by checking --stat output for "(binary)" markers.
    /// Uses `--ignore-working-copy` since the snapshot is already fresh from
    /// `get_changed_files`.
    fn get_binary_files_set(&self) -> HashSet<String> {
        let args = no_snapshot(&[
            "diff", "--from", &self.from_rev, "--to", "@", "--stat",
        ]);
        let Ok(output) = run_jj_with_retry(&self.repo_path, &args) else {
            return HashSet::new();
        };
        if !output.status.success() {
            return HashSet::new();
        }
        parse_binary_from_stat(&String::from_utf8_lossy(&output.stdout))
    }

    fn get_file_bytes_at_rev(&self, file_path: &str, rev: &str) -> Result<Option<Vec<u8>>> {
        self.run_jj_bytes(&["file", "show", "-r", rev, file_path])
    }

    /// Get the current change ID for a revision.
    fn get_change_id(&self, rev: &str) -> Result<String> {
        let output = self.run_jj(&["log", "-r", rev, "-T", "change_id.short(12)", "--no-graph", "--limit", "1"])?;
        Ok(output.trim().to_string())
    }

    #[cfg(test)]
    fn get_bookmarks(&self, rev: &str) -> Option<String> {
        let output = self.run_jj(&["log", "-r", rev, "-T", "bookmarks", "--no-graph", "--limit", "1"]).ok()?;
        let trimmed = output.trim().trim_end_matches('*');
        if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
    }

    #[cfg(test)]
    fn rev_label(&self, rev: &str) -> String {
        self.get_bookmarks(rev)
            .unwrap_or_else(|| self.get_change_id(rev).unwrap_or_else(|_| rev.to_string()))
    }

    /// Fetch bookmarks and change_id for a revision in a single command,
    /// using `--ignore-working-copy` to avoid redundant auto-snapshots.
    /// Returns (change_id, display_label).
    fn rev_metadata_no_snapshot(&self, rev: &str) -> (String, String) {
        let template = r#"bookmarks ++ "\0" ++ change_id.short(12)"#;
        let args = no_snapshot(&[
            "log", "-r", rev, "-T", template, "--no-graph", "--limit", "1",
        ]);
        match self.run_jj(&args) {
            Ok(raw) => parse_rev_metadata(&raw),
            Err(_) => (rev.to_string(), rev.to_string()),
        }
    }

    /// Check if repo is colocated (has .git directory alongside .jj).
    fn is_colocated(&self) -> bool {
        self.repo_path.join(".git").exists()
    }
}

/// Parse combined `bookmarks ++ "\0" ++ change_id` template output.
/// Returns (change_id, display_label) where label prefers bookmarks.
fn parse_rev_metadata(raw: &str) -> (String, String) {
    let raw = raw.trim();
    if let Some((bookmarks_raw, change_id)) = raw.split_once('\0') {
        let bookmarks = bookmarks_raw.trim().trim_end_matches('*');
        let change_id = change_id.trim().to_string();
        let label = if bookmarks.is_empty() {
            change_id.clone()
        } else {
            bookmarks.to_string()
        };
        (change_id, label)
    } else {
        (raw.to_string(), raw.to_string())
    }
}

/// Read file content at a revision without triggering auto-snapshot.
/// Free function for use in parallel contexts (rayon).
fn file_content_no_snapshot(repo_path: &Path, file_path: &str, rev: &str) -> Option<String> {
    let args = no_snapshot(&["file", "show", "-r", rev, file_path]);
    let output = Command::new("jj")
        .args(&args)
        .current_dir(repo_path)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        None
    }
}

enum FileProcessResult {
    Diff(FileDiff),
    Binary { path: String },
    Image { path: String },
}

fn process_jj_file(
    repo_path: &Path,
    from_rev: &str,
    changed: &ChangedFile,
    binary_files: &HashSet<String>,
) -> FileProcessResult {
    if binary_files.contains(&changed.path) {
        if is_image_file(&changed.path) {
            return FileProcessResult::Image { path: changed.path.clone() };
        }
        return FileProcessResult::Binary { path: changed.path.clone() };
    }

    let base_path = changed.old_path.as_deref().unwrap_or(&changed.path);
    let base = file_content_no_snapshot(repo_path, base_path, from_rev);
    let head = file_content_no_snapshot(repo_path, &changed.path, "@");

    FileProcessResult::Diff(compute_four_way_diff(DiffInput {
        path: &changed.path,
        base: base.as_deref(),
        head: head.as_deref(),
        index: head.as_deref(),
        working: head.as_deref(),
        old_path: changed.old_path.as_deref(),
    }))
}

/// Get the jj repo root from a path.
pub fn get_repo_root(path: &Path) -> Result<PathBuf> {
    let output = Command::new("jj")
        .args(["root"])
        .current_dir(path)
        .output()
        .context("failed to run jj root")?;

    if output.status.success() {
        let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(PathBuf::from(root))
    } else {
        anyhow::bail!("not a jj repository")
    }
}

impl crate::vcs::Vcs for JjVcs {
    fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    fn comparison_context(&self) -> Result<ComparisonContext> {
        let template = r#"bookmarks ++ "\0" ++ change_id.short(12)"#;
        // First call triggers auto-snapshot to capture current working copy
        let from_output = run_jj_with_retry(
            &self.repo_path,
            &["log", "-r", &self.from_rev, "-T", template, "--no-graph", "--limit", "1"],
        )?;
        let from_label = if from_output.status.success() {
            parse_rev_metadata(&String::from_utf8_lossy(&from_output.stdout)).1
        } else {
            self.from_rev.clone()
        };

        // Second call skips snapshot (already fresh)
        let to_args = no_snapshot(&[
            "log", "-r", "@", "-T", template, "--no-graph", "--limit", "1",
        ]);
        let to_output = run_jj_with_retry(&self.repo_path, &to_args)?;
        let to_label = if to_output.status.success() {
            parse_rev_metadata(&String::from_utf8_lossy(&to_output.stdout)).1
        } else {
            "@".to_string()
        };

        Ok(ComparisonContext { from_label, to_label })
    }

    fn refresh(&self, cancel_flag: &Arc<AtomicBool>) -> Result<RefreshResult> {
        // First command — triggers working copy auto-snapshot
        let changed_files = self.get_changed_files()?;

        if cancel_flag.load(Ordering::Relaxed) {
            anyhow::bail!("refresh cancelled");
        }

        // All subsequent commands use --ignore-working-copy
        let binary_files = self.get_binary_files_set();

        if cancel_flag.load(Ordering::Relaxed) {
            anyhow::bail!("refresh cancelled");
        }

        let results: Vec<FileProcessResult> = if changed_files.len() >= PARALLEL_THRESHOLD {
            vcs_thread_pool().install(|| {
                changed_files
                    .par_iter()
                    .map(|changed| {
                        process_jj_file(
                            &self.repo_path,
                            &self.from_rev,
                            changed,
                            &binary_files,
                        )
                    })
                    .collect()
            })
        } else {
            changed_files
                .iter()
                .map(|changed| {
                    process_jj_file(
                        &self.repo_path,
                        &self.from_rev,
                        changed,
                        &binary_files,
                    )
                })
                .collect()
        };

        if cancel_flag.load(Ordering::Relaxed) {
            anyhow::bail!("refresh cancelled");
        }

        let mut files = Vec::new();
        let mut all_lines = Vec::new();

        for result in results {
            match result {
                FileProcessResult::Diff(file_diff) => {
                    all_lines.extend(file_diff.lines.iter().cloned());
                    all_lines.push(DiffLine::new(LineSource::Base, String::new(), ' ', None));
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
                    all_lines.push(header.clone());
                    all_lines.push(marker.clone());
                    files.push(FileDiff { lines: vec![header, marker] });
                }
                FileProcessResult::Image { path } => {
                    let header = DiffLine::file_header(&path);
                    let marker = DiffLine::image_marker(&path);
                    all_lines.push(header.clone());
                    all_lines.push(marker.clone());
                    files.push(FileDiff { lines: vec![header, marker] });
                }
            }
        }

        let metrics = DiffMetrics {
            total_lines: all_lines.len(),
            file_count: files.len(),
        };
        let (base_identifier, base_label_str) =
            self.rev_metadata_no_snapshot(&self.from_rev);
        let (_, current_branch_str) = self.rev_metadata_no_snapshot("@");

        let file_paths: Vec<&str> = files
            .iter()
            .filter_map(|f| f.lines.first())
            .filter_map(|l| l.file_path.as_deref())
            .collect();
        let file_links = crate::file_links::compute_file_links(&file_paths);

        Ok(RefreshResult {
            files,
            lines: all_lines,
            base_identifier,
            base_label: Some(base_label_str),
            current_branch: Some(current_branch_str),
            metrics,
            file_links,
        })
    }

    fn single_file_diff(&self, file_path: &str) -> Option<FileDiff> {
        // First command triggers auto-snapshot
        let changed_files = self.get_changed_files().ok()?;
        let changed = changed_files.iter().find(|f| f.path == file_path);
        let old_path = changed.and_then(|f| f.old_path.as_deref());

        // Subsequent commands skip snapshot
        let base_path = old_path.unwrap_or(file_path);
        let base = file_content_no_snapshot(&self.repo_path, base_path, &self.from_rev);
        let head = file_content_no_snapshot(&self.repo_path, file_path, "@");

        if base.is_none() && head.is_none() {
            return None;
        }

        let binary_files = self.get_binary_files_set();
        if binary_files.contains(file_path) {
            return None;
        }

        Some(compute_four_way_diff(DiffInput {
            path: file_path,
            base: base.as_deref(),
            head: head.as_deref(),
            index: head.as_deref(),
            working: head.as_deref(),
            old_path,
        }))
    }

    fn base_identifier(&self) -> Result<String> {
        self.get_change_id(&self.from_rev)
    }

    fn base_file_bytes(&self, file_path: &str) -> Result<Option<Vec<u8>>> {
        self.get_file_bytes_at_rev(file_path, &self.from_rev)
    }

    fn working_file_bytes(&self, file_path: &str) -> Result<Option<Vec<u8>>> {
        self.get_file_bytes_at_rev(file_path, "@")
    }

    fn binary_files(&self) -> HashSet<String> {
        self.get_binary_files_set()
    }

    fn fetch(&self) -> Result<()> {
        if self.is_colocated() {
            self.run_jj(&["git", "fetch"])?;
        }
        Ok(())
    }

    fn has_conflicts(&self) -> Result<bool> {
        // jj conflict detection is a future enhancement
        Ok(false)
    }

    fn is_locked(&self) -> bool {
        // jj doesn't use lock files the same way git does
        false
    }

    fn watch_paths(&self) -> VcsWatchPaths {
        let jj_dir = self.repo_path.join(".jj");
        VcsWatchPaths {
            files: vec![jj_dir.join("working_copy/checkout")],
            recursive_dirs: vec![jj_dir.join("repo/op_store")],
        }
    }

    fn classify_event(&self, path: &Path) -> VcsEventType {
        let relative = path.strip_prefix(&self.repo_path).unwrap_or(path);
        let is_jj_path = relative
            .components()
            .next()
            .is_some_and(|c| c.as_os_str() == ".jj");

        if !is_jj_path {
            return VcsEventType::Source;
        }

        let path_str = relative.to_string_lossy();
        if path_str.contains("working_copy/") {
            VcsEventType::RevisionChange
        } else {
            // op_store/ writes happen on every jj command (even reads like jj diff)
            // and are mostly side-effects of our own refresh calls
            VcsEventType::Internal
        }
    }

    fn vcs_name(&self) -> &str {
        "jj"
    }
}

/// Changed file from jj diff --summary output.
#[derive(Debug, Clone)]
struct ChangedFile {
    path: String,
    old_path: Option<String>,
}

/// Parse `jj diff --summary` output into changed files.
///
/// Renames use the format `R {old_path => new_path}`.
fn parse_jj_summary(output: &str) -> Vec<ChangedFile> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let first = line.chars().next()?;
            if !matches!(first, 'M' | 'A' | 'D' | 'R' | 'C') {
                return None;
            }
            let rest = line[1..].trim();
            if rest.is_empty() {
                return None;
            }
            if first == 'R' {
                parse_rename(rest)
            } else {
                Some(ChangedFile { path: rest.to_string(), old_path: None })
            }
        })
        .collect()
}

/// Parse jj rename format: `{old_path => new_path}`
fn parse_rename(s: &str) -> Option<ChangedFile> {
    let s = s.strip_prefix('{')?.strip_suffix('}')?;
    let (old, new) = s.split_once(" => ")?;
    let old = old.trim();
    let new = new.trim();
    if new.is_empty() {
        return None;
    }
    Some(ChangedFile {
        path: new.to_string(),
        old_path: Some(old.to_string()),
    })
}

/// Parse `jj diff --stat` output to find binary files (marked with "(binary)").
///
/// Handles renamed files: `{old => new} | (binary)` extracts just the new name.
fn parse_binary_from_stat(output: &str) -> HashSet<String> {
    output
        .lines()
        .filter(|line| line.contains("(binary)"))
        .filter_map(|line| {
            let raw_path = line.split('|').next()?.trim();
            if raw_path.is_empty() {
                return None;
            }
            // Renames show as "{old => new}" — extract the new name
            let path = if let Some(inner) = raw_path.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                inner.split(" => ").last().unwrap_or(inner).trim()
            } else if raw_path.contains(" => ") {
                raw_path.split(" => ").last().unwrap_or(raw_path).trim()
            } else {
                raw_path
            };
            if path.is_empty() { None } else { Some(path.to_string()) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::Vcs;

    // === parse_jj_summary tests ===

    #[test]
    fn test_parse_summary_modified() {
        let files = parse_jj_summary("M file.txt\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "file.txt");
        assert!(files[0].old_path.is_none());
    }

    #[test]
    fn test_parse_summary_added() {
        let files = parse_jj_summary("A new_file.txt\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "new_file.txt");
        assert!(files[0].old_path.is_none());
    }

    #[test]
    fn test_parse_summary_deleted() {
        let files = parse_jj_summary("D old_file.txt\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "old_file.txt");
    }

    #[test]
    fn test_parse_summary_renamed() {
        let files = parse_jj_summary("R {old_name.txt => new_name.txt}\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "new_name.txt");
        assert_eq!(files[0].old_path.as_deref(), Some("old_name.txt"));
    }

    #[test]
    fn test_parse_summary_renamed_with_directory() {
        let files = parse_jj_summary("R {src/old.rs => src/new.rs}\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/new.rs");
        assert_eq!(files[0].old_path.as_deref(), Some("src/old.rs"));
    }

    #[test]
    fn test_parse_summary_multiple() {
        let output = "M file1.txt\nA file2.txt\nD file3.txt\n";
        let files = parse_jj_summary(output);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "file1.txt");
        assert_eq!(files[1].path, "file2.txt");
        assert_eq!(files[2].path, "file3.txt");
    }

    #[test]
    fn test_parse_summary_mixed_with_rename() {
        let output = "M file.txt\nR {old.rs => new.rs}\nA added.txt\n";
        let files = parse_jj_summary(output);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "file.txt");
        assert!(files[0].old_path.is_none());
        assert_eq!(files[1].path, "new.rs");
        assert_eq!(files[1].old_path.as_deref(), Some("old.rs"));
        assert_eq!(files[2].path, "added.txt");
        assert!(files[2].old_path.is_none());
    }

    #[test]
    fn test_parse_summary_empty() {
        let files = parse_jj_summary("");
        assert!(files.is_empty());
    }

    #[test]
    fn test_parse_summary_skips_blank_lines() {
        let output = "M file.txt\n\nA other.txt\n";
        let files = parse_jj_summary(output);
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn test_parse_summary_path_with_spaces() {
        let files = parse_jj_summary("M path with spaces.txt\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "path with spaces.txt");
    }

    #[test]
    fn test_parse_rename_malformed_no_braces() {
        let files = parse_jj_summary("R old.txt => new.txt\n");
        assert!(files.is_empty(), "rename without braces should be skipped");
    }

    #[test]
    fn test_parse_rename_malformed_no_arrow() {
        let files = parse_jj_summary("R {old.txt new.txt}\n");
        assert!(files.is_empty(), "rename without => should be skipped");
    }

    // === parse_binary_from_stat tests ===

    #[test]
    fn test_parse_binary_from_stat_detects_binary() {
        let output = "image.png | (binary)\nfile.txt  | 2 +-\n1 file changed\n";
        let binaries = parse_binary_from_stat(output);
        assert!(binaries.contains("image.png"));
        assert!(!binaries.contains("file.txt"));
    }

    #[test]
    fn test_parse_binary_from_stat_empty() {
        let binaries = parse_binary_from_stat("file.txt | 2 +-\n");
        assert!(binaries.is_empty());
    }

    #[test]
    fn test_parse_binary_from_stat_multiple() {
        let output = "a.png | (binary)\nb.jpg | (binary)\nc.txt | 1 +\n";
        let binaries = parse_binary_from_stat(output);
        assert_eq!(binaries.len(), 2);
        assert!(binaries.contains("a.png"));
        assert!(binaries.contains("b.jpg"));
    }

    #[test]
    fn test_parse_binary_from_stat_renamed_with_braces() {
        let output = "{original.bin => renamed.bin} | (binary)\n";
        let binaries = parse_binary_from_stat(output);
        assert!(binaries.contains("renamed.bin"), "should extract new name from rename");
        assert!(!binaries.contains("{original.bin => renamed.bin}"), "should not store raw rename format");
    }

    #[test]
    fn test_parse_binary_from_stat_renamed_without_braces() {
        let output = "original.bin => renamed.bin | (binary)\n";
        let binaries = parse_binary_from_stat(output);
        assert!(binaries.contains("renamed.bin"), "should extract new name from arrow format");
    }

    // === parse_rev_metadata tests ===

    #[test]
    fn test_parse_rev_metadata_with_bookmark() {
        let (change_id, label) = parse_rev_metadata("my-feature\0abcdef123456\n");
        assert_eq!(change_id, "abcdef123456");
        assert_eq!(label, "my-feature");
    }

    #[test]
    fn test_parse_rev_metadata_without_bookmark() {
        let (change_id, label) = parse_rev_metadata("\0abcdef123456\n");
        assert_eq!(change_id, "abcdef123456");
        assert_eq!(label, "abcdef123456");
    }

    #[test]
    fn test_parse_rev_metadata_strips_tracking_marker() {
        let (change_id, label) = parse_rev_metadata("main*\0abcdef123456\n");
        assert_eq!(change_id, "abcdef123456");
        assert_eq!(label, "main");
    }

    #[test]
    fn test_parse_rev_metadata_empty_string() {
        let (change_id, label) = parse_rev_metadata("");
        assert_eq!(change_id, "");
        assert_eq!(label, "");
    }

    #[test]
    fn test_parse_rev_metadata_no_separator() {
        let (change_id, label) = parse_rev_metadata("fallback_text");
        assert_eq!(change_id, "fallback_text");
        assert_eq!(label, "fallback_text");
    }

    // === no_snapshot helper tests ===

    #[test]
    fn test_no_snapshot_prepends_flag() {
        let args = no_snapshot(&["diff", "--from", "@-", "--to", "@"]);
        assert_eq!(args[0], "--ignore-working-copy");
        assert_eq!(args[1], "diff");
        assert_eq!(args.len(), 6);
    }

    // === classify_event tests ===

    #[test]
    fn test_classify_source_file() {
        let vcs = JjVcs::new(PathBuf::from("/repo")).unwrap();
        assert_eq!(
            vcs.classify_event(Path::new("/repo/src/main.rs")),
            VcsEventType::Source
        );
    }

    #[test]
    fn test_classify_jj_op_store_as_internal() {
        let vcs = JjVcs::new(PathBuf::from("/repo")).unwrap();
        assert_eq!(
            vcs.classify_event(Path::new("/repo/.jj/repo/op_store/heads")),
            VcsEventType::Internal
        );
    }

    #[test]
    fn test_classify_jj_working_copy() {
        let vcs = JjVcs::new(PathBuf::from("/repo")).unwrap();
        assert_eq!(
            vcs.classify_event(Path::new("/repo/.jj/working_copy/checkout")),
            VcsEventType::RevisionChange
        );
    }

    #[test]
    fn test_classify_jj_internal() {
        let vcs = JjVcs::new(PathBuf::from("/repo")).unwrap();
        assert_eq!(
            vcs.classify_event(Path::new("/repo/.jj/repo/store/something")),
            VcsEventType::Internal
        );
    }

    #[test]
    fn test_classify_path_outside_repo() {
        let vcs = JjVcs::new(PathBuf::from("/repo")).unwrap();
        assert_eq!(
            vcs.classify_event(Path::new("/other/file.rs")),
            VcsEventType::Source
        );
    }

    // === watch_paths tests ===

    #[test]
    fn test_watch_paths() {
        use crate::vcs::Vcs;
        let vcs = JjVcs::new(PathBuf::from("/repo")).unwrap();
        let paths = vcs.watch_paths();
        assert!(paths.files.contains(&PathBuf::from("/repo/.jj/working_copy/checkout")));
        assert!(paths.recursive_dirs.contains(&PathBuf::from("/repo/.jj/repo/op_store")));
    }

    // === is_transient_jj_error tests ===

    #[test]
    fn test_transient_error_stale() {
        assert!(is_transient_jj_error("The working copy is stale"));
    }

    #[test]
    fn test_not_transient_error() {
        assert!(!is_transient_jj_error("fatal: not a jj repository"));
        assert!(!is_transient_jj_error("Error: revision not found"));
        assert!(!is_transient_jj_error(""));
    }

    // === run_jj_with_retry tests ===

    #[test]
    fn test_run_jj_with_retry_succeeds_on_first_attempt() {
        if !jj_available() { return; }

        let temp = tempfile::TempDir::new().unwrap();
        Command::new("jj").args(["git", "init"]).current_dir(temp.path()).output().unwrap();

        let output = run_jj_with_retry(temp.path(), &["log", "--limit", "1"]).unwrap();
        assert!(output.status.success());
    }

    #[test]
    fn test_run_jj_with_retry_returns_failure_for_permanent_error() {
        if !jj_available() { return; }

        let temp = tempfile::TempDir::new().unwrap();
        Command::new("jj").args(["git", "init"]).current_dir(temp.path()).output().unwrap();

        let output = run_jj_with_retry(temp.path(), &["log", "-r", "nonexistent_rev_xyz"]).unwrap();
        assert!(!output.status.success());
    }

    // === Integration tests (require jj installed) ===

    fn jj_available() -> bool {
        Command::new("jj").arg("--version").output().is_ok_and(|o| o.status.success())
    }

    #[test]
    fn test_jj_refresh_detects_modified_file() {
        if !jj_available() { return; }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "initial\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "modified\n").unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        assert!(!result.files.is_empty(), "should detect changed file");
        assert!(!result.lines.is_empty(), "should produce diff lines");

        // Verify the file header is a normal header, not a "(deleted)" header
        let has_committed = result.lines.iter().any(|l| l.source == LineSource::Committed);
        assert!(has_committed, "modified lines should have Committed source");
        let header = &result.lines[0];
        assert_eq!(header.source, LineSource::FileHeader, "first line should be file header");
        assert!(!header.content.contains("(deleted)"),
            "modified file should not have deletion header, got: {}", header.content);
    }

    #[test]
    fn test_jj_refresh_detects_new_file() {
        if !jj_available() { return; }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("existing.txt"), "content\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("new_file.txt"), "new content\n").unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        assert!(!result.files.is_empty(), "should detect new file");
        let has_new_content = result.lines.iter().any(|l| l.content.contains("new content"));
        assert!(has_new_content, "should contain new file content in diff lines");
    }

    #[test]
    fn test_jj_comparison_context() {
        if !jj_available() { return; }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "content\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let ctx = vcs.comparison_context().unwrap();

        assert!(!ctx.to_label.is_empty(), "should have a to label");
        assert!(!ctx.from_label.is_empty(), "should have a from label");

        let base_id = vcs.base_identifier().unwrap();
        assert!(!base_id.is_empty(), "should have a base identifier");
    }

    #[test]
    fn test_jj_base_file_bytes() {
        if !jj_available() { return; }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "original\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "changed\n").unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let base = vcs.base_file_bytes("file.txt").unwrap();
        assert_eq!(base.unwrap(), b"original\n");

        let working = vcs.working_file_bytes("file.txt").unwrap();
        assert_eq!(working.unwrap(), b"changed\n");
    }

    #[test]
    fn test_jj_refresh_detects_deleted_file() {
        if !jj_available() { return; }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("doomed.txt"), "goodbye\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        std::fs::remove_file(repo.join("doomed.txt")).unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        assert!(!result.files.is_empty(), "should detect deleted file");
        let header = &result.lines[0];
        assert!(header.content.contains("(deleted)"),
            "deleted file should have deletion header, got: {}", header.content);
        let has_deleted_source = result.lines.iter().any(|l| l.source == LineSource::DeletedBase);
        assert!(has_deleted_source, "deleted file lines should have DeletedBase source");
    }

    #[test]
    fn test_jj_single_file_diff_handles_rename() {
        if !jj_available() { return; }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("original.txt"), "line1\nline2\nline3\nline4\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        std::fs::rename(repo.join("original.txt"), repo.join("renamed.txt")).unwrap();
        std::fs::write(repo.join("renamed.txt"), "line1\nline2\nline3\nmodified\n").unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let diff = vcs.single_file_diff("renamed.txt");
        assert!(diff.is_some(), "should produce a diff for renamed file");

        let diff = diff.unwrap();
        let header = &diff.lines[0];
        assert!(
            header.content.contains("original.txt"),
            "rename header should reference old filename, got: {}",
            header.content
        );
    }

    #[test]
    fn test_jj_get_repo_root() {
        if !jj_available() { return; }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();

        let root = get_repo_root(repo).unwrap();
        // Canonicalize both to handle /tmp vs /private/tmp on macOS
        let expected = repo.canonicalize().unwrap();
        let actual = root.canonicalize().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_jj_refresh_returns_base_label_with_bookmark() {
        if !jj_available() { return; }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "content\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        Command::new("jj").args(["bookmark", "set", "my-base", "-r", "@-"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "changed\n").unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        assert_eq!(result.base_label.as_deref(), Some("my-base"));
    }

    #[test]
    fn test_jj_refresh_returns_base_label_as_change_id_without_bookmark() {
        if !jj_available() { return; }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "content\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "changed\n").unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        let base_label = result.base_label.expect("should have base_label");
        assert!(!base_label.is_empty());
        assert_eq!(base_label, result.base_identifier, "without bookmark, base_label should match change_id");
    }

    #[test]
    fn test_jj_bookmark_strips_tracking_marker() {
        if !jj_available() { return; }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "content\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        Command::new("jj").args(["bookmark", "set", "my-branch"]).current_dir(repo).output().unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let label = vcs.rev_label("@");
        assert_eq!(label, "my-branch", "should strip trailing * from bookmark name");
    }

    #[test]
    fn test_jj_refresh_parallel_with_multiple_files() {
        if !jj_available() { return; }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        for i in 0..6 {
            std::fs::write(repo.join(format!("file{i}.txt")), "initial\n").unwrap();
        }
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        for i in 0..6 {
            std::fs::write(repo.join(format!("file{i}.txt")), format!("modified {i}\n")).unwrap();
        }

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        assert_eq!(result.files.len(), 6, "should detect all 6 changed files");
        assert!(!result.base_identifier.is_empty());
    }

    #[test]
    fn test_jj_rev_metadata_no_snapshot_with_bookmark() {
        if !jj_available() { return; }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "content\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        Command::new("jj").args(["bookmark", "set", "test-bm", "-r", "@-"]).current_dir(repo).output().unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let (change_id, label) = vcs.rev_metadata_no_snapshot("@-");

        assert!(!change_id.is_empty());
        assert_eq!(label, "test-bm");
    }

    #[test]
    fn test_jj_rev_metadata_no_snapshot_without_bookmark() {
        if !jj_available() { return; }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "content\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let (change_id, label) = vcs.rev_metadata_no_snapshot("@-");

        assert!(!change_id.is_empty());
        assert_eq!(label, change_id, "without bookmark, label should be change_id");
    }
}
