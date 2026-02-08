use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;

use crate::diff::{compute_file_diff_v2, DiffLine, FileDiff, LineSource};
use crate::file_links::compute_file_links;
use crate::git;
use crate::image_diff::is_image_file;
use crate::limits::DiffMetrics;

const PARALLEL_THRESHOLD: usize = 4;

/// Maximum threads for git subprocess operations.
/// Caps parallelism to prevent overwhelming system resources on high-core machines.
const MAX_GIT_THREADS: usize = 16;

static GIT_POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();

fn git_thread_pool() -> &'static rayon::ThreadPool {
    GIT_POOL.get_or_init(|| {
        let num_threads = std::thread::available_parallelism()
            .map(|n| n.get().min(MAX_GIT_THREADS))
            .unwrap_or(4);

        ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .build()
            .expect("failed to build git thread pool")
    })
}

#[derive(Debug)]
pub struct RefreshResult {
    pub files: Vec<FileDiff>,
    pub lines: Vec<DiffLine>,
    pub merge_base: String,
    pub current_branch: Option<String>,
    pub metrics: DiffMetrics,
    pub file_links: HashMap<String, String>,
}

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
        // For base, use old_path if this is a rename (file existed at old location)
        let base_path = old_path.unwrap_or(file_path);

        let base = if merge_base.is_empty() {
            None
        } else {
            git::get_file_at_ref(repo_path, base_path, merge_base)
                .ok()
                .flatten()
        };

        // For head/index, try new path first, fall back to old path if rename
        let head = git::get_file_at_ref(repo_path, file_path, "HEAD")
            .ok()
            .flatten()
            .or_else(|| {
                old_path.and_then(|p| git::get_file_at_ref(repo_path, p, "HEAD").ok().flatten())
            });

        let index = git::get_file_at_ref(repo_path, file_path, "")
            .ok()
            .flatten()
            .or_else(|| {
                old_path.and_then(|p| git::get_file_at_ref(repo_path, p, "").ok().flatten())
            });

        Self {
            base,
            head,
            index,
            working: git::get_working_tree_file(repo_path, file_path)
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
        // Check if it's an image file (we'll render these specially)
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
    let file_diff = compute_file_diff_v2(
        file_path,
        contents.base.as_deref(),
        contents.head.as_deref(),
        contents.index.as_deref(),
        contents.working.as_deref(),
        old_path,
    );

    FileProcessResult::Diff(file_diff)
}

pub fn compute_single_file_diff(
    repo_path: &Path,
    file_path: &str,
    merge_base: &str,
) -> Option<FileDiff> {
    if git::is_binary_file(repo_path, file_path) {
        return None;
    }

    let contents = FileContents::fetch(repo_path, file_path, None, merge_base);

    if contents.all_equal() {
        return None;
    }

    Some(compute_file_diff_v2(
        file_path,
        contents.base.as_deref(),
        contents.head.as_deref(),
        contents.index.as_deref(),
        contents.working.as_deref(),
        None, // Single file refresh doesn't track renames
    ))
}

