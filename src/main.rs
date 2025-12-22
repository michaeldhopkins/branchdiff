mod input;
mod print;

use branchdiff::app::{self, compute_refresh, compute_single_file_diff, App, FrameContext, RefreshResult};
use branchdiff::diff::FileDiff;
use branchdiff::git;

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use ratatui::prelude::*;

use branchdiff::ui;
use input::{handle_event, AppAction};

const FETCH_INTERVAL: Duration = Duration::from_secs(30);
const REFRESH_FALLBACK_INTERVAL: Duration = Duration::from_secs(5);
const REFRESH_WATCHDOG_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Parser)]
#[command(name = "branchdiff")]
#[command(about = "Terminal UI showing unified diff of current branch vs main/master")]
#[command(version)]
struct Cli {
    /// Path to git repository (default: current directory)
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Disable automatic fetching of base branch
    #[arg(long)]
    no_auto_fetch: bool,

    /// Print diff to stdout and exit (non-interactive mode)
    #[arg(short = 'p', long = "print")]
    print: bool,
}

pub struct FetchResult {
    pub has_conflicts: bool,
    pub new_merge_base: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Resolve to absolute path
    let repo_path = cli
        .path
        .canonicalize()
        .context("Failed to resolve repository path")?;

    // Verify it's a git repo
    let repo_root = git::get_repo_root(&repo_path).context("Not a git repository")?;

    // Non-interactive mode: print and exit
    if cli.print {
        let mut app = app::App::new(repo_root)?;
        app.collapsed_files.clear();
        app.view_mode = app::ViewMode::Full;
        for line in &mut app.lines {
            if line.old_content.is_some() {
                line.ensure_inline_spans();
            }
        }
        print::print_diff(&app)?;
        return Ok(());
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app and load initial state
    let mut app = App::new(repo_root.clone())?;

    // Setup file watcher
    let (file_tx, file_rx) = mpsc::channel();
    let mut debouncer = new_debouncer(Duration::from_millis(20), file_tx)?;

    // Watch the repo directory
    debouncer
        .watcher()
        .watch(&repo_root, notify::RecursiveMode::Recursive)?;

    // Setup refresh channel for background git operations
    let (refresh_tx, refresh_rx) = mpsc::channel::<RefreshOutcome>();

    // Main loop
    let result = run_app(
        &mut terminal,
        &mut app,
        file_rx,
        refresh_tx,
        refresh_rx,
        repo_root,
        !cli.no_auto_fetch,
    );

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

enum RefreshOutcome {
    Success(RefreshResult),
    SingleFile { path: String, diff: Option<FileDiff> },
    Cancelled,
}

fn spawn_single_file_refresh(
    repo_path: PathBuf,
    file_path: String,
    merge_base: String,
    refresh_tx: mpsc::Sender<RefreshOutcome>,
) {
    thread::spawn(move || {
        let diff = compute_single_file_diff(&repo_path, &file_path, &merge_base);
        let _ = refresh_tx.send(RefreshOutcome::SingleFile { path: file_path, diff });
    });
}

fn spawn_refresh(
    repo_path: PathBuf,
    base_branch: String,
    refresh_tx: mpsc::Sender<RefreshOutcome>,
    cancel_flag: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        match compute_refresh(&repo_path, &base_branch, &cancel_flag) {
            Ok(result) => {
                let _ = refresh_tx.send(RefreshOutcome::Success(result));
            }
            Err(_) => {
                let _ = refresh_tx.send(RefreshOutcome::Cancelled);
            }
        }
    });
}

fn spawn_fetch(
    repo_path: PathBuf,
    base_branch: String,
    fetch_tx: mpsc::Sender<FetchResult>,
) {
    thread::spawn(move || {
        if git::fetch_base_branch(&repo_path, &base_branch).is_ok() {
            let has_conflicts = git::has_merge_conflicts(&repo_path, &base_branch)
                .unwrap_or(false);

            let new_merge_base = git::get_merge_base_preferring_origin(&repo_path, &base_branch)
                .ok();

            let _ = fetch_tx.send(FetchResult {
                has_conflicts,
                new_merge_base,
            });
        }
    });
}

enum GitEventType {
    /// .git/index changes - triggers refresh
    Index,
    /// .git/HEAD or .git/refs/ changes - triggers refresh and cancels in-progress
    BranchSwitch,
}

fn classify_git_event(path_str: &str) -> Option<GitEventType> {
    if !path_str.contains(".git/") {
        return None;
    }
    if path_str.ends_with(".git/HEAD") || path_str.contains(".git/refs/") {
        Some(GitEventType::BranchSwitch)
    } else if path_str.ends_with(".git/index") {
        Some(GitEventType::Index)
    } else {
        None
    }
}

