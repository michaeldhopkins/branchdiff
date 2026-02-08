//! File link detection for app-spec file pairs
//!
//! Detects 1:1 relationships between implementation files and their corresponding
//! test/spec files using generic filename transformations.

use std::collections::{HashMap, HashSet};

/// Compute bidirectional file links from a list of file paths.
///
/// For each path, generates candidate related paths (test ↔ implementation)
/// and checks if exactly one candidate exists in the provided paths.
/// If so, creates a bidirectional link.
///
/// A link is only created if both sides have a unique match. This prevents
/// ambiguous situations where an impl file has multiple test candidates.
pub fn compute_file_links(paths: &[&str]) -> HashMap<String, String> {
    let path_set: HashSet<&str> = paths.iter().copied().collect();
    let mut links = HashMap::new();

    for path in paths {
        if links.contains_key(*path) {
            continue;
        }

        let candidates = generate_candidates(path);
        let matches: Vec<_> = candidates
            .iter()
            .filter(|c| path_set.contains(c.as_str()))
            .collect();

        if matches.len() == 1 {
            // Also check reverse: the matched file should only have one candidate too
            let reverse_candidates = generate_candidates(matches[0]);
            let reverse_matches: Vec<_> = reverse_candidates
                .iter()
                .filter(|c| path_set.contains(c.as_str()))
                .collect();

            if reverse_matches.len() == 1 {
                links.insert((*path).to_string(), matches[0].clone());
                links.insert(matches[0].clone(), (*path).to_string());
            }
        }
    }

    links
}

/// Check if a filename looks like a test file.
fn is_test_file(filename: &str) -> bool {
    filename.contains("_test.")
        || filename.contains("_spec.")
        || filename.contains(".test.")
        || filename.contains(".spec.")
        || filename.starts_with("test_")
        || filename.ends_with("_test")
        || filename.ends_with("_spec")
}

/// Check if a path is in a test directory.
fn is_in_test_directory(path: &str) -> bool {
    path.starts_with("test/")
        || path.starts_with("tests/")
        || path.starts_with("spec/")
        || path.contains("/test/")
        || path.contains("/tests/")
        || path.contains("/spec/")
}

/// Generate all candidate related paths for a given path.
fn generate_candidates(path: &str) -> Vec<String> {
    let (dir, filename) = split_path(path);
    let (name, ext) = split_extension(filename);

    if is_test_file(filename) || is_in_test_directory(path) {
        generate_impl_candidates(dir, name, ext)
    } else {
        generate_test_candidates(dir, name, ext)
    }
}

/// Split a path into (directory, filename).
/// Returns ("", filename) if no directory separator.
fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(idx) => (&path[..idx], &path[idx + 1..]),
        None => ("", path),
    }
}

/// Split a filename into (name, extension including dot).
/// Returns (filename, "") if no extension.
fn split_extension(filename: &str) -> (&str, &str) {
    match filename.rfind('.') {
        Some(idx) if idx > 0 => (&filename[..idx], &filename[idx..]),
        _ => (filename, ""),
    }
}

/// Join directory and filename, handling empty directory.
fn join_path(dir: &str, filename: &str) -> String {
    if dir.is_empty() {
        filename.to_string()
    } else {
        format!("{}/{}", dir, filename)
    }
}

