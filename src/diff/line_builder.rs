//! Line construction helpers for DiffLine.
//!
//! These associated functions create specific types of diff lines:
//! file headers, image markers, elided sections, etc.

use super::{DiffLine, LineSource};

impl DiffLine {
    /// Create a file header line for a new or modified file.
    pub fn file_header(path: &str) -> Self {
        let mut line = Self::new(LineSource::FileHeader, path.to_string(), ' ', None);
        line.file_path = Some(path.to_string());
        line
    }

    /// Create a file header line for a deleted file. `deleted_lines` annotates
    /// the header with the removed-line count (e.g. `foo.rs (deleted, -162)`) so
    /// the count is visible even while the file is auto-collapsed; a count of 0
    /// (empty file) omits it.
    pub fn deleted_file_header(path: &str, deleted_lines: usize) -> Self {
        let label = if deleted_lines > 0 {
            format!("{path} (deleted, -{deleted_lines})")
        } else {
            format!("{path} (deleted)")
        };
        let mut line = Self::new(LineSource::FileHeader, label, ' ', None);
        line.file_path = Some(path.to_string());
        line
    }

    /// Create a file header line for a renamed file.
    pub fn renamed_file_header(old_path: &str, new_path: &str) -> Self {
        let mut line = Self::new(
            LineSource::FileHeader,
            format!("{} → {}", old_path, new_path),
            ' ',
            None,
        );
        line.file_path = Some(new_path.to_string());
        line
    }

    /// Create an image marker line (UI layer will render actual image).
    pub fn image_marker(path: &str) -> Self {
        let mut line = Self::new(LineSource::Base, "[image]".to_string(), ' ', None);
        line.file_path = Some(path.to_string());
        line
    }

    /// Create an elided lines marker (shows count of hidden lines).
    pub fn elided(count: usize) -> Self {
        Self::new(LineSource::Elided, format!("{} lines", count), ' ', None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_header() {
        let header = DiffLine::file_header("src/main.rs");
        assert_eq!(header.source, LineSource::FileHeader);
        assert_eq!(header.content, "src/main.rs");
        assert_eq!(header.file_path, Some("src/main.rs".to_string()));
    }

    #[test]
    fn test_deleted_file_header() {
        let header = DiffLine::deleted_file_header("old/file.rs", 162);
        assert_eq!(header.source, LineSource::FileHeader);
        assert_eq!(header.content, "old/file.rs (deleted, -162)");
        assert_eq!(header.file_path, Some("old/file.rs".to_string()));
    }

    #[test]
    fn test_deleted_file_header_empty_omits_count() {
        let header = DiffLine::deleted_file_header("empty.txt", 0);
        assert_eq!(header.content, "empty.txt (deleted)");
    }

    #[test]
    fn test_renamed_file_header() {
        let header = DiffLine::renamed_file_header("old/path.rs", "new/path.rs");
        assert_eq!(header.source, LineSource::FileHeader);
        assert_eq!(header.content, "old/path.rs → new/path.rs");
        assert_eq!(header.file_path, Some("new/path.rs".to_string()));
    }

    #[test]
    fn test_image_marker() {
        let marker = DiffLine::image_marker("image.png");
        assert_eq!(marker.source, LineSource::Base);
        assert_eq!(marker.content, "[image]");
        assert!(marker.is_image_marker());
    }

    #[test]
    fn test_elided() {
        let elided = DiffLine::elided(42);
        assert_eq!(elided.source, LineSource::Elided);
        assert_eq!(elided.content, "42 lines");
        assert_eq!(elided.file_path, None);
    }
}
