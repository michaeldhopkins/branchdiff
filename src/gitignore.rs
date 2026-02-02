//! Gitignore-aware file filtering for the file watcher.
//!
//! Supports nested .gitignore files with correct directory scoping.

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Manages gitignore state for filtering file change events.
///
/// Supports hierarchical gitignore files - each .gitignore only applies
/// to its directory and descendants. Patterns are checked from deepest
/// (highest precedence) to shallowest (lowest precedence).
pub struct GitignoreFilter {
    /// Per-directory matchers, keyed by directory path relative to repo_root.
    /// Empty PathBuf ("") represents the repo root.
    matchers: HashMap<PathBuf, Gitignore>,

    /// Global gitignore matcher (~/.config/git/ignore or core.excludesFile)
    global_matcher: Gitignore,

    /// .git/info/exclude matcher
    exclude_matcher: Gitignore,

    repo_root: Box<Path>,
}

impl GitignoreFilter {
    /// Build a new filter from the repository root.
    ///
    /// Discovers and loads patterns from:
    /// - All .gitignore files (root and nested)
    /// - .git/info/exclude
    /// - Global gitignore (~/.config/git/ignore)
    pub fn new(repo_root: &Path) -> Self {
        let matchers = Self::build_matchers(repo_root);
        let exclude_matcher = Self::build_exclude_matcher(repo_root);
        let global_matcher = Self::build_global_matcher();

        Self {
            matchers,
            global_matcher,
            exclude_matcher,
            repo_root: repo_root.into(),
        }
    }

    /// Rebuild all matchers (call when any .gitignore changes).
    pub fn rebuild(&mut self) {
        self.matchers = Self::build_matchers(&self.repo_root);
        self.exclude_matcher = Self::build_exclude_matcher(&self.repo_root);
        // Global matcher doesn't need rebuild - it's outside the repo
    }

    /// Check if a path should be ignored.
    ///
    /// Checks matchers in order of precedence:
    /// 1. Nested .gitignore files (deepest first)
    /// 2. .git/info/exclude
    /// 3. Global gitignore
    pub fn is_ignored(&self, path: &Path) -> bool {
        let relative = path.strip_prefix(&*self.repo_root).unwrap_or(path);
        let is_dir = path.is_dir();

        // Collect all parent directories from deepest to shallowest
        let mut dirs_to_check: Vec<&Path> = Vec::new();
        let mut current = relative;
        while let Some(parent) = current.parent() {
            dirs_to_check.push(parent);
            current = parent;
        }

        // Check from deepest (highest precedence) to shallowest
        for dir in &dirs_to_check {
            if let Some(matcher) = self.matchers.get(*dir) {
                // Make path relative to this matcher's directory
                let rel_to_matcher = if dir.as_os_str().is_empty() {
                    relative
                } else {
                    relative.strip_prefix(dir).unwrap_or(relative)
                };

                // Use matched_path_or_any_parents to handle directory patterns like "target/"
                // which should also match files inside the directory
                match matcher.matched_path_or_any_parents(rel_to_matcher, is_dir) {
                    ignore::Match::Ignore(_) => return true,
                    ignore::Match::Whitelist(_) => return false, // Negation pattern
                    ignore::Match::None => continue,
                }
            }
        }

        // Check .git/info/exclude (lower precedence than .gitignore files)
        match self
            .exclude_matcher
            .matched_path_or_any_parents(relative, is_dir)
        {
            ignore::Match::Ignore(_) => return true,
            ignore::Match::Whitelist(_) => return false,
            ignore::Match::None => {}
        }

        // Check global gitignore (lowest precedence)
        matches!(
            self.global_matcher
                .matched_path_or_any_parents(relative, is_dir),
            ignore::Match::Ignore(_)
        )
    }

    /// Check if this path is a gitignore file that should trigger rebuild.
    pub fn is_gitignore_file(path: &Path) -> bool {
        let file_name = path.file_name().and_then(|n| n.to_str());
        matches!(file_name, Some(".gitignore")) || path.ends_with(".git/info/exclude")
    }

    /// Discover all .gitignore files and build per-directory matchers.
    fn build_matchers(repo_root: &Path) -> HashMap<PathBuf, Gitignore> {
        let mut matchers = HashMap::new();

        // Use WalkBuilder to discover .gitignore files while respecting existing ignores
        for entry in WalkBuilder::new(repo_root)
            .hidden(false) // Don't skip hidden files (we want .gitignore)
            .git_ignore(true) // Respect gitignore during traversal
            .git_global(true)
            .git_exclude(true)
            .filter_entry(|e| e.file_name() != ".git")
            .build()
            .flatten()
        {
            let path = entry.path();
            if path.file_name() == Some(OsStr::new(".gitignore"))
                && let Some((dir, matcher)) = Self::build_matcher_for_gitignore(path, repo_root)
            {
                matchers.insert(dir, matcher);
            }
        }

        matchers
    }

    /// Build a matcher for a single .gitignore file.
    /// Returns the relative directory path and the matcher.
    fn build_matcher_for_gitignore(
        gitignore_path: &Path,
        repo_root: &Path,
    ) -> Option<(PathBuf, Gitignore)> {
        let abs_dir = gitignore_path.parent()?;
        let rel_dir = abs_dir.strip_prefix(repo_root).unwrap_or(Path::new(""));

        // Build matcher with the gitignore's directory as root
        let mut builder = GitignoreBuilder::new(abs_dir);
        let _ = builder.add(gitignore_path);

        let matcher = builder.build().unwrap_or_else(|_| Gitignore::empty());
        Some((rel_dir.to_path_buf(), matcher))
    }