/// Generate test file candidates for an implementation file.
fn generate_test_candidates(dir: &str, name: &str, ext: &str) -> Vec<String> {
    let mut candidates = Vec::new();

    // Same directory suffix variants
    candidates.push(join_path(dir, &format!("{}_test{}", name, ext)));
    candidates.push(join_path(dir, &format!("{}_spec{}", name, ext)));
    candidates.push(join_path(dir, &format!("{}.test{}", name, ext)));
    candidates.push(join_path(dir, &format!("{}.spec{}", name, ext)));

    // Same directory prefix variant
    candidates.push(join_path(dir, &format!("test_{}{}", name, ext)));

    // Parallel directory variants
    if let Some(rest) = strip_prefix_segment(dir, "src") {
        let test_dir = prepend_segment("test", rest);
        let tests_dir = prepend_segment("tests", rest);
        let spec_dir = prepend_segment("spec", rest);

        candidates.push(join_path(&test_dir, &format!("{}{}", name, ext)));
        candidates.push(join_path(&tests_dir, &format!("{}{}", name, ext)));
        candidates.push(join_path(&spec_dir, &format!("{}{}", name, ext)));

        // With suffix
        candidates.push(join_path(&test_dir, &format!("{}_test{}", name, ext)));
        candidates.push(join_path(&tests_dir, &format!("{}_test{}", name, ext)));
        candidates.push(join_path(&spec_dir, &format!("{}_spec{}", name, ext)));
    }

    if let Some(rest) = strip_prefix_segment(dir, "lib") {
        let test_dir = prepend_segment("test", rest);
        let spec_dir = prepend_segment("spec", rest);

        candidates.push(join_path(&test_dir, &format!("{}{}", name, ext)));
        candidates.push(join_path(&spec_dir, &format!("{}{}", name, ext)));

        // With suffix
        candidates.push(join_path(&test_dir, &format!("{}_test{}", name, ext)));
        candidates.push(join_path(&spec_dir, &format!("{}_spec{}", name, ext)));
    }

    if let Some(rest) = strip_prefix_segment(dir, "app") {
        let test_dir = prepend_segment("test", rest);
        let spec_dir = prepend_segment("spec", rest);

        candidates.push(join_path(&test_dir, &format!("{}{}", name, ext)));
        candidates.push(join_path(&spec_dir, &format!("{}{}", name, ext)));

        // With suffix
        candidates.push(join_path(&test_dir, &format!("{}_test{}", name, ext)));
        candidates.push(join_path(&spec_dir, &format!("{}_spec{}", name, ext)));
    }

    candidates
}

/// Generate implementation file candidates for a test file.
fn generate_impl_candidates(dir: &str, name: &str, ext: &str) -> Vec<String> {
    let mut candidates = Vec::new();

    // Remove test suffixes/prefixes from name
    let base_name = strip_test_affixes(name);

    // Same directory (test file in same dir as impl) - only if we stripped a test affix
    // Files like test/handler.go shouldn't generate test/handler.go as a candidate
    if base_name != name {
        candidates.push(join_path(dir, &format!("{}{}", base_name, ext)));
    }

    // Parallel directory variants (test → src/lib/app)
    if let Some(rest) = strip_prefix_segment(dir, "test") {
        let src_dir = prepend_segment("src", rest);
        let lib_dir = prepend_segment("lib", rest);
        let app_dir = prepend_segment("app", rest);

        candidates.push(join_path(&src_dir, &format!("{}{}", base_name, ext)));
        candidates.push(join_path(&lib_dir, &format!("{}{}", base_name, ext)));
        candidates.push(join_path(&app_dir, &format!("{}{}", base_name, ext)));
    }

    if let Some(rest) = strip_prefix_segment(dir, "tests") {
        let src_dir = prepend_segment("src", rest);
        let lib_dir = prepend_segment("lib", rest);
        let app_dir = prepend_segment("app", rest);

        candidates.push(join_path(&src_dir, &format!("{}{}", base_name, ext)));
        candidates.push(join_path(&lib_dir, &format!("{}{}", base_name, ext)));
        candidates.push(join_path(&app_dir, &format!("{}{}", base_name, ext)));
    }

    if let Some(rest) = strip_prefix_segment(dir, "spec") {
        let src_dir = prepend_segment("src", rest);
        let lib_dir = prepend_segment("lib", rest);
        let app_dir = prepend_segment("app", rest);

        candidates.push(join_path(&src_dir, &format!("{}{}", base_name, ext)));
        candidates.push(join_path(&lib_dir, &format!("{}{}", base_name, ext)));
        candidates.push(join_path(&app_dir, &format!("{}{}", base_name, ext)));
    }

    candidates
}

/// Strip a prefix segment from a directory path.
/// e.g., strip_prefix_segment("src/foo/bar", "src") => Some("foo/bar")
fn strip_prefix_segment<'a>(dir: &'a str, prefix: &str) -> Option<&'a str> {
    if dir == prefix {
        Some("")
    } else if let Some(rest) = dir.strip_prefix(prefix) {
        rest.strip_prefix('/')
    } else {
        None
    }
}

/// Prepend a segment to a directory path.
/// e.g., prepend_segment("test", "foo/bar") => "test/foo/bar"
fn prepend_segment(prefix: &str, rest: &str) -> String {
    if rest.is_empty() {
        prefix.to_string()
    } else {
        format!("{}/{}", prefix, rest)
    }
}

