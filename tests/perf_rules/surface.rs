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

#[path = "../support/frame_scan.rs"]
mod frame_scan;

use frame_scan::{per_frame_work, production};

pub(super) use super::guard::{committed_baseline, committed_baseline_benches, recorded_baseline};

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

const ALL_APP_ACTIONS: &[AppAction] = &[
    AppAction::Quit,
    AppAction::ScrollUp(1),
    AppAction::ScrollDown(1),
    AppAction::PageUp,
    AppAction::PageDown,
    AppAction::GoToTop,
    AppAction::GoToBottom,
    AppAction::NextFile,
    AppAction::PrevFile,
    AppAction::ToggleHelp,
    AppAction::CycleViewMode,
    AppAction::StartSelection(0, 0),
    AppAction::UpdateSelection(0, 0),
    AppAction::EndSelection,
    AppAction::CopyPath,
    AppAction::CopyDiff,
    AppAction::CopyPatch,
    AppAction::CopyOrQuit,
    AppAction::OpenSearch,
    AppAction::ToggleDiffBase,
    AppAction::ToggleReviewed,
    AppAction::ToggleAllReviewed,
    AppAction::OpenEditor,
    AppAction::OpenRepo,
    AppAction::RunRecovery,
    AppAction::Resize,
    AppAction::ForceRepaint,
    AppAction::None,
];

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
pub(super) fn apply_input(app: &mut App, action: AppAction) -> UpdateResult {
    let mut refresh = RefreshState::Idle;
    let mut lock = VcsLockState::default();
    let mut timers = Timers::default();
    let config = UpdateConfig::default();
    let vcs = IdleVcs;
    update(Message::Input(action), app, &mut refresh, &mut lock, &mut timers, &config, &vcs)
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
    let mut ops: BTreeSet<String> =
        ALL_APP_ACTIONS.iter().copied().filter_map(action_name).map(str::to_string).collect();
    let lines = probe_lines();
    for event in probe_events() {
        if let Some(id) = action_name(handle_event(event.clone())) {
            assert!(ops.contains(id), "ALL_APP_ACTIONS is missing variant for {id}");
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
            scan_source_file(&path, &raw, ops);
        }
    }
}

fn scan_source_file(path: &Path, raw: &str, ops: &mut BTreeSet<String>) {
    if per_frame_work(raw).is_some() {
        ops.insert(frame_id(path));
    }
    let prod = production(raw);
    push_fetch_routes(&prod, ops);
}

