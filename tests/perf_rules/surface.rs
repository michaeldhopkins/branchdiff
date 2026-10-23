//! This program's user-facing operations: the clap tree, the TUI key map,
//! and the live page's per-frame script.

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

pub(super) fn operations() -> BTreeSet<String> {
    let mut ops = command_ids();
    ops.extend(key_operations());
    ops.extend(frame_ids());
    ops
}

/// The manifest committed on `main`, if that revision has one.
pub(super) fn committed_baseline() -> Option<String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = root.to_str().expect("repo path");
    let output = Command::new("jj")
        .args(["--repository", repo, "file", "show", "-r", "main", "tests/perf.toml"])
        .output()
        .unwrap_or_else(|err| panic!("jj file show failed to start: {err}"));
    if output.status.success() {
        return Some(String::from_utf8(output.stdout).expect("baseline is utf-8"));
    }
    let err = String::from_utf8_lossy(&output.stderr);
    if err.contains("No such path") || err.contains("does not exist") || err.contains("not found") {
        return None;
    }
    panic!("jj file show -r main tests/perf.toml failed: {err}");
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
        AppAction::Resize | AppAction::ForceRepaint => "key:force-repaint",
        AppAction::None => return None,
    })
}

/// Run one non-search action through the reducer. Clipboard and selection-end
/// stay with the caller: those arms touch the clipboard.
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

fn frame_ids() -> BTreeSet<String> {
    let mut ops = BTreeSet::new();
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    walk_frames(&src, &mut ops);
    ops
}

fn walk_frames(dir: &Path, ops: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_frames(&path, ops);
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "rs")
            && let Ok(text) = std::fs::read_to_string(&path)
            && text.contains("requestAnimationFrame")
        {
            let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("page");
            ops.insert(format!("frame:{}", normalize(stem)));
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
