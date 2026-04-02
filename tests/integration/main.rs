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
