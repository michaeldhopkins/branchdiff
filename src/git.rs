use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Context, Result};

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
    let committed = get_diff_files(repo_path, merge_base, "HEAD")?;
    for path in committed {
        files.insert(path);
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

    let mut files = Vec::new();
    let output_str = String::from_utf8_lossy(&output.stdout);

    for line in output_str.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 2 {
            continue;
        }

        let path = parts.last().unwrap().to_string();
        files.push(path);
    }

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
    let output = Command::new("git")
        .args(["fetch", "origin", base_branch])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git fetch")?;

    if !output.status.success() {
        return Err(anyhow!(
            "git fetch failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(())
}

pub fn has_merge_conflicts(repo_path: &Path, base_branch: &str) -> Result<bool> {
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

    fn git_cmd(dir: &Path, args: &[&str]) {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git command failed");
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
        let result = has_merge_conflicts(temp.path(), "main");
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

        let result = has_merge_conflicts(clone.path(), "main");
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

        let result = has_merge_conflicts(clone.path(), "main");
        assert!(result.is_ok());
        assert!(result.unwrap());
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
}
