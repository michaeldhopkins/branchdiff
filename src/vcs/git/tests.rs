use super::*;
use crate::vcs::shared::run_vcs_with_retry;
use crate::vcs::VcsEventType;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn test_parse_git_version_standard() {
    let version = commands::parse_git_version("git version 2.34.1").unwrap();
    assert_eq!(version.major, 2);
    assert_eq!(version.minor, 34);
    assert_eq!(version.patch, 1);
}

#[test]
fn test_parse_git_version_apple() {
    let version = commands::parse_git_version("git version 2.50.1 (Apple Git-155)").unwrap();
    assert_eq!(version.major, 2);
    assert_eq!(version.minor, 50);
    assert_eq!(version.patch, 1);
}

#[test]
fn test_parse_git_version_no_patch() {
    let version = commands::parse_git_version("git version 2.38").unwrap();
    assert_eq!(version.major, 2);
    assert_eq!(version.minor, 38);
    assert_eq!(version.patch, 0);
}

#[test]
fn test_parse_git_version_windows() {
    // Windows Git for Windows format
    let version = commands::parse_git_version("git version 2.39.2.windows.1").unwrap();
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
    let version = commands::parse_git_version("git version 2.34.1.ubuntu1").unwrap();
    assert_eq!(version.major, 2);
    assert_eq!(version.minor, 34);
    assert_eq!(version.patch, 1);
}

#[test]
fn test_parse_git_version_with_newline() {
    // Real output includes trailing newline
    let version = commands::parse_git_version("git version 2.34.1\n").unwrap();
    assert_eq!(version.major, 2);
    assert_eq!(version.minor, 34);
    assert_eq!(version.patch, 1);
}

#[test]
fn test_parse_git_version_old_git() {
    let version = commands::parse_git_version("git version 1.8.0").unwrap();
    assert_eq!(version.major, 1);
    assert_eq!(version.minor, 8);
    assert_eq!(version.patch, 0);
}

#[test]
fn test_parse_git_version_invalid_no_prefix() {
    let result = commands::parse_git_version("2.34.1");
    assert!(result.is_err());
}

#[test]
fn test_parse_git_version_invalid_empty() {
    let result = commands::parse_git_version("");
    assert!(result.is_err());
}

#[test]
fn test_parse_git_version_invalid_no_minor() {
    let result = commands::parse_git_version("git version 2");
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
    let result = changed_files::parse_diff_line(line);
    assert_eq!(result, Some(changed_files::FileTransition {
        from: None,
        to: Some("path/to/new_file.rs".to_string()),
    }));
}

#[test]
fn test_parse_diff_line_deleted() {
    let line = "D\tpath/to/deleted_file.rs";
    let result = changed_files::parse_diff_line(line);
    assert_eq!(result, Some(changed_files::FileTransition {
        from: Some("path/to/deleted_file.rs".to_string()),
        to: None,
    }));
}

#[test]
fn test_parse_diff_line_modified() {
    let line = "M\tpath/to/modified_file.rs";
    let result = changed_files::parse_diff_line(line);
    assert_eq!(result, Some(changed_files::FileTransition {
        from: Some("path/to/modified_file.rs".to_string()),
        to: Some("path/to/modified_file.rs".to_string()),
    }));
}

#[test]
fn test_parse_diff_line_renamed() {
    let line = "R100\told/path.rs\tnew/path.rs";
    let result = changed_files::parse_diff_line(line);
    assert_eq!(result, Some(changed_files::FileTransition {
        from: Some("old/path.rs".to_string()),
        to: Some("new/path.rs".to_string()),
    }));
}

#[test]
fn test_parse_diff_line_renamed_partial_similarity() {
    let line = "R075\told/path.rs\tnew/path.rs";
    let result = changed_files::parse_diff_line(line);
    assert_eq!(result, Some(changed_files::FileTransition {
        from: Some("old/path.rs".to_string()),
        to: Some("new/path.rs".to_string()),
    }));
}

#[test]
fn test_parse_diff_line_malformed_empty() {
    assert_eq!(changed_files::parse_diff_line(""), None);
}

#[test]
fn test_parse_diff_line_malformed_single_part() {
    assert_eq!(changed_files::parse_diff_line("M"), None);
}

#[test]
fn test_parse_diff_line_unrecognized_status() {
    // Unknown status should return None
    assert_eq!(changed_files::parse_diff_line("X\tsome/file.rs"), None);
}

#[test]
fn test_file_transition_current_path_added() {
    let t = changed_files::FileTransition {
        from: None,
        to: Some("new_file.rs".to_string()),
    };
    assert_eq!(t.current_path(), Some("new_file.rs"));
}

#[test]
fn test_file_transition_current_path_deleted() {
    let t = changed_files::FileTransition {
        from: Some("deleted_file.rs".to_string()),
        to: None,
    };
    assert_eq!(t.current_path(), Some("deleted_file.rs"));
}

#[test]
fn test_file_transition_current_path_modified() {
    let t = changed_files::FileTransition {
        from: Some("file.rs".to_string()),
        to: Some("file.rs".to_string()),
    };
    assert_eq!(t.current_path(), Some("file.rs"));
}

#[test]
fn test_file_transition_current_path_renamed() {
    let t = changed_files::FileTransition {
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
    let merge_base = commands::get_merge_base(temp.path(), "main").unwrap();

    fs::create_dir(temp.path().join("new_folder")).unwrap();
    fs::write(temp.path().join("new_folder/file1.txt"), "content1\n").unwrap();
    fs::write(temp.path().join("new_folder/file2.txt"), "content2\n").unwrap();

    let changed = changed_files::get_all_changed_files(temp.path(), &merge_base).unwrap();
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
    let result = changed_files::get_all_changed_files(repo_path, "");
    assert!(result.is_ok());

    // Should find the untracked file via git status
    let changed = result.unwrap();
    let paths: Vec<&str> = changed.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"new_file.txt"));
}

