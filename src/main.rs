mod app;
mod diff;
mod git;
mod input;
mod ui;

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use ratatui::prelude::*;

use app::{compute_refresh, App, RefreshResult};
use input::{handle_event, AppAction};

#[derive(Parser)]
#[command(name = "branchdiff")]
#[command(about = "Terminal UI showing unified diff of current branch vs main/master")]
#[command(version)]
struct Cli {
    /// Path to git repository (default: current directory)
    #[arg(default_value = ".")]
    path: PathBuf,
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
    let mut debouncer = new_debouncer(Duration::from_millis(100), file_tx)?;

    // Watch the repo directory
    debouncer
        .watcher()
        .watch(&repo_root, notify::RecursiveMode::Recursive)?;

    // Setup refresh channel for background git operations
    let (refresh_tx, refresh_rx) = mpsc::channel::<RefreshResult>();

    // Main loop
    let result = run_app(
        &mut terminal,
        &mut app,
        file_rx,
        refresh_tx,
        refresh_rx,
        repo_root,
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

fn spawn_refresh(
    repo_path: PathBuf,
    base_branch: String,
    refresh_tx: mpsc::Sender<RefreshResult>,
    cancel_flag: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        if let Ok(result) = compute_refresh(&repo_path, &base_branch, &cancel_flag) {
            let _ = refresh_tx.send(result);
        }
    });
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    file_events: mpsc::Receiver<Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>>,
    refresh_tx: mpsc::Sender<RefreshResult>,
    refresh_rx: mpsc::Receiver<RefreshResult>,
    repo_root: PathBuf,
) -> Result<()> {
    let mut refresh_in_progress = false;
    let mut refresh_pending = false;
    let cancel_flag = Arc::new(AtomicBool::new(false));

    loop {
        // 1. ALWAYS check for input FIRST with short timeout for responsiveness
        if event::poll(Duration::from_millis(10))? {
            let event = event::read()?;
            match handle_event(event) {
                AppAction::Quit => return Ok(()),
                AppAction::ScrollUp(n) => app.scroll_up(n),
                AppAction::ScrollDown(n) => app.scroll_down(n),
                AppAction::PageUp => app.page_up(),
                AppAction::PageDown => app.page_down(),
                AppAction::GoToTop => app.go_to_top(),
                AppAction::GoToBottom => app.go_to_bottom(),
                AppAction::Refresh => {
                    if refresh_in_progress {
                        cancel_flag.store(true, Ordering::Relaxed);
                        refresh_pending = true;
                    } else {
                        refresh_in_progress = true;
                        spawn_refresh(
                            repo_root.clone(),
                            app.base_branch.clone(),
                            refresh_tx.clone(),
                            cancel_flag.clone(),
                        );
                    }
                }
                AppAction::ToggleHelp => app.toggle_help(),
                AppAction::ToggleContextOnly => app.toggle_context_only(),
                AppAction::StartSelection(x, y) => app.start_selection(x, y),
                AppAction::UpdateSelection(x, y) => app.update_selection(x, y),
                AppAction::EndSelection => app.end_selection(),
                AppAction::Copy => {
                    let _ = app.copy_selection();
                }
                AppAction::None => {}
            }
        }

        // 2. Check for completed refresh (non-blocking)
        if let Ok(result) = refresh_rx.try_recv() {
            app.apply_refresh_result(result);
            refresh_in_progress = false;

            if refresh_pending {
                refresh_pending = false;
                cancel_flag.store(false, Ordering::Relaxed);
                refresh_in_progress = true;
                spawn_refresh(
                    repo_root.clone(),
                    app.base_branch.clone(),
                    refresh_tx.clone(),
                    cancel_flag.clone(),
                );
            }
        }

        // 3. Check for file change events (trigger new refresh if idle)
        if let Ok(Ok(events)) = file_events.try_recv() {
            let should_refresh = events.iter().any(|e| {
                e.kind == DebouncedEventKind::Any
                    && !e.path.to_string_lossy().contains(".git/index.lock")
            });
            if should_refresh {
                if refresh_in_progress {
                    cancel_flag.store(true, Ordering::Relaxed);
                    refresh_pending = true;
                } else {
                    refresh_in_progress = true;
                    spawn_refresh(
                        repo_root.clone(),
                        app.base_branch.clone(),
                        refresh_tx.clone(),
                        cancel_flag.clone(),
                    );
                }
            }
        }

        // 4. Render
        terminal.draw(|f| ui::draw(f, app))?;
    }
}
