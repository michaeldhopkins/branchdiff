//! Process-count floors for the commands, and zero-spawn reducer+render for keys.
//!
//! The child PATH contains only the stub directory, which is inherited by every
//! subprocess. `jj` and `git` stubs record one line per spawn and exec the real
//! binary by absolute path; generic stubs for other programs record the spawn
//! attempt and exit 127. Wall clock is not measured here.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tempfile::TempDir;

use branchdiff::app::{App, FrameContext};
use branchdiff::diff::{DiffLine, FileDiff, LineSource};
use branchdiff::input::{AppAction, handle_event};
use branchdiff::message::{LoopAction, OpenTarget, RefreshTrigger, Repaint, UpdateResult};
use branchdiff::ui::draw_with_frame;
use branchdiff::update::RecoveryHint;

use super::guard::{Op, parse_operations};
use super::surface::{action_name, apply_input, search_effect};

const FILES: usize = 500;
const CHANGED: usize = 25;

#[test]
fn command_process_counts_match_their_floors() {
    let ops = manifest_ops();
    let mut mismatches = Vec::new();
    let home = TempDir::new().expect("home");
    let config = write_jj_config(home.path());
    let stub = write_stub(home.path());
    let comments = write_comments_file(home.path());
    for op in ops.iter().filter(|op| op.class == "command" || op.class == "page") {
        let args = args_for(&op.id, &comments)
            .unwrap_or_else(|| panic!("{} has no measurement arm", op.id));
        let needs_repo = !matches!(op.id.as_str(), "command:version" | "command:help");
        let repo = needs_repo.then(|| build_repo(home.path(), &config));
        let first = run_counted(&op.id, &args, repo.as_ref(), &stub, home.path(), &config);
        let second = run_counted(&op.id, &args, repo.as_ref(), &stub, home.path(), &config);
        if first != second || first != op.processes as usize {
            mismatches.push(format!(
                "{} spawned {first} then {second} processes, floor is {}",
                op.id, op.processes
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn keystrokes_reduce_and_render_without_spawning() {
    let Some(counts_os) = std::env::var_os("BRANCHDIFF_KEYSTROKE_COUNTS") else {
        let home = TempDir::new().expect("home");
        let stub = write_stub(home.path());
        let counts = home.path().join("keystroke-counts.txt");
        fs::write(&counts, "").expect("init keystroke counts");
        let output = Command::new(std::env::current_exe().expect("current_exe"))
            .args([
                "--exact",
                "counts::keystrokes_reduce_and_render_without_spawning",
                "--nocapture",
            ])
            .env_clear()
            .env("PATH", stub_env_path(&stub))
            .env("HOME", home.path())
            .env("BRANCHDIFF_PERF_COUNTS", &counts)
            .env("BRANCHDIFF_KEYSTROKE_COUNTS", &counts)
            .output()
            .expect("spawn keystroke child");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "keystrokes child failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            stdout.contains("1 passed; 0 failed"),
            "keystrokes child must run exactly 1 test:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert_eq!(count_spawns(&counts), 0, "keystrokes must not spawn any process");
        return;
    };
    let counts = PathBuf::from(counts_os);
    let files = render_files();
    let mut rendered = 0;
    let mut recorded_effects = BTreeSet::new();
    for op in manifest_ops().iter().filter(|op| op.class == "keystroke") {
        assert_eq!(op.processes, 0, "{} is in-process and its process floor is 0", op.id);
        let drive = drive_for(&op.id).unwrap_or_else(|| panic!("{} has no key arm", op.id));
        let mut app = prepare(&files, &op.id);
        match drive {
            Drive::Action(event) => {
                let action = if matches!(event, Event::Resize(_, _)) {
                    AppAction::Resize
                } else {
                    handle_event(event)
                };
                assert_eq!(action_name(action), Some(op.id.as_str()), "{}", op.id);
                if skips_reducer(&op.id) {
                    exercise_clipboard_prep(&mut app, &op.id);
                } else {
                    let result = apply_input(&mut app, action);
                    record_effect(&op.id, &result, &mut recorded_effects);
                }
            }
            Drive::Search(event) => {
                let effect = search_effect(&mut app, event);
                assert_eq!(effect, Some(op.id.as_str()), "{}", op.id);
            }
        }
        paint(&mut app);
        assert_eq!(count_spawns(&counts), 0, "{} spawned a process", op.id);
        rendered += 1;
    }
    assert!(rendered > 0, "the manifest has no keystrokes");
    for expected in [
        "key:quit", "key:copy-or-quit", "key:force-repaint", "key:resize", "key:toggle-diff-base",
        "key:open-editor", "key:open-repo", "key:run-recovery",
    ] {
        assert!(
            recorded_effects.contains(expected),
            "effect runner did not observe expected effect for {expected}"
        );
    }
}

fn record_effect(id: &str, result: &UpdateResult, recorded: &mut BTreeSet<String>) {
    match id {
        "key:quit" | "key:copy-or-quit" => {
            assert_eq!(result.loop_action, LoopAction::Quit, "{id}");
            recorded.insert(id.to_string());
        }
        "key:force-repaint" => {
            assert_eq!(result.repaint, Repaint::Full, "{id}");
            assert!(result.needs_redraw, "{id}");
            recorded.insert(id.to_string());
        }
        "key:resize" => {
            assert!(result.needs_redraw, "{id}");
            recorded.insert(id.to_string());
        }
        "key:toggle-diff-base" => {
            assert_eq!(result.refresh, RefreshTrigger::Full, "{id}");
            recorded.insert(id.to_string());
        }
        "key:open-editor" => {
            assert!(matches!(result.open_editor, Some(OpenTarget::File(_))), "{id}");
            recorded.insert(id.to_string());
        }
        "key:open-repo" => {
            assert_eq!(result.open_editor, Some(OpenTarget::Repo), "{id}");
            recorded.insert(id.to_string());
        }
        "key:run-recovery" => {
            assert!(result.trigger_recovery.is_some(), "{id}");
            recorded.insert(id.to_string());
        }
        _ => {}
    }
}

fn manifest_ops() -> Vec<Op> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/perf.toml");
    let text = fs::read_to_string(&path).expect("read tests/perf.toml");
    parse_operations(&text).unwrap_or_else(|errors| panic!("{}", errors.join("\n")))
}

fn args_for(id: &str, comments: &Path) -> Option<Vec<String>> {
    let strs: &[&str] = match id {
        "command:branchdiff" | "command:benchmark" => &["--benchmark", "1", "--no-auto-fetch"],
        "command:print" | "command:no-auto-fetch" => &["--print", "--no-auto-fetch"],
        "command:diff" => &["--diff", "--no-auto-fetch"],
        "command:files" => &["--files", "--no-auto-fetch"],
        "command:json" => &["--files", "--json", "--no-auto-fetch"],
        "command:fingerprint" => &["--fingerprint", "--no-auto-fetch"],
        "command:base" => &["--print", "--base", "@-", "--no-auto-fetch"],
        "command:comments" => {
            return Some(vec![
                "--print".to_string(),
                "--comments".to_string(),
                comments.display().to_string(),
                "--no-auto-fetch".to_string(),
            ]);
        }
        "command:version" => &["--version"],
        "command:help" => &["--help"],
        "page:html-live" => &["--html", "--no-auto-fetch"],
        "page:html-static" => &["--html=static", "--no-auto-fetch"],
        _ => return None,
    };
    Some(strs.iter().map(|s| (*s).to_string()).collect())
}

fn run_counted(
    id: &str,
    args: &[String],
    repo: Option<&TempDir>,
    stub: &Path,
    home: &Path,
    config: &Path,
) -> usize {
    let counts = home.join(format!("counts-{}.txt", id.replace(':', "-")));
    fs::write(&counts, "").expect("reset counts");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_branchdiff"));
    cmd.args(args);
    if let Some(repo) = repo {
        cmd.current_dir(repo.path());
    }
    let path = stub_env_path(stub);
    isolated(&mut cmd, home, config, &path);
    cmd.env("BRANCHDIFF_PERF_COUNTS", &counts);
    let output = cmd.output().unwrap_or_else(|err| panic!("{id} failed to start: {err}"));
    assert_output(id, &output);
    let unexpected = unexpected_spawns(&counts);
    assert!(unexpected.is_empty(), "{id} spawned unexpected program(s): {}", unexpected.join(", "));
    count_spawns(&counts)
}

fn stub_env_path(stub: &Path) -> String {
    stub.display().to_string()
}

fn assert_output(id: &str, output: &Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{id} failed\nstdout:\n{}\nstderr:\n{}",
        tail(&stdout),
        tail(&stderr)
    );
    match id {
        "page:html-live" => {
            assert!(stdout.to_lowercase().contains("<html"), "{id}");
            assert!(stdout.contains("file_0.txt"), "{id}");
            assert!(stdout.contains("setInterval"), "{id} dropped the live poll");
        }
        "page:html-static" => {
            assert!(stdout.to_lowercase().contains("<html"), "{id}");
            assert!(stdout.contains("file_0.txt"), "{id}");
            assert!(!stdout.contains("setInterval"), "{id} still polls");
        }
        "command:version" => {
            assert!(stdout.chars().any(|ch| ch.is_ascii_digit()), "{id}: {stdout}")
        }
        "command:help" => assert!(stdout.contains("--print"), "{id}:\n{}", tail(&stdout)),
        "command:fingerprint" => assert!(!stdout.trim().is_empty(), "{id} wrote nothing"),
        "command:comments" => {
            assert!(
                stdout.contains("file_0.txt") && stdout.contains("review note 0"),
                "{id} did not render comments\n{}",
                tail(&stdout)
            );
        }
        "command:branchdiff" | "command:benchmark" => {
            assert!(stderr.contains("Running 1 frames"), "{id}:\n{}", tail(&stderr));
            assert!(!stderr.contains("No changes to benchmark"), "{id}:\n{}", tail(&stderr));
        }
        _ => assert!(stdout.contains("file_0"), "{id} did not show the fixture\n{}", tail(&stdout)),
    }
}

fn build_repo(home: &Path, config: &Path) -> TempDir {
    let repo = TempDir::new().expect("repo");
    tool(home, config, repo.path(), "jj", &["git", "init", "--colocate"]);
    for index in 0..FILES {
        let path = repo.path().join(format!("file_{index}.txt"));
        fs::write(&path, "one\ntwo\nthree\nfour\nfive\n").expect("write file");
    }
    tool(home, config, repo.path(), "jj", &["commit", "-m", "base"]);
    for index in 0..CHANGED {
        let path = repo.path().join(format!("file_{index}.txt"));
        fs::write(&path, "one\nCHANGED\nthree\nfour\nfive\n").expect("dirty file");
    }
    repo
}

fn write_comments_file(home: &Path) -> PathBuf {
    let path = home.join("comments.jsonl");
    let mut body = String::new();
    for index in 0..CHANGED {
        body.push_str(&format!(
            "{{\"path\":\"file_{index}.txt\",\"line\":2,\"body\":\"review note {index}\"}}\n"
        ));
    }
    fs::write(&path, body).expect("comments file");
    path
}

fn tool(home: &Path, config: &Path, dir: &Path, program: &str, args: &[&str]) {
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(dir);
    isolated(&mut cmd, home, config, &real_path());
    let output = cmd.output().unwrap_or_else(|err| panic!("{program} failed to start: {err}"));
    assert!(
        output.status.success(),
        "{program} {args:?} failed\n{}",
        tail(&String::from_utf8_lossy(&output.stderr))
    );
}

fn isolated(cmd: &mut Command, home: &Path, config: &Path, path: &str) {
    cmd.env_clear();
    cmd.env("PATH", path);
    cmd.env("HOME", home);
    cmd.env("JJ_CONFIG", config);
    cmd.env("JJ_USER", "Perf");
    cmd.env("JJ_EMAIL", "perf@example.com");
    cmd.env("GIT_AUTHOR_NAME", "Perf");
    cmd.env("GIT_AUTHOR_EMAIL", "perf@example.com");
    cmd.env("GIT_COMMITTER_NAME", "Perf");
    cmd.env("GIT_COMMITTER_EMAIL", "perf@example.com");
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
    cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    cmd.env("GIT_CONFIG_SYSTEM", "/dev/null");
    cmd.env("NO_COLOR", "1");
}

fn write_jj_config(home: &Path) -> PathBuf {
    let path = home.join("jj.toml");
    fs::write(
        &path,
        "[user]\nname = \"Perf\"\nemail = \"perf@example.com\"\n\n[ui]\ncolor = \"never\"\n",
    )
    .expect("jj config");
    path
}

fn resolve_binary(name: &str) -> PathBuf {
    let path_var = std::env::var_os("PATH").expect("PATH");
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return candidate.canonicalize().unwrap_or(candidate);
        }
    }
    panic!("binary `{name}` not found on PATH");
}

const GENERIC_STUB_NAMES: &[&str] = &[
    "sh", "bash", "zsh", "open", "xdg-open", "cmd", "pbcopy", "pbpaste", "xclip", "xsel",
    "wl-copy", "wl-paste", "klipper", "vi", "vim", "nvim", "nvim-remote", "view", "emacs",
    "emacsclient", "hx", "helix", "nano", "code", "code-insiders", "codium", "cursor", "subl",
    "sublime_text", "zed", "xed", "bbedit", "acme", "uname",
];

fn write_stub(home: &Path) -> PathBuf {
    let dir = home.join("stub");
    fs::create_dir_all(&dir).expect("stub dir");
    let record_only = dir.join(".record");
    fs::write(
        &record_only,
        "#!/bin/sh\nprintf '%s\\n' \"${0##*/}\" >> \"$BRANCHDIFF_PERF_COUNTS\"\nexit 127\n",
    )
    .expect("write .record");
    set_executable(&record_only);

    for name in ["jj", "git"] {
        let real = resolve_binary(name);
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"{name}\" >> \"$BRANCHDIFF_PERF_COUNTS\"\nexec \"{}\" \"$@\"\n",
            real.display()
        );
        let path = dir.join(name);
        fs::write(&path, script).expect("stub");
        set_executable(&path);
    }

    let mut names: BTreeSet<String> =
        GENERIC_STUB_NAMES.iter().map(|name| (*name).to_string()).collect();
    if let Some(path_var) = std::env::var_os("PATH") {
        for entry_dir in std::env::split_paths(&path_var) {
            let Ok(entries) = fs::read_dir(entry_dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str()
                    && !name.starts_with('.')
                {
                    names.insert(name.to_string());
                }
            }
        }
    }
    for name in names {
        if matches!(name.as_str(), "jj" | "git") {
            continue;
        }
        let dst = dir.join(&name);
        if !dst.exists() {
            let _ = symlink(".record", &dst);
        }
    }
    dir
}