fn is_noisy_path(path_str: &str) -> bool {
    path_str.contains("/tmp/")
        || path_str.contains("/node_modules/")
        || path_str.contains("/vendor/bundle/")
        || path_str.contains("/.bundle/")
        || path_str.contains("/log/")
        || path_str.ends_with(".lock")
}

enum RefreshState {
    Idle,
    InProgress {
        started_at: Instant,
        cancel_flag: Arc<AtomicBool>,
    },
    InProgressPending {
        started_at: Instant,
        cancel_flag: Arc<AtomicBool>,
    },
}

impl RefreshState {
    fn is_idle(&self) -> bool {
        matches!(self, RefreshState::Idle)
    }

    fn started_at(&self) -> Option<Instant> {
        match self {
            RefreshState::Idle => None,
            RefreshState::InProgress { started_at, .. } => Some(*started_at),
            RefreshState::InProgressPending { started_at, .. } => Some(*started_at),
        }
    }

    fn has_pending(&self) -> bool {
        matches!(self, RefreshState::InProgressPending { .. })
    }

    fn mark_pending(&mut self) {
        if let RefreshState::InProgress { started_at, cancel_flag } = self {
            *self = RefreshState::InProgressPending {
                started_at: *started_at,
                cancel_flag: cancel_flag.clone(),
            };
        }
    }

    fn cancel_and_mark_pending(&mut self) {
        match self {
            RefreshState::InProgress { started_at, cancel_flag } => {
                cancel_flag.store(true, Ordering::Relaxed);
                *self = RefreshState::InProgressPending {
                    started_at: *started_at,
                    cancel_flag: cancel_flag.clone(),
                };
            }
            RefreshState::InProgressPending { cancel_flag, .. } => {
                cancel_flag.store(true, Ordering::Relaxed);
            }
            RefreshState::Idle => {}
        }
    }

