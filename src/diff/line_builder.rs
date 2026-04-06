//! Line construction helpers for DiffLine.
//!
//! These associated functions create specific types of diff lines:
//! file headers, image markers, elided sections, etc.

use super::{DiffLine, LineSource};

impl DiffLine {
    /// Create a file header line for a new or modified file.
    pub fn file_header(path: &str) -> Self {
        Self {
            source: LineSource::FileHeader,
            content: path.to_string(),
            prefix: ' ',
            line_number: None,
            file_path: Some(path.to_string()),
            inline_spans: Vec::new(),
            old_content: None,
            change_source: None,
            in_current_bookmark: None,
            block_idx: None,
            move_target: None,
        }
    }

    /// Create a file header line for a deleted file.
    pub fn deleted_file_header(path: &str) -> Self {
        Self {
            source: LineSource::FileHeader,
            content: format!("{} (deleted)", path),
            prefix: ' ',
            line_number: None,
            file_path: Some(path.to_string()),
            inline_spans: Vec::new(),
            old_content: None,
            change_source: None,
            in_current_bookmark: None,
            block_idx: None,
            move_target: None,
        }
    }

    /// Create a file header line for a renamed file.
    pub fn renamed_file_header(old_path: &str, new_path: &str) -> Self {
        Self {
            source: LineSource::FileHeader,
            content: format!("{} → {}", old_path, new_path),
            prefix: ' ',
            line_number: None,
            file_path: Some(new_path.to_string()),
            inline_spans: Vec::new(),
            old_content: None,
            change_source: None,
            in_current_bookmark: None,
            block_idx: None,
            move_target: None,
        }
    }

    /// Create an image marker line (UI layer will render actual image).
    pub fn image_marker(path: &str) -> Self {
        Self {
            source: LineSource::Base,
            content: "[image]".to_string(),
            prefix: ' ',
            line_number: None,
            file_path: Some(path.to_string()),
            inline_spans: Vec::new(),
            old_content: None,
            change_source: None,
            in_current_bookmark: None,
            block_idx: None,
            move_target: None,
        }
    }

    /// Create an elided lines marker (shows count of hidden lines).
    pub fn elided(count: usize) -> Self {
        Self {
            source: LineSource::Elided,
            content: format!("{} lines", count),
            prefix: ' ',
            line_number: None,
            file_path: None,
            inline_spans: Vec::new(),
            old_content: None,
            change_source: None,
            in_current_bookmark: None,
            block_idx: None,
            move_target: None,
        }
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
        let header = DiffLine::deleted_file_header("old/file.rs");
        assert_eq!(header.source, LineSource::FileHeader);
        assert_eq!(header.content, "old/file.rs (deleted)");
        assert_eq!(header.file_path, Some("old/file.rs".to_string()));
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
