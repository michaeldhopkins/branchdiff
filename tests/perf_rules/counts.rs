//! Process-count floors for the commands, and in-process renders for keys.
//!
//! The stub sits ahead of `jj` and `git` on the child PATH, records one line
//! per spawn, then drops itself from PATH and execs the real binary. Nested
//! spawns from `jj` are not counted. Wall clock is not measured here.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tempfile::TempDir;

use branchdiff::app::{App, FrameContext};
use branchdiff::diff::{DiffLine, LineSource};
use branchdiff::input::handle_event;
use branchdiff::ui::draw_with_frame;

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
    for op in ops.iter().filter(|op| op.class == "command" || op.class == "page") {
        let args = args_for(&op.id).unwrap_or_else(|| panic!("{} has no measurement arm", op.id));
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
fn keystrokes_render_without_spawning() {
    let lines = render_lines();
    let mut rendered = 0;
    for op in manifest_ops().iter().filter(|op| op.class == "keystroke") {
        assert_eq!(op.processes, 0, "{} is in-process and its process floor is 0", op.id);
        let drive = drive_for(&op.id).unwrap_or_else(|| panic!("{} has no key arm", op.id));
        let mut app = prepare(&lines, &op.id);
        match drive {
            Drive::Action(event) => {
                let action = handle_event(event);
                assert_eq!(action_name(action), Some(op.id.as_str()), "{}", op.id);
                if !skips_reducer(&op.id) {
                    apply_input(&mut app, action);
                }
            }
            Drive::Search(event) => {
                let effect = search_effect(&mut app, event);
                assert_eq!(effect, Some(op.id.as_str()), "{}", op.id);
            }
        }
        paint(&mut app);
        rendered += 1;
    }
    assert!(rendered > 0, "the manifest has no keystrokes");
}

fn manifest_ops() -> Vec<Op> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/perf.toml");
    let text = fs::read_to_string(&path).expect("read tests/perf.toml");
    parse_operations(&text).unwrap_or_else(|errors| panic!("{}", errors.join("\n")))
}

fn args_for(id: &str) -> Option<Vec<&str>> {
    Some(match id {
        "command:branchdiff" | "command:benchmark" => {
            vec!["--benchmark", "1", "--no-auto-fetch"]
        }
        "command:print" | "command:no-auto-fetch" => vec!["--print", "--no-auto-fetch"],
        "command:diff" => vec!["--diff", "--no-auto-fetch"],
        "command:files" => vec!["--files", "--no-auto-fetch"],
        "command:json" => vec!["--files", "--json", "--no-auto-fetch"],
        "command:fingerprint" => vec!["--fingerprint", "--no-auto-fetch"],
        "command:base" => vec!["--print", "--base", "@-", "--no-auto-fetch"],
        "command:version" => vec!["--version"],
        "command:help" => vec!["--help"],
        "page:html-live" => vec!["--html", "--no-auto-fetch"],
        "page:html-static" => vec!["--html=static", "--no-auto-fetch"],
        _ => return None,
    })
}

fn run_counted(
    id: &str,
    args: &[&str],
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
    let path = format!("{}:{}", stub.display(), real_path());
    isolated(&mut cmd, home, config, &path);
    cmd.env("BRANCHDIFF_PERF_COUNTS", &counts);
    cmd.env("BRANCHDIFF_PERF_STUB", stub);
    let output = cmd.output().unwrap_or_else(|err| panic!("{id} failed to start: {err}"));
    assert_output(id, &output);
    count_lines(&counts)
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

fn write_stub(home: &Path) -> PathBuf {
    let dir = home.join("stub");
    fs::create_dir_all(&dir).expect("stub dir");
    let script = r#"#!/bin/sh
name=$(basename "$0")
printf '%s\n' "$name" >> "$BRANCHDIFF_PERF_COUNTS"
dir=$BRANCHDIFF_PERF_STUB
old_ifs=$IFS
IFS=:
new=
for part in $PATH; do
    if [ "$part" != "$dir" ]; then
        if [ -z "$new" ]; then
            new=$part
        else
            new=$new:$part
        fi
    fi
done
IFS=$old_ifs
PATH=$new
export PATH
exec "$name" "$@"
"#;
    for name in ["jj", "git"] {
        let path = dir.join(name);
        fs::write(&path, script).expect("stub");
        let mut permissions = fs::metadata(&path).expect("stub metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("stub mode");
    }
    dir
}

fn real_path() -> String {
    std::env::var("PATH").expect("PATH")
}

fn count_lines(path: &Path) -> usize {
    fs::read_to_string(path).expect("counts").lines().filter(|line| !line.is_empty()).count()
}

fn tail(text: &str) -> String {
    let start = text.floor_char_boundary(text.len().saturating_sub(800));
    text[start..].to_string()
}

fn render_lines() -> Vec<DiffLine> {
    let mut lines = Vec::with_capacity(30 * 301);
    for file in 0..30 {
        lines.push(DiffLine::file_header(&format!("file_{file}.txt")));
        for row in 0..300 {
            let text = if row % 100 == 0 {
                format!("value {file}:{row}")
            } else {
                format!("line {file}:{row}")
            };
            lines.push(DiffLine::new(LineSource::Base, text, ' ', Some(row + 1)));
        }
    }
    lines
}

fn prepare(lines: &[DiffLine], id: &str) -> App {
    let mut app = App::new_for_bench(lines.to_vec());
    app.set_viewport_height(50);
    if id.starts_with("key:search-") {
        app.open_search();
        app.search_insert_char('v');
        if let Some(search) = app.search.as_mut() {
            search.current = 1;
        }
    }
    app.view.scroll_offset = 100;
    app.view.sub_row = 0;
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
    Event::Mouse(MouseEvent { kind, column: 1, row: 1, modifiers: KeyModifiers::NONE })
}

fn skips_reducer(id: &str) -> bool {
    matches!(id, "key:copy-path" | "key:copy-diff" | "key:copy-patch" | "key:end-selection")
}

fn paint(app: &mut App) {
    let backend = TestBackend::new(120, 50);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            let ctx = FrameContext::new(app);
            draw_with_frame(frame, app, &ctx);
        })
        .expect("draw");
}
