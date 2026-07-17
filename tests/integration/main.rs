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

    // Blank VISUAL so an ambient $VISUAL on the dev's machine doesn't shadow our
    // mock $EDITOR (resolve_editor prefers VISUAL and skips empty values).
    let mut session = TuiSession::launch_with_env(
        repo.path(),
        &[("EDITOR", script.to_str().unwrap()), ("VISUAL", "")],
    );
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

/// Verify `E` hands the *repo root* to a directory-capable editor. The mock is
/// named `vim` so it's recognized as dir-capable and classified Foreground.
#[test]
#[cfg(unix)]
fn test_shift_e_opens_repo_in_editor() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    let repo = TestRepo::new();
    repo.add_file("src/main.rs", "fn main() {}");
    repo.commit("add main.rs");
    repo.create_branch("feature");
    repo.modify_file("src/main.rs", "fn main() {\n    println!(\"hi\");\n}");

    let mock_dir = tempfile::TempDir::new().unwrap();
    let sentinel = mock_dir.path().join("opened.txt");
    // Named `vim` so `opens_directory` accepts it; it records the path handed in.
    let script = mock_dir.path().join("vim");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nprintf '%s' \"$1\" > '{}'\n", sentinel.display()),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();

    // Blank VISUAL so an ambient $VISUAL doesn't shadow our mock $EDITOR.
    let mut session = TuiSession::launch_with_env(
        repo.path(),
        &[("EDITOR", script.to_str().unwrap()), ("VISUAL", "")],
    );
    session.assert_contains("src/main.rs");

    session.press("E");

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
    // The editor received a directory — the repo root, which holds src/main.rs.
    assert!(
        std::path::Path::new(&recorded).join("src/main.rs").exists(),
        "editor opened the wrong directory: {recorded}"
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

/// Verify view mode cycles through Context -> Full -> Context (git).
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

    // Press 'c' to cycle to Full
    session.press("c");
    session.wait_for_text("[all lines]");
    session.assert_status_bar_contains("[all lines]");

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

/// Reproduces the "came back to my desk and the screen is half painted" bug
/// deterministically, in milliseconds, and pins each repaint trigger.
///
/// The bug needs no sleeping or real display: it only needs the terminal's
/// screen and ratatui's in-memory copy of it to disagree. `simulate_terminal_wiped`
/// creates exactly that disagreement without the app being told, which is what
/// display sleep / a terminal repaint / a reattach do in the wild.
///
/// The middle of this test is the important part: after the wipe, an ordinary
/// redraw does NOT repair the screen. That is the whole bug — branchdiff was
/// relying on incidental redraws to heal it, and they can't.
#[test]
#[cfg(unix)]
fn test_screen_wiped_behind_our_back_is_repaired_by_repaint_triggers() {
    let repo = TestRepo::new();
    repo.add_file("src/main.rs", "fn main() {}");
    repo.commit("add main.rs");
    repo.create_branch("feature");
    repo.modify_file("src/main.rs", "fn main() {\n    println!(\"hi\");\n}");

    let mut session = TuiSession::launch(repo.path());
    session.assert_contains("src/main.rs");

    // The terminal loses its contents; branchdiff is never told.
    session.simulate_terminal_wiped();
    assert!(
        !session.text().contains("src/main.rs"),
        "precondition: the wipe must actually clear the screen"
    );

    // An ordinary redraw cannot repair it: ratatui diffs against a previous
    // frame that no longer matches reality, so the stale cells are never
    // rewritten. This is the bug, and it is why waiting doesn't help.
    session.press("j");
    assert!(
        !session.text().contains("src/main.rs"),
        "BUG REPRODUCED CHECK: a normal redraw should not have repaired the screen — \
         if this now passes, the diff-render assumption changed and this test is stale"
    );

    // FocusGained (CSI I) — what a terminal sends when you switch back to it.
    session.send_raw(b"\x1b[I");
    assert!(
        session.text().contains("src/main.rs"),
        "FocusGained must force a full repaint; screen was:\n{}",
        session.text()
    );

    // And Ctrl+L, the escape hatch for terminals that never report focus.
    session.simulate_terminal_wiped();
    assert!(!session.text().contains("src/main.rs"), "precondition: wiped again");
    session.send_raw(b"\x0c");
    assert!(
        session.text().contains("src/main.rs"),
        "Ctrl+L must force a full repaint; screen was:\n{}",
        session.text()
    );
}

/// branchdiff must actually *ask* the terminal to report focus.
///
/// Handling `FocusGained` is useless if we never enable focus reporting: no
/// terminal sends `CSI I` unbidden. This asserts the DECSET 1004 request goes
/// out on startup, which is the half of the mechanism that can't be verified by
/// injecting an event into the harness.
///
/// Terminals that don't implement 1004 parse and discard it, so this is inert
/// where unsupported rather than junk on screen.
#[test]
#[cfg(unix)]
fn test_enables_focus_reporting_on_startup() {
    let repo = TestRepo::new();
    repo.add_file("src/main.rs", "fn main() {}");
    repo.commit("add main.rs");
    repo.create_branch("feature");
    repo.modify_file("src/main.rs", "fn main() {\n    println!(\"hi\");\n}");

    let mut session = TuiSession::launch(repo.path());
    session.assert_contains("src/main.rs");

    assert!(
        session.emitted(b"\x1b[?1004h"),
        "branchdiff must enable focus reporting (DECSET 1004) or no terminal will ever \
         send FocusGained and the repaint-on-return fix is dead code"
    );
}