/// Strip test-related suffixes and prefixes from a filename stem.
fn strip_test_affixes(name: &str) -> &str {
    // Try suffixes first
    if let Some(base) = name.strip_suffix("_test") {
        return base;
    }
    if let Some(base) = name.strip_suffix("_spec") {
        return base;
    }
    if let Some(base) = name.strip_suffix(".test") {
        return base;
    }
    if let Some(base) = name.strip_suffix(".spec") {
        return base;
    }

    // Try prefix
    if let Some(base) = name.strip_prefix("test_") {
        return base;
    }

    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_test_file() {
        // Test files
        assert!(is_test_file("handler_test.go"));
        assert!(is_test_file("user_spec.rb"));
        assert!(is_test_file("utils.test.ts"));
        assert!(is_test_file("utils.spec.js"));
        assert!(is_test_file("test_user.py"));

        // Not test files
        assert!(!is_test_file("handler.go"));
        assert!(!is_test_file("user.rb"));
        assert!(!is_test_file("utils.ts"));
        assert!(!is_test_file("testing.py"));
        assert!(!is_test_file("contest.rb"));
    }

    #[test]
    fn test_split_path() {
        assert_eq!(split_path("src/foo/bar.rs"), ("src/foo", "bar.rs"));
        assert_eq!(split_path("file.rs"), ("", "file.rs"));
        assert_eq!(split_path("a/b"), ("a", "b"));
    }

    #[test]
    fn test_split_extension() {
        assert_eq!(split_extension("file.rs"), ("file", ".rs"));
        assert_eq!(split_extension("file.test.ts"), ("file.test", ".ts"));
        assert_eq!(split_extension("file"), ("file", ""));
        assert_eq!(split_extension(".gitignore"), (".gitignore", ""));
    }

    #[test]
    fn test_strip_test_affixes() {
        assert_eq!(strip_test_affixes("handler_test"), "handler");
        assert_eq!(strip_test_affixes("user_spec"), "user");
        assert_eq!(strip_test_affixes("utils.test"), "utils");
        assert_eq!(strip_test_affixes("utils.spec"), "utils");
        assert_eq!(strip_test_affixes("test_user"), "user");
        assert_eq!(strip_test_affixes("handler"), "handler");
    }

    // === Same directory tests ===

    #[test]
    fn test_go_style_same_directory() {
        let paths = &["handler.go", "handler_test.go"];
        let links = compute_file_links(paths);

        assert_eq!(links.get("handler.go"), Some(&"handler_test.go".to_string()));
        assert_eq!(links.get("handler_test.go"), Some(&"handler.go".to_string()));
    }

    #[test]
    fn test_jest_style_same_directory() {
        let paths = &["utils.ts", "utils.test.ts"];
        let links = compute_file_links(paths);

        assert_eq!(links.get("utils.ts"), Some(&"utils.test.ts".to_string()));
        assert_eq!(links.get("utils.test.ts"), Some(&"utils.ts".to_string()));
    }

    #[test]
    fn test_jest_spec_style_same_directory() {
        let paths = &["utils.js", "utils.spec.js"];
        let links = compute_file_links(paths);

        assert_eq!(links.get("utils.js"), Some(&"utils.spec.js".to_string()));
        assert_eq!(links.get("utils.spec.js"), Some(&"utils.js".to_string()));
    }

    #[test]
    fn test_pytest_style_same_directory() {
        let paths = &["user.py", "test_user.py"];
        let links = compute_file_links(paths);

        assert_eq!(links.get("user.py"), Some(&"test_user.py".to_string()));
        assert_eq!(links.get("test_user.py"), Some(&"user.py".to_string()));
    }

    // === Parallel directory tests ===

    #[test]
    fn test_rails_app_to_spec() {
        let paths = &["app/models/user.rb", "spec/models/user_spec.rb"];
        let links = compute_file_links(paths);

        assert_eq!(
            links.get("app/models/user.rb"),
            Some(&"spec/models/user_spec.rb".to_string())
        );
        assert_eq!(
            links.get("spec/models/user_spec.rb"),
            Some(&"app/models/user.rb".to_string())
        );
    }

    #[test]
    fn test_src_to_test_directory() {
        let paths = &["src/handler.go", "test/handler_test.go"];
        let links = compute_file_links(paths);

        assert_eq!(
            links.get("src/handler.go"),
            Some(&"test/handler_test.go".to_string())
        );
        assert_eq!(
            links.get("test/handler_test.go"),
            Some(&"src/handler.go".to_string())
        );
    }

    #[test]
    fn test_src_to_tests_directory() {
        let paths = &["src/utils.py", "tests/utils_test.py"];
        let links = compute_file_links(paths);

        assert_eq!(
            links.get("src/utils.py"),
            Some(&"tests/utils_test.py".to_string())
        );
    }

    #[test]
    fn test_lib_to_spec() {
        let paths = &["lib/user.rb", "spec/user_spec.rb"];
        let links = compute_file_links(paths);

        assert_eq!(
            links.get("lib/user.rb"),
            Some(&"spec/user_spec.rb".to_string())
        );
    }

    #[test]
    fn test_nested_parallel_directories() {
        let paths = &[
            "app/controllers/api/v1/users_controller.rb",
            "spec/controllers/api/v1/users_controller_spec.rb",
        ];
        let links = compute_file_links(paths);

        assert_eq!(
            links.get("app/controllers/api/v1/users_controller.rb"),
            Some(&"spec/controllers/api/v1/users_controller_spec.rb".to_string())
        );
    }

    // === Ambiguity tests ===

    #[test]
    fn test_ambiguous_no_link() {
        // Both foo_test.js and foo.test.js exist - ambiguous
        let paths = &["foo.js", "foo_test.js", "foo.test.js"];
        let links = compute_file_links(paths);

        // foo.js has two matches, so no link
        assert!(links.get("foo.js").is_none());
    }

    #[test]
    fn test_no_match_no_link() {
        let paths = &["foo.js", "bar.js"];
        let links = compute_file_links(paths);

        assert!(links.get("foo.js").is_none());
        assert!(links.get("bar.js").is_none());
    }

    // === Edge cases ===

    #[test]
    fn test_empty_paths() {
        let paths: &[&str] = &[];
        let links = compute_file_links(paths);
        assert!(links.is_empty());
    }

    #[test]
    fn test_single_file() {
        let paths = &["foo.rs"];
        let links = compute_file_links(paths);
        assert!(links.is_empty());
    }

    #[test]
    fn test_multiple_pairs() {
        let paths = &[
            "handler.go",
            "handler_test.go",
            "user.go",
            "user_test.go",
        ];
        let links = compute_file_links(paths);

        assert_eq!(links.get("handler.go"), Some(&"handler_test.go".to_string()));
        assert_eq!(links.get("user.go"), Some(&"user_test.go".to_string()));
        assert_eq!(links.len(), 4); // 2 pairs = 4 entries
    }

    #[test]
    fn test_only_test_files_no_impl() {
        let paths = &["handler_test.go", "user_test.go"];
        let links = compute_file_links(paths);

        // No implementation files exist, so no links
        assert!(links.is_empty());
    }

    #[test]
    fn test_file_without_extension() {
        let paths = &["Makefile", "Makefile_test"];
        let links = compute_file_links(paths);

        // Should still work
        assert_eq!(links.get("Makefile"), Some(&"Makefile_test".to_string()));
    }

    #[test]
    fn test_parallel_directory_without_suffix() {
        // Some projects put tests in test/ dir but keep same filename
        let paths = &["src/handler.go", "test/handler.go"];
        let links = compute_file_links(paths);

        assert_eq!(
            links.get("src/handler.go"),
            Some(&"test/handler.go".to_string())
        );
    }

    #[test]
    fn test_generate_test_candidates_same_dir() {
        let candidates = generate_test_candidates("", "foo", ".js");

        assert!(candidates.contains(&"foo_test.js".to_string()));
        assert!(candidates.contains(&"foo_spec.js".to_string()));
        assert!(candidates.contains(&"foo.test.js".to_string()));
        assert!(candidates.contains(&"foo.spec.js".to_string()));
        assert!(candidates.contains(&"test_foo.js".to_string()));
    }

    #[test]
    fn test_generate_impl_candidates() {
        let candidates = generate_impl_candidates("spec/models", "user_spec", ".rb");

        assert!(candidates.contains(&"spec/models/user.rb".to_string()));
        assert!(candidates.contains(&"app/models/user.rb".to_string()));
        assert!(candidates.contains(&"lib/models/user.rb".to_string()));
    }

    // === is_in_test_directory tests ===

    #[test]
    fn test_is_in_test_directory() {
        // In test directories
        assert!(is_in_test_directory("test/handler.go"));
        assert!(is_in_test_directory("tests/utils.py"));
        assert!(is_in_test_directory("spec/models/user.rb"));
        assert!(is_in_test_directory("src/test/helper.go"));
        assert!(is_in_test_directory("lib/tests/util.py"));
        assert!(is_in_test_directory("app/spec/model.rb"));

        // Not in test directories
        assert!(!is_in_test_directory("src/handler.go"));
        assert!(!is_in_test_directory("lib/utils.py"));
        assert!(!is_in_test_directory("app/models/user.rb"));
        assert!(!is_in_test_directory("testing/file.go"));
        assert!(!is_in_test_directory("contest/file.py"));
    }
}
