//! Integration tests for branchdiff TUI.
//!
//! These tests launch the actual branchdiff binary in a PTY and verify behavior.
//! Run with: cargo test --test integration -- --test-threads=1

mod harness;

use harness::{TestRepo, TuiSession};

/// Verify branchdiff shows a modified file in the diff view.
#[test]
fn test_shows_modified_file() {
    let repo = TestRepo::new();
    repo.add_file("src/main.rs", "fn main() {}");
    repo.commit("add main.rs");
    repo.create_branch("feature");
    repo.modify_file("src/main.rs", "fn main() {\n    println!(\"hello\");\n}");

    let mut session = TuiSession::launch(repo.path());

    session.assert_contains("src/main.rs");
    session.assert_contains("println");
    session.assert_status_bar_contains("1 file");
}

/// Verify `e` opens the current file in `$EDITOR` (a foreground/terminal editor)
/// and that the TUI is restored afterward. Exercises the SuspendGuard round-trip.
#[test]
#[cfg(unix)]
fn test_e_opens_current_file_in_editor() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    let repo = TestRepo::new();
    repo.add_file("src/main.rs", "fn main() {}");
    repo.commit("add main.rs");
    repo.create_branch("feature");
    repo.modify_file("src/main.rs", "fn main() {\n    println!(\"hi\");\n}");

    // Mock editor lives outside the repo so the file watcher ignores it; it just
    // records the path it was handed. Its name is unknown to the preset table,
    // so it is classified Foreground (suspend + wait + restore).
    let mock_dir = tempfile::TempDir::new().unwrap();
    let sentinel = mock_dir.path().join("opened.txt");
    let script = mock_dir.path().join("mock_editor.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nprintf '%s' \"$1\" > '{}'\n", sentinel.display()),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();

    let mut session =
        TuiSession::launch_with_env(repo.path(), &[("EDITOR", script.to_str().unwrap())]);
    session.assert_contains("src/main.rs");

    session.press("e");

    let deadline = Instant::now() + Duration::from_secs(5);
    let recorded = loop {
        match std::fs::read_to_string(&sentinel) {
            Ok(s) if !s.is_empty() => break s,
            _ => {
                assert!(Instant::now() < deadline, "editor was never invoked");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    assert!(
        recorded.ends_with("src/main.rs"),
        "editor opened the wrong file: {recorded}"
    );

    session.assert_contains("src/main.rs");
}

/// Verify branchdiff starts in Context view mode (not Full).
/// This test would have caught the ViewMode::Full regression.
#[test]
fn test_starts_in_context_mode() {
    let repo = TestRepo::new();
    repo.add_file("test.rs", "line1\nline2\nline3");
    repo.commit("add test.rs");
    repo.create_branch("feature");
    repo.modify_file("test.rs", "line1\nMODIFIED\nline3");

    let mut session = TuiSession::launch(repo.path());

    session.assert_status_bar_contains("[context]");
}

/// Verify view mode cycles through Context -> Changes -> Full.
#[test]
fn test_view_mode_cycling() {
    let repo = TestRepo::new();
    repo.add_file("test.rs", "content");
    repo.commit("add test.rs");
    repo.create_branch("feature");
    repo.modify_file("test.rs", "modified");

    let mut session = TuiSession::launch(repo.path());

    // Should start in Context mode
    session.assert_status_bar_contains("[context]");

    // Press 'c' to cycle to ChangesOnly
    session.press("c");
    session.wait_for_text("[changed lines only]");
    session.assert_status_bar_contains("[changed lines only]");

    // Press 'c' again to cycle to Full
    session.press("c");
    session
        .wait_for(
            |contents| !contents.contains("[changed lines only]") && !contents.contains("[context]"),
            std::time::Duration::from_secs(5),
        )
        .expect("timeout waiting for Full mode");

    // Press 'c' again to cycle back to Context
    session.press("c");
    session.wait_for_text("[context]");
    session.assert_status_bar_contains("[context]");
}

/// Verify 'q' quits the application.
#[test]
fn test_quit_with_q() {
    let repo = TestRepo::new();
    repo.add_file("test.rs", "x");
    repo.commit("add test.rs");
    repo.create_branch("feature");
    repo.modify_file("test.rs", "y");

    let mut session = TuiSession::launch(repo.path());

    session.press("q");
    // The process should exit - if it doesn't, the harness will timeout
}
