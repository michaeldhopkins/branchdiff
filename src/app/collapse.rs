//! File collapse/expand logic for branchdiff.
//!
//! Handles collapsing files in the diff view:
//! - Lock files and generated files are auto-collapsed by default
//! - Deleted files are auto-collapsed
//! - Users can manually toggle any file's collapsed state

use std::collections::HashSet;

use crate::diff::FileDiff;

/// File patterns that should be collapsed by default (lock files, generated files)
pub(super) const AUTO_COLLAPSE_PATTERNS: &[&str] = &[
    // Ruby/Rails
    "Gemfile.lock",
    "db/schema.rb",
    "db/structure.sql",
    // JavaScript/Node
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    // Rust
    "Cargo.lock",
    // Python
    "poetry.lock",
    "Pipfile.lock",
    "pdm.lock",
    // PHP
    "composer.lock",
    // .NET
    "packages.lock.json",
    // Go
    "go.sum",
    // Elixir
    "mix.lock",
    // Swift
    "Package.resolved",
    // Dart/Flutter
    "pubspec.lock",
];

/// Check if a file path matches any auto-collapse pattern (lock files)
pub(super) fn should_auto_collapse(path: &str) -> bool {
    AUTO_COLLAPSE_PATTERNS
        .iter()
        .any(|pattern| path.ends_with(pattern))
}

/// Check if a file header indicates a deleted file
pub(super) fn is_deleted_file(header_content: &str) -> bool {
    header_content.ends_with("(deleted)")
}

/// Auto-collapse files matching lock/generated file patterns and deleted files.
/// Also uncollapse files that were previously collapsed due to deletion but are
/// no longer deleted (unless they match auto-collapse patterns).
/// Skips files that have been manually toggled by the user.
pub(super) fn auto_collapse_files(
    files: &[FileDiff],
    collapsed_files: &mut HashSet<String>,
    manually_toggled: &HashSet<String>,
) {
    for file in files {
        if let Some(first_line) = file.lines.first()
            && let Some(ref path) = first_line.file_path
        {
            if manually_toggled.contains(path) {
                continue;
            }

            let is_pattern_match = should_auto_collapse(path);
            let is_deleted = is_deleted_file(&first_line.content);

            if is_pattern_match || is_deleted {
                collapsed_files.insert(path.clone());
            } else if collapsed_files.contains(path) {
                // File was collapsed but is no longer deleted and doesn't match
                // auto-collapse patterns - uncollapse it
                collapsed_files.remove(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_auto_collapse_patterns() {
        // Should match
        assert!(should_auto_collapse("Cargo.lock"));
        assert!(should_auto_collapse("path/to/Cargo.lock"));
        assert!(should_auto_collapse("package-lock.json"));
        assert!(should_auto_collapse("yarn.lock"));
        assert!(should_auto_collapse("Gemfile.lock"));
        assert!(should_auto_collapse("db/schema.rb"));

        // Should not match
        assert!(!should_auto_collapse("Cargo.toml"));
        assert!(!should_auto_collapse("package.json"));
        assert!(!should_auto_collapse("main.rs"));
        assert!(!should_auto_collapse("lock.txt"));
    }

    #[test]
    fn test_is_deleted_file() {
        assert!(is_deleted_file("path/to/file.rs (deleted)"));
        assert!(is_deleted_file("file.txt (deleted)"));

        assert!(!is_deleted_file("path/to/file.rs"));
        assert!(!is_deleted_file("deleted_file.rs"));
        assert!(!is_deleted_file("(deleted) file.rs"));
    }
}
