// Refresh pipeline has been moved to src/vcs/git.rs.
// Tests remain here to avoid moving their git repo setup helpers.

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use crate::diff::LineSource;
    use crate::vcs::git::GitVcs;
    use crate::vcs::Vcs;

    use std::process::Command;
    use tempfile::TempDir;

    fn create_test_repo() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        Command::new("git")
            .args(["init"])
            .current_dir(repo_path)
            .output()
            .expect("failed to init git repo");

        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo_path)
            .output()
            .expect("failed to set git email");

        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo_path)
            .output()
            .expect("failed to set git name");

        std::fs::write(repo_path.join("file.txt"), "initial content\n").unwrap();

        Command::new("git")
            .args(["add", "."])
            .current_dir(repo_path)
            .output()
            .expect("failed to add files");

        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(repo_path)
            .output()
            .expect("failed to commit");

        Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(repo_path)
            .output()
            .expect("failed to rename branch");

        temp_dir
    }

    fn git_cmd(dir: &std::path::Path, args: &[&str]) {
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

    #[test]
    fn test_cancel_flag_stops_refresh_before_file_processing() {
        let temp_dir = create_test_repo();
        let repo_path = temp_dir.path();
        std::fs::write(repo_path.join("file.txt"), "modified content\n").unwrap();

        let vcs = GitVcs::new(repo_path.to_path_buf()).unwrap();
        let cancel_flag = Arc::new(AtomicBool::new(true));
        let result = vcs.refresh(&cancel_flag);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cancelled"));
    }

    #[test]
    fn test_refresh_with_no_changes_returns_empty() {
        let temp_dir = create_test_repo();
        let vcs = GitVcs::new(temp_dir.path().to_path_buf()).unwrap();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel_flag);

        assert!(result.is_ok());
        let refresh = result.unwrap();
        assert!(refresh.files.is_empty());
        assert!(refresh.lines.is_empty());
    }

    #[test]
    fn test_refresh_with_modified_file() {
        let temp_dir = create_test_repo();
        std::fs::write(temp_dir.path().join("file.txt"), "modified content\n").unwrap();

        let vcs = GitVcs::new(temp_dir.path().to_path_buf()).unwrap();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel_flag);

        assert!(result.is_ok());
        let refresh = result.unwrap();
        assert_eq!(refresh.files.len(), 1);
        assert!(!refresh.lines.is_empty());
    }

    #[test]
    fn test_refresh_with_new_file() {
        let temp_dir = create_test_repo();
        std::fs::write(temp_dir.path().join("new_file.txt"), "new content\n").unwrap();

        let vcs = GitVcs::new(temp_dir.path().to_path_buf()).unwrap();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel_flag);

        assert!(result.is_ok());
        assert_eq!(result.unwrap().files.len(), 1);
    }

    #[test]
    fn test_refresh_with_deleted_file() {
        let temp_dir = create_test_repo();
        std::fs::remove_file(temp_dir.path().join("file.txt")).unwrap();

        let vcs = GitVcs::new(temp_dir.path().to_path_buf()).unwrap();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel_flag);

        assert!(result.is_ok());
        assert_eq!(result.unwrap().files.len(), 1);
    }

    #[test]
    fn test_refresh_with_staged_changes() {
        let temp_dir = create_test_repo();
        std::fs::write(temp_dir.path().join("file.txt"), "staged content\n").unwrap();
        git_cmd(temp_dir.path(), &["add", "file.txt"]);

        let vcs = GitVcs::new(temp_dir.path().to_path_buf()).unwrap();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel_flag);

        assert!(result.is_ok());
        assert_eq!(result.unwrap().files.len(), 1);
    }

    #[test]
    fn test_refresh_returns_current_branch() {
        let temp_dir = create_test_repo();
        let vcs = GitVcs::new(temp_dir.path().to_path_buf()).unwrap();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel_flag).unwrap();
        assert_eq!(result.current_branch, Some("main".to_string()));
    }

    #[test]
    fn test_refresh_with_feature_branch() {
        let temp_dir = create_test_repo();
        git_cmd(temp_dir.path(), &["checkout", "-b", "feature"]);
        std::fs::write(temp_dir.path().join("new_feature_file.txt"), "feature content\n").unwrap();

        let vcs = GitVcs::new(temp_dir.path().to_path_buf()).unwrap();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel_flag).unwrap();

        assert_eq!(result.current_branch, Some("feature".to_string()));
        assert!(!result.files.is_empty());
    }

    #[test]
    fn test_refresh_with_binary_file() {
        let temp_dir = create_test_repo();
        std::fs::write(temp_dir.path().join("binary.bin"), &[0u8, 1, 2, 255, 254, 253]).unwrap();

        let vcs = GitVcs::new(temp_dir.path().to_path_buf()).unwrap();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel_flag).unwrap();

        let binary_line = result.lines.iter().find(|l| l.content.contains("binary"));
        assert!(binary_line.is_some());
    }

    #[test]
    fn test_renamed_file_shows_only_content_changes() {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        Command::new("git").args(["init"]).current_dir(repo_path).output().unwrap();
        Command::new("git").args(["config", "user.email", "test@test.com"]).current_dir(repo_path).output().unwrap();
        Command::new("git").args(["config", "user.name", "Test"]).current_dir(repo_path).output().unwrap();

        std::fs::write(repo_path.join("original.txt"), "line 1\nline 2\nline 3\nline 4\nline 5\n").unwrap();
        git_cmd(repo_path, &["add", "."]);
        git_cmd(repo_path, &["commit", "-m", "initial"]);
        git_cmd(repo_path, &["branch", "-M", "main"]);

        std::fs::remove_file(repo_path.join("original.txt")).unwrap();
        std::fs::write(repo_path.join("renamed.txt"), "line 1\nline 2 modified\nline 3\nline 4\nline 5\n").unwrap();

        let vcs = GitVcs::new(repo_path.to_path_buf()).unwrap();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let refresh = vcs.refresh(&cancel_flag).unwrap();

        assert_eq!(refresh.files.len(), 1, "Expected 1 file, got {}", refresh.files.len());

        let modified_lines: Vec<_> = refresh.lines.iter()
            .filter(|l| l.old_content.is_some() || l.change_source.is_some())
            .collect();

        assert!(!modified_lines.is_empty(), "Expected at least one modified line");

        let mod_line = modified_lines.iter()
            .find(|l| l.content.contains("line 2 modified"))
            .expect("Should have modification for line 2");

        assert_eq!(mod_line.old_content.as_deref(), Some("line 2"));
        assert_eq!(mod_line.change_source, Some(LineSource::Unstaged));
    }

    #[test]
    fn test_refresh_with_mixed_text_and_binary() {
        let temp_dir = create_test_repo();
        let repo_path = temp_dir.path();

        std::fs::write(repo_path.join("text.txt"), "text content\n").unwrap();
        std::fs::write(repo_path.join("binary.bin"), &[0u8, 1, 2, 255, 254, 253]).unwrap();
        git_cmd(repo_path, &["add", "binary.bin"]);

        let vcs = GitVcs::new(repo_path.to_path_buf()).unwrap();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let refresh = vcs.refresh(&cancel_flag).unwrap();

        assert_eq!(refresh.files.len(), 2);
        assert!(refresh.lines.iter().any(|l| l.content.contains("text content")));
        assert!(refresh.lines.iter().any(|l| l.content == "[binary file]"));

        let file_headers: Vec<_> = refresh.lines.iter()
            .filter(|l| l.source == LineSource::FileHeader)
            .collect();
        assert_eq!(file_headers.len(), 2);
        assert_eq!(refresh.metrics.file_count, 2);
    }

    #[test]
    fn test_refresh_computes_file_links_for_matching_files() {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        Command::new("git").args(["init"]).current_dir(repo_path).output().unwrap();
        Command::new("git").args(["config", "user.email", "test@test.com"]).current_dir(repo_path).output().unwrap();
        Command::new("git").args(["config", "user.name", "Test"]).current_dir(repo_path).output().unwrap();

        std::fs::write(repo_path.join("handler.go"), "package main\n").unwrap();
        std::fs::write(repo_path.join("handler_test.go"), "package main\n").unwrap();
        git_cmd(repo_path, &["add", "."]);
        git_cmd(repo_path, &["commit", "-m", "initial"]);
        git_cmd(repo_path, &["branch", "-M", "main"]);

        std::fs::write(repo_path.join("handler.go"), "package main\nfunc Handler() {}\n").unwrap();
        std::fs::write(repo_path.join("handler_test.go"), "package main\nfunc TestHandler() {}\n").unwrap();

        let vcs = GitVcs::new(repo_path.to_path_buf()).unwrap();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let refresh = vcs.refresh(&cancel_flag).unwrap();

        assert_eq!(refresh.files.len(), 2);
        assert_eq!(refresh.file_links.get("handler.go"), Some(&"handler_test.go".to_string()));
        assert_eq!(refresh.file_links.get("handler_test.go"), Some(&"handler.go".to_string()));
    }

    #[test]
    fn test_image_file_produces_image_marker() {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        Command::new("git").args(["init"]).current_dir(repo_path).output().unwrap();
        Command::new("git").args(["config", "user.email", "test@test.com"]).current_dir(repo_path).output().unwrap();
        Command::new("git").args(["config", "user.name", "Test"]).current_dir(repo_path).output().unwrap();

        let png_bytes: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE,
            0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54,
            0x08, 0xD7, 0x63, 0xF8, 0xFF, 0xFF, 0x3F, 0x00,
            0x05, 0xFE, 0x02, 0xFE, 0xA3, 0x56, 0x5A, 0x09,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44,
            0xAE, 0x42, 0x60, 0x82,
        ];

        std::fs::write(repo_path.join("image.png"), &png_bytes).unwrap();
        std::fs::write(repo_path.join("readme.txt"), "initial\n").unwrap();
        git_cmd(repo_path, &["add", "."]);
        git_cmd(repo_path, &["commit", "-m", "initial"]);
        git_cmd(repo_path, &["branch", "-M", "main"]);

        let modified_png: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE,
            0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54,
            0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00,
            0x02, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB4,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44,
            0xAE, 0x42, 0x60, 0x82,
        ];
        std::fs::write(repo_path.join("image.png"), &modified_png).unwrap();

        let vcs = GitVcs::new(repo_path.to_path_buf()).unwrap();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let refresh = vcs.refresh(&cancel_flag).unwrap();

        let image_marker = refresh.lines.iter().find(|line| line.is_image_marker());
        assert!(image_marker.is_some());
        let marker = image_marker.unwrap();
        assert_eq!(marker.file_path, Some("image.png".to_string()));
        assert_eq!(marker.content, "[image]");
    }

    #[test]
    fn test_image_and_binary_files_counted_in_metrics() {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        Command::new("git").args(["init"]).current_dir(repo_path).output().unwrap();
        Command::new("git").args(["config", "user.email", "test@test.com"]).current_dir(repo_path).output().unwrap();
        Command::new("git").args(["config", "user.name", "Test"]).current_dir(repo_path).output().unwrap();

        std::fs::write(repo_path.join("text.txt"), "initial\n").unwrap();
        git_cmd(repo_path, &["add", "."]);
        git_cmd(repo_path, &["commit", "-m", "initial"]);
        git_cmd(repo_path, &["branch", "-M", "main"]);

        std::fs::write(repo_path.join("text.txt"), "modified\n").unwrap();
        let png_bytes: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE,
            0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54,
            0x08, 0xD7, 0x63, 0xF8, 0xFF, 0xFF, 0x3F, 0x00,
            0x05, 0xFE, 0x02, 0xFE, 0xA3, 0x56, 0x5A, 0x09,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44,
            0xAE, 0x42, 0x60, 0x82,
        ];
        std::fs::write(repo_path.join("image.png"), &png_bytes).unwrap();

        let vcs = GitVcs::new(repo_path.to_path_buf()).unwrap();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let refresh = vcs.refresh(&cancel_flag).unwrap();

        assert_eq!(refresh.files.len(), 2);
        assert_eq!(refresh.metrics.file_count, 2);
    }
}