fn set_executable(path: &Path) {
    let mut permissions = fs::metadata(path).expect("stub metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("stub mode");
}

fn real_path() -> String {
    std::env::var("PATH").expect("PATH")
}

fn count_spawns(path: &Path) -> usize {
    fs::read_to_string(path).expect("counts").lines().filter(|line| !line.trim().is_empty()).count()
}

fn unexpected_spawns(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .expect("counts")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !matches!(*line, "jj" | "git"))
        .map(String::from)
        .collect()
}

fn tail(text: &str) -> String {
    let start = text.floor_char_boundary(text.len().saturating_sub(800));
    text[start..].to_string()
}

fn render_files() -> Vec<FileDiff> {
    let mut files = Vec::with_capacity(30);
    for file in 0..30 {
        let path = format!("file_{file}.txt");
        let mut lines = Vec::with_capacity(301);
        lines.push(DiffLine::file_header(&path));
        for row in 0..300 {
            let (source, prefix, old_line, new_line) = match row % 20 {
                2 => (LineSource::DeletedBase, '-', Some(row + 1), None),
                3 => (LineSource::Committed, '+', None, Some(row + 1)),
                7 => (LineSource::DeletedCommitted, '-', Some(row + 1), None),
                8 => (LineSource::Staged, '+', None, Some(row + 1)),
                12 => (LineSource::Unstaged, '+', None, Some(row + 1)),
                _ => (LineSource::Base, ' ', Some(row + 1), Some(row + 1)),
            };
            let text = if row % 100 == 0 {
                format!("value {file}:{row}")
            } else {
                format!("line {file}:{row}")
            };
            let mut line = DiffLine::new(source, text, prefix, new_line).with_file_path(&path);
            if let Some(old) = old_line {
                line = line.with_old_line_number(old);
            }
            lines.push(line);
        }
        files.push(FileDiff::new(lines));
    }
    files
}

