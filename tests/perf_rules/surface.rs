//! This program's user-facing operations: the clap tree, the TUI key map,
//! and the live page's per-frame script and routes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};

use branchdiff::app::App;
use branchdiff::diff::{DiffLine, LineSource};
use branchdiff::file_events::VcsLockState;
use branchdiff::input::{AppAction, handle_event};
use branchdiff::message::{Message, Repaint, UpdateResult};
use branchdiff::update::{RefreshState, Timers, UpdateConfig, update};
use branchdiff::vcs::{
    ComparisonContext, RefreshResult, Vcs, VcsBackend, VcsEventType, VcsWatchPaths,
};

const FRAME_MARKERS: [&str; 9] = [
    "requestAnimationFrame",
    "requestVideoFrameCallback",
    "<canvas",
    "createElement('canvas')",
    "createElement(\"canvas\")",
    ".getContext(",
    "CADisplayLink",
    "TimelineView",
    "MTKView",
];

pub(super) fn operations() -> BTreeSet<String> {
    let commands = command_ids();
    assert!(!commands.is_empty(), "command surface must not be empty");
    let keys = key_operations();
    assert!(!keys.is_empty(), "keystroke surface must not be empty");
    let frames = frame_and_route_ids();
    assert!(frames.iter().any(|id| id.starts_with("frame:")), "frame surface must not be empty");
    let mut ops = commands;
    ops.extend(keys);
    ops.extend(frames);
    ops
}

/// The manifest committed on `main@origin` or `main`, if that revision has one.
pub(super) fn committed_baseline() -> Option<String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    committed_baseline_in(&root)
}

pub(super) fn committed_baseline_in(root: &Path) -> Option<String> {
    if root.join(".jj").exists() {
        committed_baseline_jj(root)
    } else {
        committed_baseline_git(root)
    }
}

fn committed_baseline_jj(root: &Path) -> Option<String> {
    let repo = root.to_str().expect("repo path");
    for rev in ["main@origin", "main"] {
        let resolved = Command::new("jj")
            .args([
                "--ignore-working-copy", "--repository", repo, "log", "--no-graph", "-r", rev,
                "-T", "commit_id",
            ])
            .output()
            .unwrap_or_else(|err| panic!("jj log failed to start: {err}"));
        if !resolved.status.success() || String::from_utf8_lossy(&resolved.stdout).trim().is_empty()
        {
            continue;
        }
        let listed = Command::new("jj")
            .args([
                "--ignore-working-copy", "--repository", repo, "file", "list", "-r", rev,
                "tests/perf.toml",
            ])
            .output()
            .unwrap_or_else(|err| panic!("jj file list failed to start: {err}"));
        assert!(
            listed.status.success(),
            "jj file list -r {rev} tests/perf.toml failed: {}",
            String::from_utf8_lossy(&listed.stderr)
        );
        if String::from_utf8_lossy(&listed.stdout).trim().is_empty() {
            return None;
        }
        let shown = Command::new("jj")
            .args([
                "--ignore-working-copy", "--repository", repo, "file", "show", "-r", rev,
                "tests/perf.toml",
            ])
            .output()
            .unwrap_or_else(|err| panic!("jj file show failed to start: {err}"));
        assert!(
            shown.status.success(),
            "jj file show -r {rev} tests/perf.toml failed: {}",
            String::from_utf8_lossy(&shown.stderr)
        );
        return Some(String::from_utf8(shown.stdout).expect("baseline is utf-8"));
    }
    panic!("neither main@origin nor main resolved in jj repository {}", root.display());
}

fn committed_baseline_git(root: &Path) -> Option<String> {
    let repo = root.to_str().expect("repo path");
    for rev in ["origin/main", "main"] {
        let verify_arg = format!("{rev}^{{commit}}");
        let resolved = Command::new("git")
            .args(["-C", repo, "rev-parse", "--verify", "--quiet", &verify_arg])
            .output()
            .unwrap_or_else(|err| panic!("git rev-parse failed to start: {err}"));
        if !resolved.status.success() {
            continue;
        }
        let listed = Command::new("git")
            .args(["-C", repo, "ls-tree", "--name-only", rev, "--", "tests/perf.toml"])
            .output()
            .unwrap_or_else(|err| panic!("git ls-tree failed to start: {err}"));
        assert!(
            listed.status.success(),
            "git ls-tree {rev} tests/perf.toml failed: {}",
            String::from_utf8_lossy(&listed.stderr)
        );
        if String::from_utf8_lossy(&listed.stdout).trim().is_empty() {
            return None;
        }
        let spec = format!("{rev}:tests/perf.toml");
        let shown = Command::new("git")
            .args(["-C", repo, "show", &spec])
            .output()
            .unwrap_or_else(|err| panic!("git show failed to start: {err}"));
        assert!(
            shown.status.success(),
            "git show {spec} failed: {}",
            String::from_utf8_lossy(&shown.stderr)
        );
        return Some(String::from_utf8(shown.stdout).expect("baseline is utf-8"));
    }
    panic!("neither origin/main nor main resolved in git repository {}", root.display());
}

