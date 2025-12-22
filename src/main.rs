mod print;

use branchdiff::app::{self, compute_refresh, compute_single_file_diff, App, FrameContext};
use branchdiff::input::{handle_event, AppAction};
use branchdiff::message::{FetchResult, Message, RefreshOutcome};
use branchdiff::update::{update, RefreshState, Timers, UpdateConfig};
use branchdiff::git;
use branchdiff::ui;

use std::io;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
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
use notify_debouncer_mini::new_debouncer;
use ratatui::prelude::*;

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

fn main() -> Result<()> {
    let cli = Cli::parse();

    let repo_path = cli
        .path
        .canonicalize()
        .context("Failed to resolve repository path")?;

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

    debouncer
        .watcher()
        .watch(&repo_root, notify::RecursiveMode::Recursive)?;

    // Setup refresh channel for background git operations
    let (refresh_tx, refresh_rx) = mpsc::channel::<RefreshOutcome>();

    // Main loop
    let config = UpdateConfig {
        auto_fetch: !cli.no_auto_fetch,
        ..Default::default()
    };

    let result = run_app(
        &mut terminal,
        &mut app,
        file_rx,
        refresh_tx,
        refresh_rx,
        repo_root,
        config,
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

fn spawn_fetch(repo_path: PathBuf, base_branch: String, fetch_tx: mpsc::Sender<FetchResult>) {
    thread::spawn(move || {
        if git::fetch_base_branch(&repo_path, &base_branch).is_ok() {
            let has_conflicts = git::has_merge_conflicts(&repo_path, &base_branch).unwrap_or(false);
            let new_merge_base =
                git::get_merge_base_preferring_origin(&repo_path, &base_branch).ok();

            let _ = fetch_tx.send(FetchResult {
                has_conflicts,
                new_merge_base,
            });
        }
    });
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    file_events: mpsc::Receiver<Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>>,
    refresh_tx: mpsc::Sender<RefreshOutcome>,
    refresh_rx: mpsc::Receiver<RefreshOutcome>,
    repo_root: PathBuf,
    config: UpdateConfig,
) -> Result<()> {
    let mut refresh_state = RefreshState::Idle;
    let mut timers = Timers::default();

    let (fetch_tx, fetch_rx) = mpsc::channel::<FetchResult>();

    loop {
        // Collect messages from all sources
        let messages = collect_messages(
            &file_events,
            &refresh_rx,
            &fetch_rx,
        )?;

        // Process each message
        for msg in messages {
            let result = update(
                msg,
                app,
                &mut refresh_state,
                &mut timers,
                &config,
                &repo_root,
            );

            if result.quit {
                return Ok(());
            }

            if result.trigger_refresh {
                let cancel_flag = refresh_state.start();
                spawn_refresh(
                    repo_root.clone(),
                    app.base_branch.clone(),
                    refresh_tx.clone(),
                    cancel_flag,
                );
            }

            if let Some(file_path) = result.trigger_single_file {
                refresh_state.start_single_file();
                spawn_single_file_refresh(
                    repo_root.clone(),
                    file_path.to_string_lossy().to_string(),
                    app.merge_base.clone(),
                    refresh_tx.clone(),
                );
            }

            if result.trigger_fetch {
                spawn_fetch(repo_root.clone(), app.base_branch.clone(), fetch_tx.clone());
            }
        }

        // Render with FrameContext
        let visible_height = terminal.size()?.height as usize;
        if app.needs_inline_spans() {
            app.ensure_inline_spans_for_visible(visible_height);
            app.clear_needs_inline_spans();
        }
        terminal.draw(|f| {
            let frame_ctx = FrameContext::new(app);
            ui::draw_with_frame(f, app, &frame_ctx)
        })?;
    }
}

/// Collect messages from all event sources.
fn collect_messages(
    file_events: &mpsc::Receiver<Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>>,
    refresh_rx: &mpsc::Receiver<RefreshOutcome>,
    fetch_rx: &mpsc::Receiver<FetchResult>,
) -> Result<Vec<Message>> {
    let mut messages = Vec::new();

    // Check for input with short timeout for responsiveness
    if event::poll(Duration::from_millis(10))? {
        let event = event::read()?;
        let action = handle_event(event);
        if action != AppAction::None {
            messages.push(Message::Input(action));
        }
    }

    // Check for completed refresh (non-blocking)
    if let Ok(outcome) = refresh_rx.try_recv() {
        messages.push(Message::RefreshCompleted(outcome));
    }

    // Check for file change events
    if let Ok(Ok(events)) = file_events.try_recv() {
        if !events.is_empty() {
            messages.push(Message::FileChanged(events));
        }
    }

    // Check for completed fetch results
    if let Ok(result) = fetch_rx.try_recv() {
        messages.push(Message::FetchCompleted(result));
    }

    // Always send a tick for timer-based operations
    messages.push(Message::Tick);

    Ok(messages)
}