fn prepare(files: &[FileDiff], id: &str) -> App {
    let lines: Vec<DiffLine> = files.iter().flat_map(|file| file.lines.iter().cloned()).collect();
    let mut app = App::new_for_bench(lines);
    app.files = files.iter().map(|file| FileDiff::new(file.lines.clone())).collect();
    app.set_viewport_height(50);
    app.view.scroll_offset = 100;
    app.view.sub_row = 0;
    if id.starts_with("key:search-") {
        app.open_search();
        app.search_insert_char('v');
        if let Some(search) = app.search.as_mut() {
            search.current = 1;
        }
    }
    if id == "key:run-recovery" {
        app.error = Some("Working copy is stale".to_string());
        app.pending_recovery = Some(RecoveryHint::jj_update_stale());
    }
    if matches!(id, "key:start-selection" | "key:update-selection" | "key:end-selection") {
        paint(&mut app);
        if matches!(id, "key:update-selection" | "key:end-selection") {
            app.start_selection(10, 5);
            app.view.last_click = Some((Instant::now(), 10, 5, 1));
        }
    }
    app
}

enum Drive {
    Action(Event),
    Search(Event),
}

fn drive_for(id: &str) -> Option<Drive> {
    Some(match id {
        "key:quit" => action(KeyCode::Char('q'), KeyModifiers::NONE),
        "key:copy-or-quit" => action(KeyCode::Char('c'), KeyModifiers::CONTROL),
        "key:force-repaint" => action(KeyCode::Char('l'), KeyModifiers::CONTROL),
        "key:scroll-up" => action(KeyCode::Up, KeyModifiers::NONE),
        "key:scroll-down" => action(KeyCode::Down, KeyModifiers::NONE),
        "key:next-file" => action(KeyCode::Char('j'), KeyModifiers::NONE),
        "key:prev-file" => action(KeyCode::Char('k'), KeyModifiers::NONE),
        "key:page-up" => action(KeyCode::PageUp, KeyModifiers::NONE),
        "key:page-down" => action(KeyCode::PageDown, KeyModifiers::NONE),
        "key:go-to-top" => action(KeyCode::Char('g'), KeyModifiers::NONE),
        "key:go-to-bottom" => action(KeyCode::Char('G'), KeyModifiers::SHIFT),
        "key:toggle-all-reviewed" => action(KeyCode::Char('R'), KeyModifiers::SHIFT),
        "key:toggle-help" => action(KeyCode::Char('?'), KeyModifiers::NONE),
        "key:cycle-view-mode" => action(KeyCode::Char('c'), KeyModifiers::NONE),
        "key:copy-path" => action(KeyCode::Char('p'), KeyModifiers::NONE),
        "key:copy-diff" => action(KeyCode::Char('Y'), KeyModifiers::SHIFT),
        "key:copy-patch" => action(KeyCode::Char('D'), KeyModifiers::SHIFT),
        "key:open-search" => action(KeyCode::Char('/'), KeyModifiers::NONE),
        "key:toggle-diff-base" => action(KeyCode::Char('m'), KeyModifiers::NONE),
        "key:toggle-reviewed" => action(KeyCode::Char('r'), KeyModifiers::NONE),
        "key:open-editor" => action(KeyCode::Char('e'), KeyModifiers::NONE),
        "key:open-repo" => action(KeyCode::Char('E'), KeyModifiers::SHIFT),
        "key:run-recovery" => action(KeyCode::Char('u'), KeyModifiers::NONE),
        "key:resize" => Drive::Action(Event::Resize(120, 50)),
        "key:start-selection" => Drive::Action(mouse(MouseEventKind::Down(MouseButton::Left))),
        "key:update-selection" => Drive::Action(mouse(MouseEventKind::Drag(MouseButton::Left))),
        "key:end-selection" => Drive::Action(mouse(MouseEventKind::Up(MouseButton::Left))),
        "key:search-type" => search(KeyCode::Char('x'), KeyModifiers::NONE),
        "key:search-backspace" => search(KeyCode::Backspace, KeyModifiers::NONE),
        "key:search-next" => search(KeyCode::Enter, KeyModifiers::NONE),
        "key:search-prev" => search(KeyCode::Enter, KeyModifiers::SHIFT),
        "key:search-close" => search(KeyCode::Esc, KeyModifiers::NONE),
        _ => return None,
    })
}