pub(super) fn action_name(action: AppAction) -> Option<&'static str> {
    Some(match action {
        AppAction::Quit => "key:quit",
        AppAction::ScrollUp(_) => "key:scroll-up",
        AppAction::ScrollDown(_) => "key:scroll-down",
        AppAction::PageUp => "key:page-up",
        AppAction::PageDown => "key:page-down",
        AppAction::GoToTop => "key:go-to-top",
        AppAction::GoToBottom => "key:go-to-bottom",
        AppAction::NextFile => "key:next-file",
        AppAction::PrevFile => "key:prev-file",
        AppAction::ToggleHelp => "key:toggle-help",
        AppAction::CycleViewMode => "key:cycle-view-mode",
        AppAction::StartSelection(_, _) => "key:start-selection",
        AppAction::UpdateSelection(_, _) => "key:update-selection",
        AppAction::EndSelection => "key:end-selection",
        AppAction::CopyPath => "key:copy-path",
        AppAction::CopyDiff => "key:copy-diff",
        AppAction::CopyPatch => "key:copy-patch",
        AppAction::CopyOrQuit => "key:copy-or-quit",
        AppAction::OpenSearch => "key:open-search",
        AppAction::ToggleDiffBase => "key:toggle-diff-base",
        AppAction::ToggleReviewed => "key:toggle-reviewed",
        AppAction::ToggleAllReviewed => "key:toggle-all-reviewed",
        AppAction::OpenEditor => "key:open-editor",
        AppAction::OpenRepo => "key:open-repo",
        AppAction::RunRecovery => "key:run-recovery",
        AppAction::Resize => "key:resize",
        AppAction::ForceRepaint => "key:force-repaint",
        AppAction::None => return None,
    })
}

/// Run one non-search action through the reducer. Clipboard arms stay with the
/// caller so unit tests never touch the system clipboard.
pub(super) fn apply_input(app: &mut App, action: AppAction) {
    let mut refresh = RefreshState::Idle;
    let mut lock = VcsLockState::default();
    let mut timers = Timers::default();
    let config = UpdateConfig::default();
    let vcs = IdleVcs;
    let _ =
        update(Message::Input(action), app, &mut refresh, &mut lock, &mut timers, &config, &vcs);
}

/// Apply one search-mode event and name the effect. An effect the buckets
/// do not recognise is `key:search-unclassified`, which has no budget.
pub(super) fn search_effect(app: &mut App, event: Event) -> Option<&'static str> {
    let before = snap(app);
    let result = apply_search(app, event);
    classify(&before, &snap(app), &result)
}

fn command_ids() -> BTreeSet<String> {
    use clap::CommandFactory;
    let mut cmd = branchdiff::cli::Cli::command();
    // Help is added while the command is built, not in the derive output.
    cmd.build();
    let mut ops = BTreeSet::new();
    walk_command(&cmd, &mut ops);
    ops
}

fn walk_command(cmd: &clap::Command, ops: &mut BTreeSet<String>) {
    ops.insert(format!("command:{}", normalize(cmd.get_name())));
    for arg in cmd.get_arguments() {
        if arg.is_positional() {
            continue;
        }
        let name = normalize(arg.get_long().unwrap_or(arg.get_id().as_str()));
        if name == "html" {
            push_html_pages(arg, ops);
            continue;
        }
        ops.insert(format!("command:{name}"));
    }
    for sub in cmd.get_subcommands() {
        walk_command(sub, ops);
    }
}

fn push_html_pages(arg: &clap::Arg, ops: &mut BTreeSet<String>) {
    let mut count = 0;
    for value in arg.get_possible_values() {
        count += 1;
        ops.insert(format!("page:html-{}", normalize(value.get_name())));
    }
    if count == 0 {
        ops.insert("page:html-unspecified".to_string());
    }
}

fn normalize(name: &str) -> String {
    name.replace('_', "-")
}

fn key_operations() -> BTreeSet<String> {
    let lines = probe_lines();
    let mut ops = BTreeSet::new();
    for event in probe_events() {
        if let Some(id) = action_name(handle_event(event.clone())) {
            ops.insert(id.to_string());
        }
        let mut app = probe_app(&lines);
        if let Some(id) = search_effect(&mut app, event) {
            ops.insert(id.to_string());
        }
    }
    ops
}

fn frame_and_route_ids() -> BTreeSet<String> {
    let mut ops = BTreeSet::new();
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    walk_frames_and_routes(&src, &mut ops);
    ops
}