fn frame_id(path: &Path) -> String {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let rel = path.strip_prefix(&src).unwrap_or(path).with_extension("");
    let parts: Vec<String> =
        rel.components().filter_map(|comp| comp.as_os_str().to_str()).map(normalize).collect();
    let slug = if parts.is_empty() { "page".to_string() } else { parts.join("/") };
    format!("frame:{slug}")
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

    fn git_head(dir: &Path) -> String {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .expect("git rev-parse HEAD");
        assert!(out.status.success());
        String::from_utf8(out.stdout).expect("utf8").trim().to_string()
    }

    #[test]
    fn surface_includes_resize_keystroke_and_path_based_frame_ids() {
        let ops = operations();
        assert!(ops.contains("key:resize"), "probe_events must surface key:resize");
        assert!(ops.contains("frame:html-page"), "src/html_page.rs must map to frame:html-page");

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut frame_ops = BTreeSet::new();
        let snippet = "const RAF: &str = \"requestAnimationFrame(step)\";";
        scan_source_file(&root.join("src/ui/mod.rs"), snippet, &mut frame_ops);
        scan_source_file(&root.join("src/vcs/mod.rs"), snippet, &mut frame_ops);
        assert!(frame_ops.contains("frame:ui/mod"), "{frame_ops:?}");
        assert!(frame_ops.contains("frame:vcs/mod"), "{frame_ops:?}");
        assert_eq!(frame_ops.len(), 2, "two mod.rs files must not collide: {frame_ops:?}");
    }

    fn baseline_child(repo_os: std::ffi::OsString) {
        let repo = PathBuf::from(repo_os);
        let mode = std::env::var("BRANCHDIFF_TEST_BASELINE_MODE").expect("mode");
        let baseline = committed_baseline();
        match mode.as_str() {
            "tighter" => {
                let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
                let manifest =
                    fs::read_to_string(repo.join("tests/perf.toml")).expect("read manifest");
                let surface = operations();
                let mut tests = BTreeSet::new();
                super::super::guard::collect_test_names(&root.join("tests"), &mut tests);
                super::super::guard::collect_test_names(&root.join("src"), &mut tests);
                let bench_text = fs::read_to_string(root.join("benches/baselines.toml"))
                    .expect("read baselines");
                let benches = super::super::guard::must_recorded_benches(&bench_text);
                let baseline = baseline.expect("baseline");
                let errors = super::super::guard::check(&super::super::guard::Input {
                    manifest: &manifest,
                    surface: &surface,
                    tests: &tests,
                    baseline: baseline.as_deref(),
                    benches: &benches,
                });
                assert!(errors.is_empty(), "manifest looser than baseline:\n{}", errors.join("\n"));
            }
            "new_zero" => {
                assert_eq!(baseline, Ok(None), "all-zero SHA on a new manifest must return None");
            }
            _ => unreachable!("unknown mode {mode}"),
        }
    }

    fn spawn_baseline_child(repo: &Path, mode: &str, rev: &str) -> std::process::Output {
        Command::new(std::env::current_exe().expect("current_exe"))
            .args([
                "--exact",
                "surface::tests::perf_baseline_rev_env_var_tighter_commit_fails_and_all_zero_checks_manifest_history",
                "--nocapture",
            ])
            .env("PERF_BASELINE_REPO", repo)
            .env("BRANCHDIFF_TEST_BASELINE_MODE", mode)
            .env("PERF_BASELINE_REV", rev)
            .output()
            .expect("spawn baseline child")
    }

    #[test]
    fn perf_baseline_rev_env_var_tighter_commit_fails_and_all_zero_checks_manifest_history() {
        if let Some(repo_os) = std::env::var_os("PERF_BASELINE_REPO") {
            baseline_child(repo_os);
            return;
        }

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let live_manifest =
            fs::read_to_string(root.join("tests/perf.toml")).expect("read live manifest");
        let tighter_manifest = live_manifest.replacen("processes = 61", "processes = 1", 1);

        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "git", &["init", "-q", "-b", "main"]);
        fs::write(tmp.path().join("README.md"), "init\n").expect("write readme");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "init without manifest"]);

        fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        fs::write(tmp.path().join("tests/perf.toml"), &tighter_manifest)
            .expect("write tighter perf.toml");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "add tighter perf.toml"]);
        let tighter_sha = git_head(tmp.path());

        let zero = "0000000000000000000000000000000000000000";
        let new_zero = spawn_baseline_child(tmp.path(), "new_zero", zero);
        assert!(
            new_zero.status.success(),
            "all-zero SHA when manifest is new must pass:\n{}",
            String::from_utf8_lossy(&new_zero.stderr)
        );

        fs::write(tmp.path().join("tests/perf.toml"), &live_manifest)
            .expect("write live perf.toml");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "loosen perf.toml"]);

        let tighter_out = spawn_baseline_child(tmp.path(), "tighter", &tighter_sha);
        assert!(!tighter_out.status.success(), "tighter PERF_BASELINE_REV must fail the child");
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&tighter_out.stdout),
            String::from_utf8_lossy(&tighter_out.stderr)
        );
        assert!(
            combined.contains("processes 61 is looser than the baseline 1"),
            "expected looser-than-baseline error, got:\n{combined}"
        );

        let existing_zero = spawn_baseline_child(tmp.path(), "new_zero", zero);
        assert!(
            !existing_zero.status.success(),
            "all-zero SHA when manifest already existed in parent must fail loudly"
        );
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

    #[test]
    fn raf_or_short_timer_added_to_src_html_rs_fails_the_guard() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let manifest = fs::read_to_string(root.join("tests/perf.toml")).expect("read perf.toml");
        let html_rs = fs::read_to_string(root.join("src/html.rs")).expect("read src/html.rs");
        let mut tests = BTreeSet::new();
        super::super::guard::collect_test_names(&root.join("tests"), &mut tests);
        super::super::guard::collect_test_names(&root.join("src"), &mut tests);
        let bench_text =
            fs::read_to_string(root.join("benches/baselines.toml")).expect("read baselines");
        let benches = super::super::guard::must_recorded_benches(&bench_text);

        for snippet in [
            "const RAF: &str = \"requestAnimationFrame(step)\";",
            "const TIMER: &str = \"setInterval(step, 16)\";",
        ] {
            let mut surface = operations();
            let modified = format!("{html_rs}\n{snippet}\n");
            scan_source_file(&root.join("src/html.rs"), &modified, &mut surface);
            let errors = super::super::guard::check(&super::super::guard::Input {
                manifest: &manifest,
                surface: &surface,
                tests: &tests,
                baseline: None,
                benches: &benches,
            });
            assert!(
                errors.iter().any(|err| err.contains("frame:html") && err.contains("no budget")),
                "snippet `{snippet}` in src/html.rs must fail the guard: {errors:?}"
            );
        }
    }
}
