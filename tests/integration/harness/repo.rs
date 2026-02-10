//! Git repository fixture for integration tests.

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// A temporary git repository for testing.
pub struct TestRepo {
    dir: TempDir,
}

impl TestRepo {
    /// Create a new git repository in a temporary directory.
    pub fn new() -> Self {
        let dir = TempDir::new().expect("failed to create temp dir");

        // Initialize git repo
        run_git(dir.path(), &["init"]);

        // Configure git user for commits (required for git commit to work)
        run_git(dir.path(), &["config", "user.email", "test@test.com"]);
        run_git(dir.path(), &["config", "user.name", "Test User"]);

        // Create initial commit on main branch
        run_git(dir.path(), &["commit", "--allow-empty", "-m", "Initial commit"]);

        Self { dir }
    }

    /// Add a file to the repo and stage it.
    pub fn add_file(&self, path: &str, content: &str) {
        let full_path = self.dir.path().join(path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).expect("failed to create parent dirs");
        }
        std::fs::write(&full_path, content).expect("failed to write file");
        run_git(self.dir.path(), &["add", path]);
    }

    /// Commit all staged changes.
    pub fn commit(&self, msg: &str) {
        run_git(self.dir.path(), &["commit", "-m", msg]);
    }

    /// Create and checkout a new branch.
    pub fn create_branch(&self, name: &str) {
        run_git(self.dir.path(), &["checkout", "-b", name]);
    }

    /// Modify a file without staging it.
    pub fn modify_file(&self, path: &str, content: &str) {
        let full_path = self.dir.path().join(path);
        std::fs::write(full_path, content).expect("failed to write file");
    }

    /// Get the path to the repository.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run git");

    if !output.status.success() {
        panic!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
