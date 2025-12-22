//! Gitignore-aware file filtering for the file watcher.

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::path::Path;

/// Manages gitignore state for filtering file change events.
///
/// Rebuilds automatically when .gitignore files change.
pub struct GitignoreFilter {
    matcher: Gitignore,
    repo_root: Box<Path>,
}

impl GitignoreFilter {
    /// Build a new filter from the repository root.
    ///
    /// Loads patterns from:
    /// - .gitignore (root and nested)
    /// - .git/info/exclude
    pub fn new(repo_root: &Path) -> Self {
        let matcher = Self::build_matcher(repo_root);
        Self {
            matcher,
            repo_root: repo_root.into(),
        }
    }

    /// Rebuild the matcher (call when .gitignore changes).
    pub fn rebuild(&mut self) {
        self.matcher = Self::build_matcher(&self.repo_root);
    }

    /// Check if a path should be ignored.
    pub fn is_ignored(&self, path: &Path) -> bool {
        let relative = path.strip_prefix(&*self.repo_root).unwrap_or(path);
        let is_dir = path.is_dir();

        // Use matched_path_or_any_parents to handle directory patterns like "target/"
        // which should also match files inside the directory
        matches!(
            self.matcher.matched_path_or_any_parents(relative, is_dir),
            ignore::Match::Ignore(_)
        )
    }

    /// Check if this path is a gitignore file that should trigger rebuild.
    pub fn is_gitignore_file(path: &Path) -> bool {
        let file_name = path.file_name().and_then(|n| n.to_str());
        matches!(file_name, Some(".gitignore")) || path.ends_with(".git/info/exclude")
    }

    fn build_matcher(repo_root: &Path) -> Gitignore {
        let mut builder = GitignoreBuilder::new(repo_root);

        // Add .git/info/exclude if it exists
        let exclude_path = repo_root.join(".git/info/exclude");
        if exclude_path.exists() {
            let _ = builder.add(&exclude_path);
        }

        // Add root .gitignore
        let root_gitignore = repo_root.join(".gitignore");
        if root_gitignore.exists() {
            let _ = builder.add(&root_gitignore);
        }

        builder.build().unwrap_or_else(|_| Gitignore::empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_repo() -> TempDir {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".git/info")).unwrap();
        temp
    }

    #[test]
    fn test_basic_gitignore_matching() {
        let temp = create_test_repo();
        let path = temp.path();

        fs::write(path.join(".gitignore"), "*.log\ntarget/\n").unwrap();

        let filter = GitignoreFilter::new(path);

        assert!(filter.is_ignored(&path.join("debug.log")));
        assert!(filter.is_ignored(&path.join("target/release/binary")));
        assert!(!filter.is_ignored(&path.join("src/main.rs")));
    }

    #[test]
    fn test_negation_patterns() {
        let temp = create_test_repo();
        let path = temp.path();

        fs::write(path.join(".gitignore"), "*.log\n!important.log\n").unwrap();

        let filter = GitignoreFilter::new(path);

        assert!(filter.is_ignored(&path.join("debug.log")));
        assert!(!filter.is_ignored(&path.join("important.log")));
    }

    #[test]
    fn test_subdirectory_patterns() {
        let temp = create_test_repo();
        let path = temp.path();

        // Test patterns that match subdirectories from root .gitignore
        fs::write(path.join(".gitignore"), "subdir/*.tmp\n").unwrap();
        fs::create_dir_all(path.join("subdir")).unwrap();

        let filter = GitignoreFilter::new(path);

        assert!(filter.is_ignored(&path.join("subdir/file.tmp")));
        // Root level .tmp should not be ignored (pattern only matches subdir/)
        assert!(!filter.is_ignored(&path.join("file.tmp")));
    }

    #[test]
    fn test_git_info_exclude() {
        let temp = create_test_repo();
        let path = temp.path();

        fs::write(path.join(".git/info/exclude"), "*.secret\n").unwrap();

        let filter = GitignoreFilter::new(path);

        assert!(filter.is_ignored(&path.join("passwords.secret")));
        assert!(!filter.is_ignored(&path.join("passwords.txt")));
    }

    #[test]
    fn test_rebuild_on_gitignore_change() {
        let temp = create_test_repo();
        let path = temp.path();

        fs::write(path.join(".gitignore"), "").unwrap();
        let mut filter = GitignoreFilter::new(path);

        assert!(!filter.is_ignored(&path.join("test.log")));

        fs::write(path.join(".gitignore"), "*.log\n").unwrap();
        filter.rebuild();

        assert!(filter.is_ignored(&path.join("test.log")));
    }

    #[test]
    fn test_is_gitignore_file() {
        assert!(GitignoreFilter::is_gitignore_file(Path::new(".gitignore")));
        assert!(GitignoreFilter::is_gitignore_file(Path::new(
            "subdir/.gitignore"
        )));
        assert!(GitignoreFilter::is_gitignore_file(Path::new(
            ".git/info/exclude"
        )));
        assert!(GitignoreFilter::is_gitignore_file(Path::new(
            "/repo/.git/info/exclude"
        )));
        assert!(!GitignoreFilter::is_gitignore_file(Path::new("src/main.rs")));
        assert!(!GitignoreFilter::is_gitignore_file(Path::new(
            ".gitignore.bak"
        )));
    }

    #[test]
    fn test_directory_patterns() {
        let temp = create_test_repo();
        let path = temp.path();

        fs::write(path.join(".gitignore"), "node_modules/\n").unwrap();
        fs::create_dir_all(path.join("node_modules/pkg")).unwrap();

        let filter = GitignoreFilter::new(path);

        assert!(filter.is_ignored(&path.join("node_modules")));
        assert!(filter.is_ignored(&path.join("node_modules/pkg/index.js")));
    }

    #[test]
    fn test_empty_repo_no_gitignore() {
        let temp = create_test_repo();
        let path = temp.path();

        let filter = GitignoreFilter::new(path);

        assert!(!filter.is_ignored(&path.join("any_file.txt")));
        assert!(!filter.is_ignored(&path.join("node_modules/pkg.js")));
    }
}