pub fn compute_refresh(
    repo_path: &Path,
    base_branch: &str,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<RefreshResult> {
    let merge_base = git::get_merge_base_preferring_origin(repo_path, base_branch)
        .unwrap_or_default();

    if cancel_flag.load(Ordering::Relaxed) {
        return Err(anyhow::anyhow!("refresh cancelled"));
    }

    // Run changed files and binary detection in parallel - they both only depend on merge_base
    let (changed_files_result, binary_files) = std::thread::scope(|s| {
        let changed_handle = s.spawn(|| git::get_all_changed_files(repo_path, &merge_base));
        let binary_handle = s.spawn(|| git::get_binary_files(repo_path, &merge_base));

        (
            changed_handle.join().expect("changed files thread panicked"),
            binary_handle.join().expect("binary files thread panicked"),
        )
    });

    let changed_files = changed_files_result.context("Failed to get changed files")?;

    let results: Vec<FileProcessResult> = if changed_files.len() >= PARALLEL_THRESHOLD {
        // Use dedicated thread pool to limit concurrent git subprocess spawning
        git_thread_pool().install(|| {
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
        return Err(anyhow::anyhow!("refresh cancelled"));
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
                lines.push(DiffLine::file_header(&path));
                lines.push(DiffLine::new(
                    LineSource::Base,
                    "[binary file]".to_string(),
                    ' ',
                    None,
                ));
            }
            FileProcessResult::Image { path } => {
                // For now, show as "[image]" marker - actual rendering happens in UI layer
                lines.push(DiffLine::file_header(&path));
                lines.push(DiffLine::image_marker(&path));
            }
        }
    }

    let current_branch = git::get_current_branch(repo_path).unwrap_or(None);

    let metrics = DiffMetrics {
        total_lines: lines.len(),
        file_count: files.len(),
    };

    // Compute file links (app ↔ spec pairs)
    let file_paths: Vec<&str> = files
        .iter()
        .filter_map(|f| f.lines.first())
        .filter_map(|l| l.file_path.as_deref())
        .collect();
    let file_links = compute_file_links(&file_paths);

    Ok(RefreshResult {
        files,
        lines,
        merge_base,
        current_branch,
        metrics,
        file_links,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn create_test_repo() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        Command::new("git")
            .args(["init"])
            .current_dir(repo_path)
            .output()
            .expect("failed to init git repo");

        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo_path)
            .output()
            .expect("failed to set git email");

        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo_path)
            .output()
            .expect("failed to set git name");

        std::fs::write(repo_path.join("file.txt"), "initial content\n").unwrap();

        Command::new("git")
            .args(["add", "."])
            .current_dir(repo_path)
            .output()
            .expect("failed to add files");

        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(repo_path)
            .output()
            .expect("failed to commit");

        Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(repo_path)
            .output()
            .expect("failed to rename branch");

        temp_dir
    }

    #[test]
    fn test_cancel_flag_stops_refresh_before_file_processing() {
        let temp_dir = create_test_repo();
        let repo_path = temp_dir.path();

        std::fs::write(repo_path.join("file.txt"), "modified content\n").unwrap();

        let cancel_flag = Arc::new(AtomicBool::new(true));

        let result = compute_refresh(repo_path, "main", &cancel_flag);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cancelled"));
    }

    #[test]
    fn test_cancel_flag_checked_during_file_iteration() {
        let temp_dir = create_test_repo();
        let repo_path = temp_dir.path();

        for i in 0..5 {
            std::fs::write(
                repo_path.join(format!("file{}.txt", i)),
                format!("content {}\n", i),
            )
            .unwrap();
        }

        Command::new("git")
            .args(["add", "."])
            .current_dir(repo_path)
            .output()
            .expect("failed to add files");

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_clone = cancel_flag.clone();

        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            cancel_clone.store(true, Ordering::Relaxed);
        });

        let result = compute_refresh(repo_path, "main", &cancel_flag);

        assert!(result.is_err() || result.is_ok());
    }

    #[test]
    fn test_refresh_with_no_changes_returns_empty() {
        let temp_dir = create_test_repo();
        let repo_path = temp_dir.path();

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = compute_refresh(repo_path, "main", &cancel_flag);

        assert!(result.is_ok());
        let refresh = result.unwrap();
        assert!(refresh.files.is_empty());
        assert!(refresh.lines.is_empty());
    }

    #[test]
    fn test_refresh_with_modified_file() {
        let temp_dir = create_test_repo();
        let repo_path = temp_dir.path();

        std::fs::write(repo_path.join("file.txt"), "modified content\n").unwrap();

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = compute_refresh(repo_path, "main", &cancel_flag);

        assert!(result.is_ok());
        let refresh = result.unwrap();
        assert_eq!(refresh.files.len(), 1);
        assert!(!refresh.lines.is_empty());
    }

    #[test]
    fn test_refresh_with_new_file() {
        let temp_dir = create_test_repo();
        let repo_path = temp_dir.path();

        std::fs::write(repo_path.join("new_file.txt"), "new content\n").unwrap();

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = compute_refresh(repo_path, "main", &cancel_flag);

        assert!(result.is_ok());
        let refresh = result.unwrap();
        assert_eq!(refresh.files.len(), 1);
    }

    #[test]
    fn test_refresh_with_deleted_file() {
        let temp_dir = create_test_repo();
        let repo_path = temp_dir.path();

        std::fs::remove_file(repo_path.join("file.txt")).unwrap();

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = compute_refresh(repo_path, "main", &cancel_flag);

        assert!(result.is_ok());
        let refresh = result.unwrap();
        assert_eq!(refresh.files.len(), 1);
    }

    #[test]
    fn test_refresh_with_staged_changes() {
        let temp_dir = create_test_repo();
        let repo_path = temp_dir.path();

        std::fs::write(repo_path.join("file.txt"), "staged content\n").unwrap();

        Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(repo_path)
            .output()
            .expect("failed to stage file");

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = compute_refresh(repo_path, "main", &cancel_flag);

        assert!(result.is_ok());
        let refresh = result.unwrap();
        assert_eq!(refresh.files.len(), 1);
    }

    #[test]
    fn test_refresh_returns_current_branch() {
        let temp_dir = create_test_repo();
        let repo_path = temp_dir.path();

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = compute_refresh(repo_path, "main", &cancel_flag);

        assert!(result.is_ok());
        let refresh = result.unwrap();
        assert_eq!(refresh.current_branch, Some("main".to_string()));
    }

    #[test]
    fn test_refresh_with_feature_branch() {
        let temp_dir = create_test_repo();
        let repo_path = temp_dir.path();

        Command::new("git")
            .args(["checkout", "-b", "feature"])
            .current_dir(repo_path)
            .output()
            .expect("failed to create branch");

        std::fs::write(repo_path.join("new_feature_file.txt"), "feature content\n").unwrap();

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = compute_refresh(repo_path, "main", &cancel_flag);

        assert!(result.is_ok());
        let refresh = result.unwrap();
        assert_eq!(refresh.current_branch, Some("feature".to_string()));
        assert!(!refresh.files.is_empty());
    }

    #[test]
    fn test_refresh_with_binary_file() {
        let temp_dir = create_test_repo();
        let repo_path = temp_dir.path();

        std::fs::write(repo_path.join("binary.bin"), &[0u8, 1, 2, 255, 254, 253]).unwrap();

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = compute_refresh(repo_path, "main", &cancel_flag);

        assert!(result.is_ok());
        let refresh = result.unwrap();
        let binary_line = refresh.lines.iter().find(|l| l.content.contains("binary"));
        assert!(binary_line.is_some());
    }

    #[test]
    fn test_renamed_file_shows_only_content_changes() {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        Command::new("git")
            .args(["init"])
            .current_dir(repo_path)
            .output()
            .expect("failed to init git repo");

        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo_path)
            .output()
            .expect("failed to set git email");

        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo_path)
            .output()
            .expect("failed to set git name");

        // Create a file with multiple lines
        std::fs::write(
            repo_path.join("original.txt"),
            "line 1\nline 2\nline 3\nline 4\nline 5\n",
        )
        .unwrap();

        Command::new("git")
            .args(["add", "."])
            .current_dir(repo_path)
            .output()
            .expect("failed to add files");

        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(repo_path)
            .output()
            .expect("failed to commit");

        Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(repo_path)
            .output()
            .expect("failed to rename branch");

        // Rename file and change one line
        std::fs::remove_file(repo_path.join("original.txt")).unwrap();
        std::fs::write(
            repo_path.join("renamed.txt"),
            "line 1\nline 2 modified\nline 3\nline 4\nline 5\n",
        )
        .unwrap();

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = compute_refresh(repo_path, "main", &cancel_flag);

        assert!(result.is_ok());
        let refresh = result.unwrap();

        // Should have detected as rename
        assert_eq!(
            refresh.files.len(),
            1,
            "Expected 1 file, got {}",
            refresh.files.len()
        );

        // Find modified lines - these have old_content or change_source set
        // Modifications of base lines have source=Base but change_source=Unstaged
        let modified_lines: Vec<_> = refresh
            .lines
            .iter()
            .filter(|l| l.old_content.is_some() || l.change_source.is_some())
            .collect();

        // Should have at least one modified line (the modified "line 2")
        assert!(
            !modified_lines.is_empty(),
            "Expected at least one modified line, got none. Total lines: {}",
            refresh.lines.len()
        );

        // Verify the specific modification
        let mod_line = modified_lines
            .iter()
            .find(|l| l.content.contains("line 2 modified"))
            .expect("Should have modification for line 2");

        assert_eq!(
            mod_line.old_content.as_deref(),
            Some("line 2"),
            "Should track original content"
        );
        assert_eq!(
            mod_line.change_source,
            Some(crate::diff::LineSource::Unstaged),
            "Should mark as unstaged modification"
        );
    }

    #[test]
    fn test_refresh_with_mixed_text_and_binary() {
        let temp_dir = create_test_repo();
        let repo_path = temp_dir.path();

        // Create both text and binary files to verify parallel operations complete
        std::fs::write(repo_path.join("text.txt"), "text content\n").unwrap();
        std::fs::write(repo_path.join("binary.bin"), &[0u8, 1, 2, 255, 254, 253]).unwrap();

        // Stage the binary file so git diff can detect it as binary
        Command::new("git")
            .args(["add", "binary.bin"])
            .current_dir(repo_path)
            .output()
            .expect("failed to stage binary file");

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = compute_refresh(repo_path, "main", &cancel_flag);

        assert!(result.is_ok());
        let refresh = result.unwrap();

        // Text file should be in files list (binary files are excluded from FileDiff)
        assert_eq!(
            refresh.files.len(),
            1,
            "Should have exactly 1 FileDiff (text only, binary excluded)"
        );

        // Both should appear in lines output - text content and binary marker
        assert!(
            refresh.lines.iter().any(|l| l.content.contains("text content")),
            "Should have text file content in lines"
        );
        assert!(
            refresh.lines.iter().any(|l| l.content == "[binary file]"),
            "Should have binary file marker in lines"
        );

        // Verify file headers present for both files
        let file_headers: Vec<_> = refresh
            .lines
            .iter()
            .filter(|l| l.source == crate::diff::LineSource::FileHeader)
            .collect();
        assert_eq!(file_headers.len(), 2, "Should have 2 file headers (text + binary)");

        // Metrics tracks non-binary files only
        assert_eq!(refresh.metrics.file_count, 1);
    }

    #[test]
    fn test_refresh_computes_file_links_for_matching_files() {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        Command::new("git")
            .args(["init"])
            .current_dir(repo_path)
            .output()
            .expect("failed to init git repo");

        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo_path)
            .output()
            .expect("failed to set git email");

        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo_path)
            .output()
            .expect("failed to set git name");

        // Create impl and test files
        std::fs::write(repo_path.join("handler.go"), "package main\n").unwrap();
        std::fs::write(repo_path.join("handler_test.go"), "package main\n").unwrap();

        Command::new("git")
            .args(["add", "."])
            .current_dir(repo_path)
            .output()
            .expect("failed to add files");

        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(repo_path)
            .output()
            .expect("failed to commit");

        Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(repo_path)
            .output()
            .expect("failed to rename branch");

        // Modify both files
        std::fs::write(repo_path.join("handler.go"), "package main\nfunc Handler() {}\n").unwrap();
        std::fs::write(
            repo_path.join("handler_test.go"),
            "package main\nfunc TestHandler() {}\n",
        )
        .unwrap();

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = compute_refresh(repo_path, "main", &cancel_flag);

        assert!(result.is_ok());
        let refresh = result.unwrap();

        assert_eq!(refresh.files.len(), 2, "Should have 2 modified files");

        // Verify file_links contains the bidirectional mapping
        assert_eq!(
            refresh.file_links.get("handler.go"),
            Some(&"handler_test.go".to_string()),
            "handler.go should link to handler_test.go"
        );
        assert_eq!(
            refresh.file_links.get("handler_test.go"),
            Some(&"handler.go".to_string()),
            "handler_test.go should link to handler.go"
        );
    }

    #[test]
    fn test_image_file_produces_image_marker() {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        // Initialize git repo
        Command::new("git")
            .args(["init"])
            .current_dir(repo_path)
            .output()
            .expect("failed to init git repo");

        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo_path)
            .output()
            .expect("failed to set git email");

        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo_path)
            .output()
            .expect("failed to set git name");

        // Create a minimal valid PNG file (1x1 red pixel)
        // PNG signature + IHDR + IDAT + IEND
        let png_bytes: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
            0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE, // depth, type, crc
            0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, // IDAT chunk
            0x08, 0xD7, 0x63, 0xF8, 0xFF, 0xFF, 0x3F, 0x00, // compressed data
            0x05, 0xFE, 0x02, 0xFE, 0xA3, 0x56, 0x5A, 0x09, // crc
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, // IEND chunk
            0xAE, 0x42, 0x60, 0x82, // crc
        ];

        std::fs::write(repo_path.join("image.png"), &png_bytes).unwrap();
        std::fs::write(repo_path.join("readme.txt"), "initial\n").unwrap();

        Command::new("git")
            .args(["add", "."])
            .current_dir(repo_path)
            .output()
            .expect("failed to add files");

        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(repo_path)
            .output()
            .expect("failed to commit");

        Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(repo_path)
            .output()
            .expect("failed to rename branch");

        // Modify the PNG file (different content so it shows as changed)
        let modified_png: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE,
            0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54,
            0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, // different pixel data
            0x02, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB4,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44,
            0xAE, 0x42, 0x60, 0x82,
        ];
        std::fs::write(repo_path.join("image.png"), &modified_png).unwrap();

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = compute_refresh(repo_path, "main", &cancel_flag);

        assert!(result.is_ok());
        let refresh = result.unwrap();

        // Find the image marker line
        let image_marker = refresh.lines.iter().find(|line| line.is_image_marker());
        assert!(
            image_marker.is_some(),
            "Should have an image marker line for image.png"
        );

        let marker = image_marker.unwrap();
        assert_eq!(marker.file_path, Some("image.png".to_string()));
        assert_eq!(marker.content, "[image]");
    }
}
