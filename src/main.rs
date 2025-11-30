mod app;
mod diff;
mod git;
mod input;
mod ui;

use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
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

use app::App;
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
    let (tx, rx) = mpsc::channel();
    let mut debouncer = new_debouncer(Duration::from_millis(100), tx)?;

    // Watch the repo directory
    debouncer
        .watcher()
        .watch(&repo_root, notify::RecursiveMode::Recursive)?;

    // Main loop
    let result = run_app(&mut terminal, &mut app, rx);

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

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    file_events: mpsc::Receiver<Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>>,
) -> Result<()> {
    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        // Check for file changes (non-blocking)
        if let Ok(Ok(events)) = file_events.try_recv() {
            // Filter out irrelevant events (like .git/index.lock)
            let should_refresh = events.iter().any(|e| {
                e.kind == DebouncedEventKind::Any
                    && !e
                        .path
                        .to_string_lossy()
                        .contains(".git/index.lock")
            });
            if should_refresh {
                app.refresh()?;
            }
        }

        // Poll for input events with timeout
        if event::poll(Duration::from_millis(50))? {
            let event = event::read()?;
            match handle_event(event) {
                AppAction::Quit => return Ok(()),
                AppAction::ScrollUp(n) => app.scroll_up(n),
                AppAction::ScrollDown(n) => app.scroll_down(n),
                AppAction::PageUp => app.page_up(),
                AppAction::PageDown => app.page_down(),
                AppAction::GoToTop => app.go_to_top(),
                AppAction::GoToBottom => app.go_to_bottom(),
                AppAction::Refresh => app.refresh()?,
                AppAction::ToggleHelp => app.toggle_help(),
                AppAction::ToggleContextOnly => app.toggle_context_only(),
                AppAction::StartSelection(x, y) => app.start_selection(x, y),
                AppAction::UpdateSelection(x, y) => app.update_selection(x, y),
                AppAction::EndSelection => app.end_selection(),
                AppAction::Copy => { let _ = app.copy_selection(); }
                AppAction::None => {}
            }
        }
    }
}