    /// Build matcher for .git/info/exclude
    fn build_exclude_matcher(repo_root: &Path) -> Gitignore {
        let exclude_path = repo_root.join(".git/info/exclude");
        if exclude_path.exists() {
            let mut builder = GitignoreBuilder::new(repo_root);
            let _ = builder.add(&exclude_path);
            builder.build().unwrap_or_else(|_| Gitignore::empty())
        } else {
            Gitignore::empty()
        }
    }

    /// Build global gitignore matcher (~/.config/git/ignore or core.excludesFile)
    fn build_global_matcher() -> Gitignore {
        let builder = GitignoreBuilder::new(PathBuf::new());
        let (gitignore, _err) = builder.build_global();
        gitignore
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

    // --- New tests for nested .gitignore support ---

    #[test]
    fn test_nested_gitignore_scoping() {
        let temp = create_test_repo();
        let path = temp.path();

        // Root ignores *.log
        fs::write(path.join(".gitignore"), "*.log\n").unwrap();

        // subdir ignores *.txt (should only apply to subdir/)
        fs::create_dir_all(path.join("subdir")).unwrap();
        fs::write(path.join("subdir/.gitignore"), "*.txt\n").unwrap();

        let filter = GitignoreFilter::new(path);

        // Root patterns apply everywhere
        assert!(filter.is_ignored(&path.join("test.log")));
        assert!(filter.is_ignored(&path.join("subdir/test.log")));

        // Subdir patterns only apply to subdir
        assert!(
            !filter.is_ignored(&path.join("test.txt")),
            "Root .txt should NOT be ignored"
        );
        assert!(
            filter.is_ignored(&path.join("subdir/test.txt")),
            "subdir .txt SHOULD be ignored"
        );
    }

    #[test]
    fn test_nested_gitignore_negation() {
        let temp = create_test_repo();
        let path = temp.path();

        // Root ignores all *.log
        fs::write(path.join(".gitignore"), "*.log\n").unwrap();

        // subdir un-ignores important.log
        fs::create_dir_all(path.join("subdir")).unwrap();
        fs::write(path.join("subdir/.gitignore"), "!important.log\n").unwrap();

        let filter = GitignoreFilter::new(path);

        assert!(filter.is_ignored(&path.join("test.log")));
        assert!(filter.is_ignored(&path.join("subdir/test.log")));
        assert!(
            !filter.is_ignored(&path.join("subdir/important.log")),
            "Nested negation should un-ignore important.log"
        );
    }

    #[test]
    fn test_deeply_nested_gitignore() {
        let temp = create_test_repo();
        let path = temp.path();

        fs::create_dir_all(path.join("a/b/c")).unwrap();
        fs::write(path.join("a/.gitignore"), "*.a\n").unwrap();
        fs::write(path.join("a/b/.gitignore"), "*.b\n").unwrap();
        fs::write(path.join("a/b/c/.gitignore"), "*.c\n").unwrap();

        let filter = GitignoreFilter::new(path);

        // *.a pattern only applies at a/ and below
        assert!(!filter.is_ignored(&path.join("test.a")));
        assert!(filter.is_ignored(&path.join("a/test.a")));
        assert!(filter.is_ignored(&path.join("a/b/test.a")));
        assert!(filter.is_ignored(&path.join("a/b/c/test.a")));

        // *.b pattern only applies at a/b/ and below
        assert!(!filter.is_ignored(&path.join("test.b")));
        assert!(!filter.is_ignored(&path.join("a/test.b")));
        assert!(filter.is_ignored(&path.join("a/b/test.b")));
        assert!(filter.is_ignored(&path.join("a/b/c/test.b")));

        // *.c pattern only applies at a/b/c/
        assert!(!filter.is_ignored(&path.join("test.c")));
        assert!(!filter.is_ignored(&path.join("a/test.c")));
        assert!(!filter.is_ignored(&path.join("a/b/test.c")));
        assert!(filter.is_ignored(&path.join("a/b/c/test.c")));
    }

    #[test]
    fn test_nested_gitignore_rebuild() {
        let temp = create_test_repo();
        let path = temp.path();

        fs::create_dir_all(path.join("subdir")).unwrap();
        fs::write(path.join(".gitignore"), "").unwrap();

        let mut filter = GitignoreFilter::new(path);

        // Initially nothing is ignored
        assert!(!filter.is_ignored(&path.join("subdir/test.txt")));

        // Add nested gitignore
        fs::write(path.join("subdir/.gitignore"), "*.txt\n").unwrap();
        filter.rebuild();

        // Now subdir .txt files should be ignored
        assert!(filter.is_ignored(&path.join("subdir/test.txt")));
    }

    #[test]
    fn test_nested_gitignore_directory_pattern() {
        let temp = create_test_repo();
        let path = temp.path();

        // subdir/.gitignore ignores build/
        fs::create_dir_all(path.join("subdir/build/output")).unwrap();
        fs::write(path.join("subdir/.gitignore"), "build/\n").unwrap();

        let filter = GitignoreFilter::new(path);

        // Root build/ should NOT be ignored (pattern is in subdir/)
        assert!(!filter.is_ignored(&path.join("build")));
        assert!(!filter.is_ignored(&path.join("build/output/file")));

        // subdir/build/ SHOULD be ignored
        assert!(filter.is_ignored(&path.join("subdir/build")));
        assert!(filter.is_ignored(&path.join("subdir/build/output")));
        assert!(filter.is_ignored(&path.join("subdir/build/output/file.txt")));
    }
}
