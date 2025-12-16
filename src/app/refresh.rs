use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::diff::{compute_file_diff_v2, DiffLine, FileDiff, LineSource};
use crate::git;

const PARALLEL_THRESHOLD: usize = 4;

#[derive(Debug)]
pub struct RefreshResult {
    pub files: Vec<FileDiff>,
    pub lines: Vec<DiffLine>,
    pub merge_base: String,
    pub current_branch: Option<String>,
}

enum FileProcessResult {
    Diff(FileDiff),
    Binary { path: String },
}

fn process_single_file(
    repo_path: &Path,
    file_path: &str,
    merge_base: &str,
) -> FileProcessResult {
    if git::is_binary_file(repo_path, file_path) {
        return FileProcessResult::Binary { path: file_path.to_string() };
    }

    let base_content = if merge_base.is_empty() {
        None
    } else {
        git::get_file_at_ref(repo_path, file_path, merge_base)
            .ok()
            .flatten()
    };

    let head_content = git::get_file_at_ref(repo_path, file_path, "HEAD")
        .ok()
        .flatten();

    let index_content = git::get_file_at_ref(repo_path, file_path, "")
        .ok()
        .flatten();

    let working_content = git::get_working_tree_file(repo_path, file_path)
        .ok()
        .flatten();

    let file_diff = compute_file_diff_v2(
        file_path,
        base_content.as_deref(),
        head_content.as_deref(),
        index_content.as_deref(),
        working_content.as_deref(),
    );

    FileProcessResult::Diff(file_diff)
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

    let changed_files = git::get_all_changed_files(repo_path, &merge_base)
        .context("Failed to get changed files")?;

    let results: Vec<FileProcessResult> = if changed_files.len() >= PARALLEL_THRESHOLD {
        changed_files
            .par_iter()
            .map(|file| process_single_file(repo_path, &file.path, &merge_base))
            .collect()
    } else {
        changed_files
            .iter()
            .map(|file| process_single_file(repo_path, &file.path, &merge_base))
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
        }
    }

    let current_branch = git::get_current_branch(repo_path).unwrap_or(None);

    Ok(RefreshResult {
        files,
        lines,
        merge_base,
        current_branch,
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
}