    fn start(
        &mut self,
        repo_path: PathBuf,
        base_branch: String,
        refresh_tx: mpsc::Sender<RefreshOutcome>,
    ) {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        *self = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: cancel_flag.clone(),
        };
        spawn_refresh(repo_path, base_branch, refresh_tx, cancel_flag);
    }

    fn start_single_file(
        &mut self,
        repo_path: PathBuf,
        file_path: String,
        merge_base: String,
        refresh_tx: mpsc::Sender<RefreshOutcome>,
    ) {
        *self = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        spawn_single_file_refresh(repo_path, file_path, merge_base, refresh_tx);
    }

    fn complete(&mut self) -> bool {
        let had_pending = self.has_pending();
        *self = RefreshState::Idle;
        had_pending
    }
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    file_events: mpsc::Receiver<Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>>,
    refresh_tx: mpsc::Sender<RefreshOutcome>,
    refresh_rx: mpsc::Receiver<RefreshOutcome>,
    repo_root: PathBuf,
    auto_fetch: bool,
) -> Result<()> {
    let mut refresh_state = RefreshState::Idle;
    let mut last_refresh = Instant::now();

    let (fetch_tx, fetch_rx) = mpsc::channel::<FetchResult>();
    let mut last_fetch = Instant::now();
    let mut fetch_in_progress = false;

    loop {
        // 1. ALWAYS check for input FIRST with short timeout for responsiveness
        if event::poll(Duration::from_millis(10))? {
            let event = event::read()?;
            match handle_event(event) {
                AppAction::Quit => {
                    if app.should_quit() {
                        return Ok(());
                    }
                }
                AppAction::ScrollUp(n) => app.scroll_up(n),
                AppAction::ScrollDown(n) => app.scroll_down(n),
                AppAction::PageUp => app.page_up(),
                AppAction::PageDown => app.page_down(),
                AppAction::GoToTop => app.go_to_top(),
                AppAction::GoToBottom => app.go_to_bottom(),
                AppAction::NextFile => app.next_file(),
                AppAction::PrevFile => app.prev_file(),
                AppAction::Refresh => {
                    if refresh_state.is_idle() {
                        refresh_state.start(
                            repo_root.clone(),
                            app.base_branch.clone(),
                            refresh_tx.clone(),
                        );
                    } else {
                        refresh_state.cancel_and_mark_pending();
                    }
                }
                AppAction::ToggleHelp => app.toggle_help(),
                AppAction::CycleViewMode => app.cycle_view_mode(),
                AppAction::StartSelection(x, y) => {
                    // Check if clicking on a file header - toggle collapse
                    if let Some(file_path) = app.get_file_header_at(x, y) {
                        app.toggle_file_collapsed(&file_path);
                    } else {
                        app.start_selection(x, y);
                    }
                }
                AppAction::UpdateSelection(x, y) => app.update_selection(x, y),
                AppAction::EndSelection => app.end_selection(),
                AppAction::Copy => {
                    let _ = app.copy_selection();
                }
                AppAction::CopyOrQuit => {
                    if app.has_selection() {
                        let _ = app.copy_selection();
                    } else if app.should_quit() {
                        return Ok(());
                    }
                }
                AppAction::None => {}
            }
        }

        // 2. Check for completed refresh (non-blocking)
        if let Ok(outcome) = refresh_rx.try_recv() {
            match outcome {
                RefreshOutcome::Success(result) => {
                    app.apply_refresh_result(result);
                    last_refresh = Instant::now();
                }
                RefreshOutcome::SingleFile { path, diff } => {
                    app.update_single_file(&path, diff);
                    last_refresh = Instant::now();
                }
                RefreshOutcome::Cancelled => {}
            }

            if refresh_state.complete() {
                refresh_state.start(
                    repo_root.clone(),
                    app.base_branch.clone(),
                    refresh_tx.clone(),
                );
            }
        }

        // 3. Check for file change events (trigger new refresh if idle)
        if let Ok(Ok(events)) = file_events.try_recv() {
            let dominated_events: Vec<_> = events.iter()
                .filter(|e| e.kind == DebouncedEventKind::Any)
                .filter(|e| !is_noisy_path(&e.path.to_string_lossy()))
                .collect();

            let mut should_refresh = false;
            let mut has_git_change = false;
            let mut source_files = Vec::new();

            for event in &dominated_events {
                let path_str = event.path.to_string_lossy();
                match classify_git_event(&path_str) {
                    Some(GitEventType::BranchSwitch) => {
                        should_refresh = true;
                        has_git_change = true;
                    }
                    Some(GitEventType::Index) => {
                        should_refresh = true;
                    }
                    None => {
                        if !path_str.contains(".git/") {
                            should_refresh = true;
                            source_files.push(&event.path);
                        }
                    }
                }
            }

            if should_refresh {
                if !refresh_state.is_idle() {
                    if has_git_change {
                        refresh_state.cancel_and_mark_pending();
                    } else {
                        refresh_state.mark_pending();
                    }
                } else {
                    let can_use_single_file = !has_git_change
                        && source_files.len() == 1
                        && !app.files.is_empty();

                    if can_use_single_file {
                        let file_path = source_files[0]
                            .strip_prefix(&repo_root)
                            .unwrap_or(source_files[0])
                            .to_string_lossy()
                            .to_string();

                        refresh_state.start_single_file(
                            repo_root.clone(),
                            file_path,
                            app.merge_base.clone(),
                            refresh_tx.clone(),
                        );
                    } else {
                        refresh_state.start(
                            repo_root.clone(),
                            app.base_branch.clone(),
                            refresh_tx.clone(),
                        );
                    }
                }
            }
        }

        // 4. Check for completed fetch results
        if let Ok(result) = fetch_rx.try_recv() {
            fetch_in_progress = false;
            if result.has_conflicts {
                app.conflict_warning = Some("Merge conflicts detected with remote".to_string());
            } else {
                app.conflict_warning = None;
            }

            if let Some(new_base) = result.new_merge_base {
                if new_base != app.merge_base {
                    app.merge_base = new_base;
                    if refresh_state.is_idle() {
                        refresh_state.start(
                            repo_root.clone(),
                            app.base_branch.clone(),
                            refresh_tx.clone(),
                        );
                    } else {
                        refresh_state.mark_pending();
                    }
                }
            }
        }

        // 5. Trigger periodic fetch if enabled
        if auto_fetch && !fetch_in_progress && last_fetch.elapsed() >= FETCH_INTERVAL {
            fetch_in_progress = true;
            last_fetch = Instant::now();
            spawn_fetch(
                repo_root.clone(),
                app.base_branch.clone(),
                fetch_tx.clone(),
            );
        }

        // 6. Watchdog: reset stuck refresh and start a new one
        if let Some(started) = refresh_state.started_at() {
            if started.elapsed() >= REFRESH_WATCHDOG_TIMEOUT {
                refresh_state.start(
                    repo_root.clone(),
                    app.base_branch.clone(),
                    refresh_tx.clone(),
                );
            }
        }

        // 7. Periodic fallback refresh (in case file watcher stops working)
        if refresh_state.is_idle() && last_refresh.elapsed() >= REFRESH_FALLBACK_INTERVAL {
            refresh_state.start(
                repo_root.clone(),
                app.base_branch.clone(),
                refresh_tx.clone(),
            );
        }

        // 8. Render with FrameContext
        let visible_height = terminal.size()?.height as usize;
        app.ensure_inline_spans_for_visible(visible_height);
        terminal.draw(|f| {
            let frame_ctx = FrameContext::new(app);
            ui::draw_with_frame(f, app, &frame_ctx)
        })?;
    }
}