fn action(code: KeyCode, modifiers: KeyModifiers) -> Drive {
    Drive::Action(key_event(code, modifiers))
}

fn search(code: KeyCode, modifiers: KeyModifiers) -> Drive {
    Drive::Search(key_event(code, modifiers))
}

fn key_event(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent { code, modifiers, kind: KeyEventKind::Press, state: KeyEventState::NONE })
}

fn mouse(kind: MouseEventKind) -> Event {
    Event::Mouse(MouseEvent { kind, column: 10, row: 8, modifiers: KeyModifiers::NONE })
}

fn skips_reducer(id: &str) -> bool {
    matches!(id, "key:copy-path" | "key:copy-diff" | "key:copy-patch")
}

fn exercise_clipboard_prep(app: &mut App, id: &str) {
    match id {
        "key:copy-path" => {
            assert!(app.current_file().is_some(), "current_file must resolve");
        }
        "key:copy-diff" => {
            assert!(!branchdiff::output::prepare(app).files.is_empty(), "diff must not be empty");
        }
        "key:copy-patch" => {
            assert!(
                !branchdiff::patch::generate_patch(app.lines()).is_empty(),
                "generated patch must not be empty"
            );
        }
        _ => {}
    }
}

fn paint(app: &mut App) {
    let backend = TestBackend::new(120, 50);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let items = if app.needs_inline_spans() {
        let items = app.ensure_inline_spans_for_visible(50);
        app.clear_needs_inline_spans();
        Some(items)
    } else {
        None
    };
    terminal
        .draw(|frame| {
            let ctx = match items {
                Some(items) => FrameContext::with_items(items, app),
                None => FrameContext::new(app),
            };
            draw_with_frame(frame, app, &ctx);
        })
        .expect("draw");
    let has_text =
        terminal.backend().buffer().content.iter().any(|cell| !cell.symbol().trim().is_empty());
    assert!(has_text, "rendered frame is blank");
}

