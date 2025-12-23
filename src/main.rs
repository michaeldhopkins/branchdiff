mod print;

use branchdiff::app::{self, compute_refresh, compute_single_file_diff, App, FrameContext};
use branchdiff::input::{handle_event, AppAction};
use branchdiff::limits;
use branchdiff::message::{FetchResult, Message, RefreshOutcome, RefreshTrigger};
use branchdiff::update::{update, RefreshState, Timers, UpdateConfig};
use branchdiff::git;
use branchdiff::ui;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
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
use ignore::WalkBuilder;
use notify::RecursiveMode::{NonRecursive, Recursive};
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

    /// Run stress test for profiling (renders N frames with simulated input)
    #[arg(long, value_name = "FRAMES")]
    benchmark: Option<usize>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let repo_path = cli
        .path
        .canonicalize()
        .context("Failed to resolve repository path")?;

    let repo_root = git::get_repo_root(&repo_path).context("Not a git repository")?;

    // Detect git version (for feature gating like merge-tree --write-tree)
    let git_version = git::get_git_version().context("Failed to detect git version")?;

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

    // Benchmark mode: stress test for profiling
    if let Some(frames) = cli.benchmark {
        return run_benchmark(repo_root, frames);
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Detect system limits for file watching
    let system_limits = limits::SystemLimits::detect();

    // Create app and load initial state
    let mut app = App::new(repo_root.clone())?;

    // Setup file watcher (only watch non-ignored directories)
    let (file_tx, file_rx) = mpsc::channel();
    let mut debouncer = new_debouncer(Duration::from_millis(20), file_tx)?;

    let watcher_metrics = setup_watcher(debouncer.watcher(), &repo_root, &system_limits)?;

    // Check for watch-related warnings
    if let Some(warning) = system_limits.check_watch_warning(&watcher_metrics) {
        app.performance_warning = Some(warning);
    }

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
        debouncer.watcher(),
        file_rx,
        refresh_tx,
        refresh_rx,
        repo_root,
        config,
        git_version,
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

fn run_benchmark(repo_root: PathBuf, frames: usize) -> Result<()> {
    use ratatui::backend::TestBackend;

    eprintln!("Loading diff from {}...", repo_root.display());
    let load_start = Instant::now();
    let mut app = App::new(repo_root)?;
    let load_time = load_start.elapsed();
    eprintln!(
        "Loaded {} lines across {} files in {:?}",
        app.lines.len(),
        app.files.len(),
        load_time
    );

    if app.lines.is_empty() {
        eprintln!("No changes to benchmark. Try running in a repo with uncommitted changes.");
        return Ok(());
    }

    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend)?;
    let visible_height = 40_usize;

    app.set_viewport_height(visible_height);
    app.collapsed_files.clear();

    eprintln!("Running {} frames...", frames);
    let bench_start = Instant::now();

    let ctx = FrameContext::new(&app);
    let max_scroll = ctx.max_scroll(&app);

    for frame_num in 0..frames {
        let action = match frame_num % 20 {
            0..=4 => AppAction::ScrollDown(3),
            5..=9 => AppAction::ScrollUp(2),
            10 => AppAction::NextFile,
            11 => AppAction::PrevFile,
            12 => AppAction::CycleViewMode,
            13 => AppAction::GoToBottom,
            14 => AppAction::GoToTop,
            15 => AppAction::PageDown,
            16 => AppAction::PageUp,
            _ => AppAction::ScrollDown(1),
        };

        match action {
            AppAction::ScrollDown(n) => {
                let new_offset = (app.scroll_offset + n).min(max_scroll);
                app.scroll_offset = new_offset;
            }
            AppAction::ScrollUp(n) => {
                app.scroll_offset = app.scroll_offset.saturating_sub(n);
            }
            AppAction::NextFile => app.next_file(),
            AppAction::PrevFile => app.prev_file(),
            AppAction::CycleViewMode => app.cycle_view_mode(),
            AppAction::GoToBottom => app.go_to_bottom(),
            AppAction::GoToTop => app.go_to_top(),
            AppAction::PageDown => app.page_down(),
            AppAction::PageUp => app.page_up(),
            _ => {}
        }

        if app.needs_inline_spans() {
            app.ensure_inline_spans_for_visible(visible_height);
            app.clear_needs_inline_spans();
        }

        terminal.draw(|f| {
            let frame_ctx = FrameContext::new(&app);
            ui::draw_with_frame(f, &mut app, &frame_ctx)
        })?;
    }

    let bench_time = bench_start.elapsed();
    let avg_frame = bench_time.as_micros() as f64 / frames as f64;

    eprintln!("\nResults:");
    eprintln!("  Total time:     {:?}", bench_time);
    eprintln!("  Frames:         {}", frames);
    eprintln!("  Avg frame:      {:.1} µs", avg_frame);
    eprintln!("  Throughput:     {:.0} fps", 1_000_000.0 / avg_frame);

    Ok(())
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

fn spawn_fetch(repo_path: PathBuf, base_branch: String, git_version: git::GitVersion, fetch_tx: mpsc::Sender<FetchResult>) {
    thread::spawn(move || {
        if git::fetch_base_branch(&repo_path, &base_branch).is_ok() {
            let has_conflicts = git::has_merge_conflicts(&repo_path, &base_branch, &git_version).unwrap_or(false);
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
    watcher: &mut (impl notify::Watcher + ?Sized),
    file_events: mpsc::Receiver<Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>>,
    refresh_tx: mpsc::Sender<RefreshOutcome>,
    refresh_rx: mpsc::Receiver<RefreshOutcome>,
    repo_root: PathBuf,
    config: UpdateConfig,
    git_version: git::GitVersion,
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
        for msg in &messages {
            // Watch any newly created directories
            if let Message::FileChanged(events) = msg {
                watch_new_directories(watcher, &repo_root, events)?;
            }
        }

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

            match result.refresh {
                RefreshTrigger::Full => {
                    let cancel_flag = refresh_state.start();
                    spawn_refresh(
                        repo_root.clone(),
                        app.base_branch.clone(),
                        refresh_tx.clone(),
                        cancel_flag,
                    );
                }
                RefreshTrigger::SingleFile(file_path) => {
                    refresh_state.start_single_file();
                    spawn_single_file_refresh(
                        repo_root.clone(),
                        file_path.to_string_lossy().to_string(),
                        app.merge_base.clone(),
                        refresh_tx.clone(),
                    );
                }
                RefreshTrigger::None => {}
            }

            if result.trigger_fetch {
                spawn_fetch(repo_root.clone(), app.base_branch.clone(), git_version, fetch_tx.clone());
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
    if let Ok(Ok(events)) = file_events.try_recv()
        && !events.is_empty()
    {
        messages.push(Message::FileChanged(events));
    }

    // Check for completed fetch results
    if let Ok(result) = fetch_rx.try_recv() {
        messages.push(Message::FetchCompleted(result));
    }

    // Always send a tick for timer-based operations
    messages.push(Message::Tick);

    Ok(messages)
}

/// Setup file watcher to only watch non-ignored directories.
///
/// Uses `ignore::WalkBuilder` to respect .gitignore rules, avoiding watches
/// on large ignored directories like `target/` or `node_modules/`.
///
/// Returns metrics about how many directories were found and watched.
fn setup_watcher(
    watcher: &mut (impl notify::Watcher + ?Sized),
    repo_root: &Path,
    limits: &limits::SystemLimits,
) -> Result<limits::WatcherMetrics> {
    let mut metrics = limits::WatcherMetrics::default();
    let mut watches_added = 0;

    // Watch specific .git paths we care about (for detecting commits, branch switches)
    let git_dir = repo_root.join(".git");
    if git_dir.exists() {
        // Watch index for staging changes
        let index = git_dir.join("index");
        if index.exists() {
            watcher.watch(&index, NonRecursive)?;
        }
        // Watch HEAD for branch switches
        let head = git_dir.join("HEAD");
        if head.exists() {
            watcher.watch(&head, NonRecursive)?;
        }
        // Watch refs for branch updates
        let refs = git_dir.join("refs");
        if refs.exists() {
            watcher.watch(&refs, Recursive)?;
        }
    }

    // Walk non-ignored directories and watch each one (up to the limit)
    for entry in WalkBuilder::new(repo_root)
        .hidden(false) // Don't skip hidden files (but .git is handled separately)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|e| {
            // Skip .git directory (handled above)
            e.file_name() != ".git"
        })
        .build()
        .flatten()
    {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            metrics.directory_count += 1;

            // Stop adding watches if we've hit the limit
            if watches_added >= limits.max_recommended_watches {
                metrics.skipped_count += 1;
                continue;
            }

            if watcher.watch(entry.path(), NonRecursive).is_ok() {
                watches_added += 1;
            } else {
                metrics.skipped_count += 1;
            }
        }
    }

    Ok(metrics)
}

/// Add watches for any newly created directories in file change events.
///
/// When a new directory is created in the repo, we need to watch it to detect
/// file changes. This function checks each event path and adds watches for
/// new directories that aren't gitignored.
///
/// Note: Deleted directories are handled automatically by notify - the watch
/// becomes invalid when the directory is removed.
fn watch_new_directories(
    watcher: &mut (impl notify::Watcher + ?Sized),
    repo_root: &Path,
    events: &[notify_debouncer_mini::DebouncedEvent],
) -> Result<()> {
    for event in events {
        let path = &event.path;

        // Only care about directories that currently exist
        if !path.is_dir() {
            continue;
        }

        // Must be under repo_root
        if !path.starts_with(repo_root) {
            continue;
        }

        // Skip anything inside .git
        if let Ok(relative) = path.strip_prefix(repo_root)
            && relative.components().any(|c| c.as_os_str() == ".git")
        {
            continue;
        }

        // Check if this directory should be watched (respects gitignore)
        if is_directory_watchable(path) {
            watcher.watch(path, NonRecursive)?;
        }
    }

    Ok(())
}

/// Check if a directory should be watched by verifying it's not gitignored.
///
/// Uses WalkBuilder on the parent directory to check if the target would be
/// included when respecting gitignore rules.
fn is_directory_watchable(dir_path: &Path) -> bool {
    let parent = match dir_path.parent() {
        Some(p) => p,
        None => return false,
    };

    // Walk the parent with depth 1 and see if our directory is yielded
    WalkBuilder::new(parent)
        .max_depth(Some(1))
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build()
        .flatten()
        .any(|entry| entry.path() == dir_path)
}
