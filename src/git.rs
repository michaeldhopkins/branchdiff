use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Context, Result};

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

    let output = Command::new("git")
        .args(["show", &ref_path])
        .current_dir(repo_path)
        .output()
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

/// A file that has changes
#[derive(Debug, Clone)]
pub struct ChangedFile {
    pub path: String,
}

/// Get all files that have changes compared to merge-base, HEAD, index, or working tree
pub fn get_all_changed_files(repo_path: &Path, merge_base: &str) -> Result<Vec<ChangedFile>> {
    let mut files: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. Get committed changes (merge-base to HEAD)
    // Skip if merge_base is empty (no commits yet)
    if !merge_base.is_empty()
        && let Ok(committed) = get_diff_files(repo_path, merge_base, "HEAD")
    {
        for path in committed {
            files.insert(path);
        }
    }

    // 2. Get staged changes (HEAD to index) and unstaged changes (index to working tree)
    // Use -uall to show individual files in untracked directories
    let status_output = Command::new("git")
        .args(["status", "--porcelain=v1", "-uall"])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git status")?;

    if status_output.status.success() {
        let status_str = String::from_utf8_lossy(&status_output.stdout);
        for line in status_str.lines() {
            if line.len() < 3 {
                continue;
            }

            let path = line[3..].to_string();

            // Handle renames which have "old -> new" format
            let path = if path.contains(" -> ") {
                let parts: Vec<&str> = path.split(" -> ").collect();
                parts[1].to_string()
            } else {
                path
            };

            files.insert(path);
        }
    }

    let mut result: Vec<ChangedFile> = files
        .into_iter()
        .map(|path| ChangedFile { path })
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

/// Get files changed between two refs
fn get_diff_files(repo_path: &Path, from: &str, to: &str) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["diff", "--name-status", from, to])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git diff --name-status")?;

    if !output.status.success() {
        return Err(anyhow!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let output_str = String::from_utf8_lossy(&output.stdout);
    let files: Vec<String> = output_str
        .lines()
        .filter_map(parse_diff_line)
        .filter_map(|t| t.current_path().map(|s| s.to_string()))
        .collect();

    Ok(files)
}

/// Check if a file is binary
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

#[cfg(test)]
mod tests {
    use super::*;
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
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path();

        git_cmd(path, &["init"]);
        git_cmd(path, &["config", "user.email", "test@test.com"]);
        git_cmd(path, &["config", "user.name", "Test"]);

        fs::write(path.join("file.txt"), "initial\n").unwrap();
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
}