#[test]
fn stub_path_is_stub_dir_only_and_counts_any_spawn_including_non_vcs_programs() {
    let home = TempDir::new().expect("home");
    let stub = write_stub(home.path());
    let counts = home.path().join("counts.txt");
    fs::write(&counts, "").expect("init counts");
    assert_eq!(stub_env_path(&stub), stub.display().to_string());
    for name in ["jj", "git"] {
        let body = fs::read_to_string(stub.join(name)).expect("read stub");
        assert!(body.contains("exec \"/"), "{name} stub must exec an absolute path:\n{body}");
    }
    for name in ["uname", "open"] {
        let out = Command::new(name)
            .env_clear()
            .env("PATH", stub_env_path(&stub))
            .env("BRANCHDIFF_PERF_COUNTS", &counts)
            .output()
            .expect("generic recording stub must intercept spawn");
        assert!(!out.status.success(), "{name} generic stub must exit non-zero");
    }
    assert_eq!(count_spawns(&counts), 2, "count_spawns must count non-jj/git spawns");
    assert_eq!(unexpected_spawns(&counts), vec!["uname".to_string(), "open".to_string()]);

    let unstubbed = Command::new("nonexistent-branchdiff-program")
        .env_clear()
        .env("PATH", stub_env_path(&stub))
        .output();
    assert!(unstubbed.is_err(), "program absent from stub dir must fail to start");
}