fn walk_frames_and_routes(dir: &Path, ops: &mut BTreeSet<String>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("read_dir {} failed: {err}", dir.display()));
    for entry in entries {
        let entry =
            entry.unwrap_or_else(|err| panic!("read_dir entry in {} failed: {err}", dir.display()));
        let path = entry.path();
        if path.is_dir() {
            walk_frames_and_routes(&path, ops);
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "rs") {
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("read {} failed: {err}", path.display()));
            let prod = strip_inline_test_mod(&raw);
            if FRAME_MARKERS.iter().any(|marker| prod.contains(marker)) {
                let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("page");
                ops.insert(format!("frame:{}", normalize(stem)));
            }
            push_fetch_routes(prod, ops);
        }
    }
}

fn strip_inline_test_mod(text: &str) -> &str {
    text.find("#[cfg(test)]").map_or(text, |idx| &text[..idx])
}

fn push_fetch_routes(text: &str, ops: &mut BTreeSet<String>) {
    for prefix in ["fetch('", "fetch(\""] {
        let quote = prefix.chars().last().expect("quote");
        let mut rest = text;
        while let Some(pos) = rest.find(prefix) {
            let after = &rest[pos + prefix.len()..];
            if let Some(end) = after.find(quote) {
                let route = &after[..end];
                if !route.is_empty() {
                    ops.insert(format!("page:route-{}", normalize(route.trim_start_matches('/'))));
                }
            }
            rest = after;
        }
    }
}

fn probe_lines() -> Vec<DiffLine> {
    let mut lines = vec![DiffLine::file_header("probe.txt")];
    for row in 0..210 {
        let text = if matches!(row, 10 | 80 | 150) {
            format!("value {row}")
        } else {
            format!("row {row}")
        };
        lines.push(DiffLine::new(LineSource::Base, text, ' ', Some(row + 1)));
    }
    lines
}

fn probe_app(lines: &[DiffLine]) -> App {
    let mut app = App::new_for_bench(lines.to_vec());
    app.set_viewport_height(50);
    app.open_search();
    app.search_insert_char('v');
    if let Some(search) = app.search.as_mut() {
        search.current = 1;
    }
    app.view.scroll_offset = 100;
    app.view.sub_row = 0;
    app
}

fn probe_events() -> Vec<Event> {
    let mut events = Vec::new();
    push_keys(&mut events);
    push_pointer(&mut events);
    events.push(Event::Resize(120, 50));
    events.push(Event::FocusGained);
    events.push(Event::FocusLost);
    events
}

fn push_keys(events: &mut Vec<Event>) {
    let modifiers = [
        KeyModifiers::NONE,
        KeyModifiers::SHIFT,
        KeyModifiers::CONTROL,
        KeyModifiers::ALT,
        KeyModifiers::SHIFT | KeyModifiers::CONTROL,
    ];
    for byte in 0u8..=127 {
        for modifiers in modifiers {
            events.push(key(KeyCode::Char(byte as char), modifiers));
        }
    }
    let mut specials = vec![
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Enter,
        KeyCode::Esc,
        KeyCode::Backspace,
        KeyCode::Tab,
        KeyCode::Delete,
        KeyCode::Insert,
    ];
    specials.extend((1..=12).map(KeyCode::F));
    for code in specials {
        for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT, KeyModifiers::CONTROL] {
            events.push(key(code, modifiers));
        }
    }
}

fn push_pointer(events: &mut Vec<Event>) {
    for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
        events.push(mouse(MouseEventKind::Down(button)));
        events.push(mouse(MouseEventKind::Drag(button)));
        events.push(mouse(MouseEventKind::Up(button)));
    }
    for kind in [
        MouseEventKind::Moved,
        MouseEventKind::ScrollUp,
        MouseEventKind::ScrollDown,
        MouseEventKind::ScrollLeft,
        MouseEventKind::ScrollRight,
    ] {
        events.push(mouse(kind));
    }
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent { code, modifiers, kind: KeyEventKind::Press, state: KeyEventState::NONE })
}

fn mouse(kind: MouseEventKind) -> Event {
    Event::Mouse(MouseEvent { kind, column: 1, row: 1, modifiers: KeyModifiers::NONE })
}

struct Snap {
    query: Option<String>,
    current: usize,
    scroll: usize,
}

fn snap(app: &App) -> Snap {
    Snap {
        query: app.search.as_ref().map(|search| search.query.clone()),
        current: app.search.as_ref().map(|search| search.current).unwrap_or(0),
        scroll: app.view.scroll_offset,
    }
}

