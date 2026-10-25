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

/// The manifest committed at `PERF_BASELINE_REV` (when set in CI) or on
/// `main@origin` / `main`, if that revision has one.
pub(super) fn committed_baseline() -> Option<String> {
    committed_baseline_file("tests/perf.toml")
}

pub(super) fn committed_baseline_benches() -> Option<String> {
    committed_baseline_file("benches/baselines.toml")
}

fn committed_baseline_file(rel_path: &str) -> Option<String> {
    let root = std::env::var_os("BRANCHDIFF_TEST_BASELINE_REPO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    let rev = std::env::var("PERF_BASELINE_REV").ok();
    let github_ref = std::env::var("GITHUB_REF").ok();
    committed_baseline_file_in(&root, rev.as_deref(), github_ref.as_deref(), rel_path)
}

#[cfg(test)]
pub(super) fn committed_baseline_in(root: &Path, rev_override: Option<&str>) -> Option<String> {
    committed_baseline_file_in(root, rev_override, None, "tests/perf.toml")
}

pub(super) fn committed_baseline_file_in(
    root: &Path,
    rev_override: Option<&str>,
    github_ref: Option<&str>,
    rel_path: &str,
) -> Option<String> {
    let rev_override = rev_override.map(str::trim).filter(|rev| !rev.is_empty());
    let github_ref = github_ref.map(str::trim).filter(|gref| !gref.is_empty());
    if root.join(".jj").exists() {
        committed_baseline_jj(root, rev_override, github_ref, rel_path)
    } else {
        committed_baseline_git(root, rev_override, github_ref, rel_path)
    }
}

fn is_all_zero_sha(rev: &str) -> bool {
    !rev.is_empty() && rev.bytes().all(|b| b == b'0')
}

fn committed_baseline_jj(
    root: &Path,
    rev_override: Option<&str>,
    github_ref: Option<&str>,
    rel_path: &str,
) -> Option<String> {
    let repo = root.to_str().expect("repo path");
    let on_main = github_ref.is_none_or(|gref| gref == "refs/heads/main" || gref == "main");
    if let Some(rev) = rev_override {
        if !on_main {
            let is_pr = github_ref.is_some_and(|gref| gref.starts_with("refs/pull/"));
            if is_pr
                && !is_all_zero_sha(rev)
                && let Some(found) = read_jj_file_at(root, repo, rev, rel_path)
            {
                return found;
            }
            for revset in
                ["heads(::@ & ::main@origin)", "heads(::@ & ::main)", "main@origin", "main"]
            {
                if let Some(found) = read_jj_file_at(root, repo, revset, rel_path) {
                    return found;
                }
            }
        }
        if is_all_zero_sha(rev) {
            let exists_in_parent =
                read_jj_file_at(root, repo, "heads(::@ & ~empty())-", rel_path).flatten().is_some();
            assert!(
                !exists_in_parent,
                "PERF_BASELINE_REV is all-zero ({rev}) but {rel_path} already exists in the parent revision of {}",
                root.display()
            );
            return None;
        }
        return read_jj_file_at(root, repo, rev, rel_path).unwrap_or_else(|| {
            panic!("PERF_BASELINE_REV `{rev}` did not resolve in jj repository {}", root.display())
        });
    }
    if !on_main {
        for revset in ["heads(::@ & ::main@origin)", "heads(::@ & ::main)"] {
            if let Some(found) = read_jj_file_at(root, repo, revset, rel_path) {
                return found;
            }
        }
    }
    for rev in ["main@origin", "main"] {
        if let Some(found) = read_jj_file_at(root, repo, rev, rel_path) {
            return found;
        }
    }
    panic!("neither main@origin nor main resolved in jj repository {}", root.display());
}

fn read_jj_file_at(root: &Path, repo: &str, rev: &str, rel_path: &str) -> Option<Option<String>> {
    let resolved = Command::new("jj")
        .current_dir(root)
        .args([
            "--ignore-working-copy", "--repository", repo, "log", "--no-graph", "-r", rev, "-T",
            "commit_id ++ \"\\n\"",
        ])
        .output()
        .unwrap_or_else(|err| panic!("jj log failed to start: {err}"));
    if !resolved.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&resolved.stdout);
    let mut commits: Vec<&str> = stdout.lines().map(str::trim).filter(|s| !s.is_empty()).collect();
    commits.sort_unstable();
    let commit_id = *commits.first()?;
    let root_pattern = format!("root:\"{rel_path}\"");
    let listed = Command::new("jj")
        .current_dir(root)
        .args([
            "--ignore-working-copy", "--repository", repo, "file", "list", "-r", commit_id,
            &root_pattern,
        ])
        .output()
        .unwrap_or_else(|err| panic!("jj file list failed to start: {err}"));
    assert!(
        listed.status.success(),
        "jj file list -r {commit_id} {rel_path} failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    if String::from_utf8_lossy(&listed.stdout).trim().is_empty() {
        return Some(None);
    }
    let shown = Command::new("jj")
        .current_dir(root)
        .args([
            "--ignore-working-copy", "--repository", repo, "file", "show", "-r", commit_id,
            &root_pattern,
        ])
        .output()
        .unwrap_or_else(|err| panic!("jj file show failed to start: {err}"));
    assert!(
        shown.status.success(),
        "jj file show -r {commit_id} {rel_path} failed: {}",
        String::from_utf8_lossy(&shown.stderr)
    );
    Some(Some(String::from_utf8(shown.stdout).expect("baseline is utf-8")))
}