#[test]
fn test_is_transient_error_index_lock() {
    // index.lock is the most common transient error
    assert!(commands::is_transient_error(
        "fatal: Unable to create '/path/.git/index.lock': File exists."
    ));
}

#[test]
fn test_is_transient_error_other_lock() {
    // Other lock files should also be retried
    assert!(commands::is_transient_error(
        "Unable to create '/path/.git/refs/heads/main.lock': File exists"
    ));
}

#[test]
fn test_is_transient_error_not_lock() {
    // Non-lock errors should not be retried
    assert!(!commands::is_transient_error("fatal: not a git repository"));
    assert!(!commands::is_transient_error("fatal: pathspec 'foo' did not match any files"));
    assert!(!commands::is_transient_error(""));
}

#[test]
fn test_run_vcs_with_retry_git_succeeds_on_first_attempt() {
    let output = run_vcs_with_retry(
        "git", Path::new("."), &["--version"], commands::is_transient_error,
    )
    .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("git version"));
}

#[test]
fn test_run_vcs_with_retry_git_returns_failure_for_permanent_error() {
    let output = run_vcs_with_retry(
        "git", Path::new("."),
        &["rev-parse", "--verify", "nonexistent-branch-12345"],
        commands::is_transient_error,
    )
    .unwrap();

    assert!(!output.status.success());
}

#[test]
fn test_get_binary_files_empty_repo() {
    let temp = create_test_repo();
    let merge_base = commands::get_merge_base(temp.path(), "main").unwrap();

    // No binary files in a clean repo
    let binaries = get_binary_files(temp.path(), &merge_base);
    assert!(binaries.is_empty());
}

#[test]
fn test_get_binary_files_detects_binary() {
    let temp = create_test_repo();
    let merge_base = commands::get_merge_base(temp.path(), "main").unwrap();

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
    let merge_base = commands::get_merge_base(temp.path(), "main").unwrap();

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

    let merge_base = commands::get_merge_base(temp.path(), "main").unwrap();

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
    let merge_base = commands::get_merge_base(temp.path(), "main").unwrap();

    // Rename file using filesystem mv (not git mv)
    fs::rename(
        temp.path().join("file.txt"),
        temp.path().join("renamed.txt"),
    )
    .unwrap();

    let changed = changed_files::get_all_changed_files(temp.path(), &merge_base).unwrap();

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

    let merge_base = commands::get_merge_base(path, "main").unwrap();

    // Rename file and modify content slightly (add one line)
    fs::remove_file(path.join("file.txt")).unwrap();
    fs::write(
        path.join("renamed.txt"),
        format!("{}line 9\n", original_content),
    )
    .unwrap();

    let changed = changed_files::get_all_changed_files(path, &merge_base).unwrap();

    // Git's rename detection should still match (>50% similarity)
    assert_eq!(changed.len(), 1, "Should detect as rename despite small change");
    let renamed = &changed[0];
    assert_eq!(renamed.path, "renamed.txt");
    assert_eq!(renamed.old_path, Some("file.txt".to_string()));
}

#[test]
fn test_no_rename_detection_when_only_deleted() {
    let temp = create_test_repo();
    let merge_base = commands::get_merge_base(temp.path(), "main").unwrap();

    // Only delete, no new files
    fs::remove_file(temp.path().join("file.txt")).unwrap();

    let changed = changed_files::get_all_changed_files(temp.path(), &merge_base).unwrap();

    // Should be a plain deletion
    assert_eq!(changed.len(), 1);
    let deleted = &changed[0];
    assert_eq!(deleted.path, "file.txt");
    assert!(deleted.old_path.is_none());
}

#[test]
fn test_no_rename_detection_when_only_new_file() {
    let temp = create_test_repo();
    let merge_base = commands::get_merge_base(temp.path(), "main").unwrap();

    // Only add, no deletions
    fs::write(temp.path().join("new_file.txt"), "new content\n").unwrap();

    let changed = changed_files::get_all_changed_files(temp.path(), &merge_base).unwrap();

    // Should be a plain addition
    assert_eq!(changed.len(), 1);
    let added = &changed[0];
    assert_eq!(added.path, "new_file.txt");
    assert!(added.old_path.is_none());
}

#[test]
fn test_staged_rename_with_git_mv() {
    let temp = create_test_repo();
    let merge_base = commands::get_merge_base(temp.path(), "main").unwrap();

    // Use git mv to rename (creates a staged rename)
    git_cmd(temp.path(), &["mv", "file.txt", "staged_rename.txt"]);

    let changed = changed_files::get_all_changed_files(temp.path(), &merge_base).unwrap();

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

    let merge_base = commands::get_merge_base(path, "main").unwrap();

    // Rename within subdirectory using filesystem mv
    fs::rename(
        path.join("subdir/file.txt"),
        path.join("subdir/renamed.txt"),
    )
    .unwrap();

    let changed = changed_files::get_all_changed_files(path, &merge_base).unwrap();

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
    let old = changed_files::find_rename_source(temp.path(), "renamed.txt", &merge_base);
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
    let old = changed_files::find_rename_source(temp.path(), "file.txt", &merge_base);
    assert!(old.is_none());
}

#[test]
fn test_find_rename_source_empty_merge_base() {
    let old = changed_files::find_rename_source(Path::new("/tmp"), "file.txt", "");
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
    assert_eq!(header.source, crate::diff::LineSource::FileHeader);
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