fn classify(before: &Snap, after: &Snap, result: &UpdateResult) -> Option<&'static str> {
    match (&before.query, &after.query) {
        (Some(_), None) => return Some("key:search-close"),
        (Some(old), Some(new)) if new.chars().count() > old.chars().count() => {
            return Some("key:search-type");
        }
        (Some(old), Some(new)) if new.chars().count() < old.chars().count() => {
            return Some("key:search-backspace");
        }
        _ => {}
    }
    if before.query.is_some() && after.current != before.current {
        return Some(if after.current > before.current {
            "key:search-next"
        } else {
            "key:search-prev"
        });
    }
    let delta = after.scroll as i64 - before.scroll as i64;
    if (-5..=-1).contains(&delta) {
        return Some("key:scroll-up");
    }
    if (1..=5).contains(&delta) {
        return Some("key:scroll-down");
    }
    if delta <= -6 {
        return Some("key:page-up");
    }
    if delta >= 6 {
        return Some("key:page-down");
    }
    let unchanged = before.query == after.query && before.current == after.current && delta == 0;
    if unchanged && result.repaint == Repaint::Full {
        return Some("key:force-repaint");
    }
    if unchanged && !result.needs_redraw {
        return None;
    }
    Some("key:search-unclassified")
}

fn apply_search(app: &mut App, event: Event) -> UpdateResult {
    let mut refresh = RefreshState::Idle;
    let mut lock = VcsLockState::default();
    let mut timers = Timers::default();
    let config = UpdateConfig::default();
    let vcs = IdleVcs;
    update(Message::SearchInput(event), app, &mut refresh, &mut lock, &mut timers, &config, &vcs)
}

struct IdleVcs;

impl Vcs for IdleVcs {
    fn repo_path(&self) -> &Path {
        Path::new("/bench")
    }

    fn comparison_context(&self) -> anyhow::Result<ComparisonContext> {
        unimplemented!("a keystroke must not query the vcs")
    }

    fn refresh(&self, _cancel: &Arc<AtomicBool>) -> anyhow::Result<RefreshResult> {
        unimplemented!("a keystroke must not refresh")
    }

    fn single_file_diff(&self, _file_path: &str) -> Option<branchdiff::diff::FileDiff> {
        None
    }

    fn base_identifier(&self) -> anyhow::Result<String> {
        unimplemented!("a keystroke must not query the vcs")
    }

    fn base_file_bytes(&self, _file_path: &str) -> anyhow::Result<Option<Vec<u8>>> {
        unimplemented!("a keystroke must not read file bytes")
    }

    fn working_file_bytes(&self, _file_path: &str) -> anyhow::Result<Option<Vec<u8>>> {
        unimplemented!("a keystroke must not read file bytes")
    }

    fn fetch(&self) -> anyhow::Result<()> {
        unimplemented!("a keystroke must not fetch")
    }

    fn has_conflicts(&self) -> anyhow::Result<bool> {
        Ok(false)
    }

    fn is_locked(&self) -> bool {
        false
    }

    fn watch_paths(&self) -> VcsWatchPaths {
        VcsWatchPaths { files: Vec::new(), recursive_dirs: Vec::new() }
    }

    fn classify_event(&self, _path: &Path) -> VcsEventType {
        VcsEventType::Internal
    }

    fn backend(&self) -> VcsBackend {
        VcsBackend::Git
    }

    fn current_revision_id(&self) -> anyhow::Result<String> {
        Ok("bench".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn run_cmd(dir: &Path, program: &str, args: &[&str]) {
        let out = Command::new(program)
            .args(args)
            .current_dir(dir)
            .env("JJ_USER", "Test")
            .env("JJ_EMAIL", "test@example.com")
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("spawn command");
        assert!(
            out.status.success(),
            "{program} {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    #[should_panic(expected = "resolved")]
    fn committed_baseline_panics_when_revision_does_not_resolve() {
        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "jj", &["git", "init", "--colocate"]);
        let _ = committed_baseline_in(tmp.path());
    }

    #[test]
    fn committed_baseline_returns_none_when_revision_resolves_and_lacks_file_and_reads_git_checkout()
     {
        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "git", &["init", "-q", "-b", "main"]);
        fs::write(tmp.path().join("README.md"), "hello\n").expect("write readme");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "init"]);
        assert_eq!(committed_baseline_in(tmp.path()), None);

        fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        fs::write(tmp.path().join("tests/perf.toml"), "# baseline\n").expect("write perf.toml");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "add perf"]);
        assert_eq!(committed_baseline_in(tmp.path()).as_deref(), Some("# baseline\n"));
    }

    #[test]
    #[should_panic(expected = "read_dir")]
    fn walk_frames_and_routes_panics_when_read_dir_fails() {
        let mut ops = BTreeSet::new();
        walk_frames_and_routes(
            Path::new("/nonexistent/branchdiff-perf-surface-missing-dir"),
            &mut ops,
        );
    }
}