fn is_on_main_git(repo: &str, github_ref: Option<&str>) -> bool {
    if let Some(gref) = github_ref {
        return gref == "refs/heads/main" || gref == "main";
    }
    let out = Command::new("git")
        .args(["-C", repo, "symbolic-ref", "--short", "-q", "HEAD"])
        .output()
        .unwrap_or_else(|err| panic!("git symbolic-ref failed to start: {err}"));
    if out.status.success() {
        let branch = String::from_utf8_lossy(&out.stdout);
        return branch.trim() == "main";
    }
    true
}

fn git_merge_base(repo: &str, left: &str, right: &str) -> String {
    let out = Command::new("git")
        .args(["-C", repo, "merge-base", left, right])
        .output()
        .unwrap_or_else(|err| panic!("git merge-base failed to start: {err}"));
    assert!(
        out.status.success(),
        "git merge-base {left} {right} failed in {repo}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("merge-base utf-8").trim().to_string()
}

fn committed_baseline_git(
    root: &Path,
    rev_override: Option<&str>,
    github_ref: Option<&str>,
    rel_path: &str,
) -> Option<String> {
    let repo = root.to_str().expect("repo path");
    let on_main = is_on_main_git(repo, github_ref);
    if let Some(rev) = rev_override {
        if !on_main {
            let is_pr = github_ref.is_some_and(|gref| gref.starts_with("refs/pull/"));
            if is_pr
                && !is_all_zero_sha(rev)
                && let Some(found) = read_git_file_at(repo, rev, rel_path)
            {
                return found;
            }
            let mb = git_merge_base(repo, "HEAD", "origin/main");
            return read_git_file_at(repo, &mb, rel_path).unwrap_or_else(|| {
                panic!(
                    "merge-base `{mb}` with origin/main did not resolve in git repository {}",
                    root.display()
                )
            });
        }
        if is_all_zero_sha(rev) {
            let exists_in_parent = read_git_file_at(repo, "HEAD~1", rel_path).flatten().is_some();
            assert!(
                !exists_in_parent,
                "PERF_BASELINE_REV is all-zero ({rev}) but {rel_path} already exists in HEAD~1 of {}",
                root.display()
            );
            return None;
        }
        return read_git_file_at(repo, rev, rel_path).unwrap_or_else(|| {
            panic!("PERF_BASELINE_REV `{rev}` did not resolve in git repository {}", root.display())
        });
    }
    if !on_main
        && Command::new("git")
            .args(["-C", repo, "rev-parse", "--verify", "--quiet", "origin/main^{commit}"])
            .status()
            .is_ok_and(|s| s.success())
    {
        let mb = git_merge_base(repo, "HEAD", "origin/main");
        if let Some(found) = read_git_file_at(repo, &mb, rel_path) {
            return found;
        }
    }
    for rev in ["origin/main", "main"] {
        if let Some(found) = read_git_file_at(repo, rev, rel_path) {
            return found;
        }
    }
    panic!("neither origin/main nor main resolved in git repository {}", root.display());
}

fn read_git_file_at(repo: &str, rev: &str, rel_path: &str) -> Option<Option<String>> {
    let verify_arg = format!("{rev}^{{commit}}");
    let resolved = Command::new("git")
        .args(["-C", repo, "rev-parse", "--verify", "--quiet", &verify_arg])
        .output()
        .unwrap_or_else(|err| panic!("git rev-parse failed to start: {err}"));
    if !resolved.status.success() {
        return None;
    }
    let listed = Command::new("git")
        .args(["-C", repo, "ls-tree", "--name-only", rev, "--", rel_path])
        .output()
        .unwrap_or_else(|err| panic!("git ls-tree failed to start: {err}"));
    assert!(
        listed.status.success(),
        "git ls-tree {rev} {rel_path} failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    if String::from_utf8_lossy(&listed.stdout).trim().is_empty() {
        return Some(None);
    }
    let spec = format!("{rev}:{rel_path}");
    let shown = Command::new("git")
        .args(["-C", repo, "show", &spec])
        .output()
        .unwrap_or_else(|err| panic!("git show failed to start: {err}"));
    assert!(
        shown.status.success(),
        "git show {spec} failed: {}",
        String::from_utf8_lossy(&shown.stderr)
    );
    Some(Some(String::from_utf8(shown.stdout).expect("baseline is utf-8")))
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

    #[test]
    fn perf_baseline_rev_env_var_tighter_commit_fails_and_all_zero_checks_manifest_history() {
        if let Some(repo_os) = std::env::var_os("BRANCHDIFF_TEST_BASELINE_REPO") {
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
                    let benches = super::super::guard::recorded_benches(&bench_text);
                    let errors = super::super::guard::check(&super::super::guard::Input {
                        manifest: &manifest,
                        surface: &surface,
                        tests: &tests,
                        baseline: baseline.as_deref(),
                        benches: &benches,
                    });
                    assert!(
                        errors.is_empty(),
                        "manifest looser than baseline:\n{}",
                        errors.join("\n")
                    );
                }
                "new_zero" => {
                    assert_eq!(baseline, None, "all-zero SHA on a new manifest must return None");
                }
                _ => unreachable!("unknown mode {mode}"),
            }
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
        let new_zero = Command::new(std::env::current_exe().expect("current_exe"))
            .args([
                "--exact",
                "surface::tests::perf_baseline_rev_env_var_tighter_commit_fails_and_all_zero_checks_manifest_history",
                "--nocapture",
            ])
            .env("BRANCHDIFF_TEST_BASELINE_REPO", tmp.path())
            .env("BRANCHDIFF_TEST_BASELINE_MODE", "new_zero")
            .env("PERF_BASELINE_REV", zero)
            .output()
            .expect("spawn new_zero child");
        assert!(
            new_zero.status.success(),
            "all-zero SHA when manifest is new must pass:\n{}",
            String::from_utf8_lossy(&new_zero.stderr)
        );

        fs::write(tmp.path().join("tests/perf.toml"), &live_manifest)
            .expect("write live perf.toml");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "loosen perf.toml"]);

        let tighter_out = Command::new(std::env::current_exe().expect("current_exe"))
            .args([
                "--exact",
                "surface::tests::perf_baseline_rev_env_var_tighter_commit_fails_and_all_zero_checks_manifest_history",
                "--nocapture",
            ])
            .env("BRANCHDIFF_TEST_BASELINE_REPO", tmp.path())
            .env("BRANCHDIFF_TEST_BASELINE_MODE", "tighter")
            .env("PERF_BASELINE_REV", &tighter_sha)
            .output()
            .expect("spawn tighter child");
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

        let existing_zero = Command::new(std::env::current_exe().expect("current_exe"))
            .args([
                "--exact",
                "surface::tests::perf_baseline_rev_env_var_tighter_commit_fails_and_all_zero_checks_manifest_history",
                "--nocapture",
            ])
            .env("BRANCHDIFF_TEST_BASELINE_REPO", tmp.path())
            .env("BRANCHDIFF_TEST_BASELINE_MODE", "new_zero")
            .env("PERF_BASELINE_REV", zero)
            .output()
            .expect("spawn existing_zero child");
        assert!(
            !existing_zero.status.success(),
            "all-zero SHA when manifest already existed in parent must fail loudly"
        );
    }

    #[test]
    #[should_panic(expected = "resolved")]
    fn committed_baseline_panics_when_revision_does_not_resolve() {
        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "jj", &["git", "init", "--colocate"]);
        let _ = committed_baseline_in(tmp.path(), None);
    }

    #[test]
    #[should_panic(expected = "all-zero")]
    fn committed_baseline_jj_panics_on_all_zero_sha_when_manifest_exists_in_parent() {
        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "jj", &["git", "init", "--colocate"]);
        fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        fs::write(tmp.path().join("tests/perf.toml"), "# first\n").expect("write perf.toml");
        run_cmd(tmp.path(), "jj", &["commit", "-m", "add perf"]);
        let zero = "0000000000000000000000000000000000000000";
        assert_eq!(committed_baseline_in(tmp.path(), Some(zero)), None);

        fs::write(tmp.path().join("tests/perf.toml"), "# second\n").expect("write second");
        run_cmd(tmp.path(), "jj", &["commit", "-m", "update perf"]);
        let _ = committed_baseline_in(tmp.path(), Some(zero));
    }

    #[test]
    fn committed_baseline_returns_none_when_revision_resolves_and_lacks_file_and_reads_git_checkout()
     {
        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "git", &["init", "-q", "-b", "main"]);
        fs::write(tmp.path().join("README.md"), "hello\n").expect("write readme");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "init"]);
        assert_eq!(committed_baseline_in(tmp.path(), None), None);

        fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        fs::write(tmp.path().join("tests/perf.toml"), "# baseline\n").expect("write perf.toml");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "add perf"]);
        assert_eq!(committed_baseline_in(tmp.path(), None).as_deref(), Some("# baseline\n"));
    }

    #[test]
    fn non_main_branch_push_and_force_push_resolve_baseline_via_origin_main_merge_base() {
        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "git", &["init", "-q", "-b", "main"]);
        fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        fs::write(tmp.path().join("tests/perf.toml"), "# origin-main-baseline\n")
            .expect("write main perf.toml");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "main baseline"]);
        let main_sha = git_head(tmp.path());
        run_cmd(tmp.path(), "git", &["update-ref", "refs/remotes/origin/main", &main_sha]);

        run_cmd(tmp.path(), "git", &["checkout", "-q", "-b", "feature"]);
        fs::write(tmp.path().join("tests/perf.toml"), "# feature-commit-1\n")
            .expect("write feature 1");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "feature commit 1"]);
        let feature_1_sha = git_head(tmp.path());

        fs::write(tmp.path().join("tests/perf.toml"), "# feature-commit-2\n")
            .expect("write feature 2");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "feature commit 2"]);

        let zero = "0000000000000000000000000000000000000000";
        let unreachable_sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

        // New-branch push: before is all-zeros while HEAD~1 already has tests/perf.toml.
        assert_eq!(
            committed_baseline_file_in(
                tmp.path(),
                Some(zero),
                Some("refs/heads/feature"),
                "tests/perf.toml"
            )
            .as_deref(),
            Some("# origin-main-baseline\n"),
            "new-branch push must resolve baseline via merge-base with origin/main"
        );

        // Force-push: old before SHA is unreachable.
        assert_eq!(
            committed_baseline_file_in(
                tmp.path(),
                Some(unreachable_sha),
                Some("refs/heads/feature"),
                "tests/perf.toml"
            )
            .as_deref(),
            Some("# origin-main-baseline\n"),
            "force-push with unreachable before SHA must resolve baseline via merge-base with origin/main"
        );

        // Incremental push to feature branch: still baselines against merge-base with origin/main.
        assert_eq!(
            committed_baseline_file_in(
                tmp.path(),
                Some(&feature_1_sha),
                Some("refs/heads/feature"),
                "tests/perf.toml"
            )
            .as_deref(),
            Some("# origin-main-baseline\n"),
            "feature branch push must baseline against merge-base with origin/main"
        );

        // PR with reachable base.sha uses base.sha directly; unreachable base.sha falls back to merge-base.
        assert_eq!(
            committed_baseline_file_in(
                tmp.path(),
                Some(&feature_1_sha),
                Some("refs/pull/1/merge"),
                "tests/perf.toml"
            )
            .as_deref(),
            Some("# feature-commit-1\n"),
            "PR with reachable base.sha must use that revision"
        );
        assert_eq!(
            committed_baseline_file_in(
                tmp.path(),
                Some(unreachable_sha),
                Some("refs/pull/1/merge"),
                "tests/perf.toml"
            )
            .as_deref(),
            Some("# origin-main-baseline\n"),
            "PR with unreachable base.sha must fall back to merge-base with origin/main"
        );
    }

    #[test]
    fn jj_criss_cross_merge_with_multiple_merge_base_heads_picks_one_deterministically() {
        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "jj", &["git", "init", "--colocate"]);
        fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        fs::write(tmp.path().join("tests/perf.toml"), "# root\n").expect("write root");
        run_cmd(tmp.path(), "jj", &["commit", "-m", "root"]);

        // Two diverging commits A and B above root, both with the same baseline file.
        run_cmd(tmp.path(), "jj", &["new", "@-", "-m", "A"]);
        fs::write(tmp.path().join("a.txt"), "a\n").expect("write a");
        fs::write(tmp.path().join("tests/perf.toml"), "# criss-cross-base\n")
            .expect("write perf A");
        run_cmd(tmp.path(), "jj", &["bookmark", "create", "branch-a", "-r", "@"]);

        run_cmd(tmp.path(), "jj", &["new", "description(substring:\"root\")", "-m", "B"]);
        fs::write(tmp.path().join("b.txt"), "b\n").expect("write b");
        fs::write(tmp.path().join("tests/perf.toml"), "# criss-cross-base\n")
            .expect("write perf B");
        run_cmd(tmp.path(), "jj", &["bookmark", "create", "branch-b", "-r", "@"]);

        // Merge 1 (`main` bookmark): parents branch-a and branch-b.
        run_cmd(tmp.path(), "jj", &["new", "branch-a", "branch-b", "-m", "main merge"]);
        fs::write(tmp.path().join("m1.txt"), "m1\n").expect("write m1");
        run_cmd(tmp.path(), "jj", &["bookmark", "create", "main", "-r", "@"]);

        // Merge 2 (`@` feature): also parents branch-a and branch-b -> criss-cross!
        run_cmd(tmp.path(), "jj", &["new", "branch-a", "branch-b", "-m", "feature merge"]);
        fs::write(tmp.path().join("m2.txt"), "m2\n").expect("write m2");

        // Verify `heads(::@ & ::main)` returns 2 revisions (branch-a and branch-b).
        let heads_out = Command::new("jj")
            .current_dir(tmp.path())
            .args([
                "--ignore-working-copy", "log", "--no-graph", "-r", "heads(::@ & ::main)", "-T",
                "commit_id ++ \"\\n\"",
            ])
            .output()
            .expect("jj log heads");
        let heads_stdout = String::from_utf8_lossy(&heads_out.stdout);
        let heads: Vec<_> = heads_stdout.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(heads.len(), 2, "criss-cross merge must produce 2 heads: {heads:?}");

        // committed_baseline_file_in must deterministically resolve one head without failing `jj file list`.
        let resolved = committed_baseline_file_in(
            tmp.path(),
            Some("0000000000000000000000000000000000000000"),
            Some("refs/heads/feature"),
            "tests/perf.toml",
        );
        assert_eq!(resolved.as_deref(), Some("# criss-cross-base\n"));
    }

    #[test]
    #[should_panic(expected = "did not resolve")]
    fn main_branch_push_with_unreachable_before_sha_panics_loudly() {
        let tmp = TempDir::new().expect("tempdir");
        run_cmd(tmp.path(), "git", &["init", "-q", "-b", "main"]);
        fs::create_dir_all(tmp.path().join("tests")).expect("mkdir tests");
        fs::write(tmp.path().join("tests/perf.toml"), "# main\n").expect("write perf.toml");
        run_cmd(tmp.path(), "git", &["add", "."]);
        run_cmd(tmp.path(), "git", &["commit", "-q", "-m", "main"]);
        let _ = committed_baseline_file_in(
            tmp.path(),
            Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
            Some("refs/heads/main"),
            "tests/perf.toml",
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
        let benches = super::super::guard::recorded_benches(&bench_text);

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
