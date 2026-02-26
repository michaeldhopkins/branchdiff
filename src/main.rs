// Lint configuration for code quality
#![warn(
    clippy::unwrap_used,        // Require .expect() over .unwrap()
    clippy::redundant_clone,    // Catch unnecessary clones
    clippy::too_many_lines,     // Flag long functions (configured in clippy.toml)
    clippy::excessive_nesting,  // Flag deeply nested code
)]

mod print;

use branchdiff::app::{self, App, FrameContext};
use branchdiff::file_events::VcsLockState;
#[cfg(target_os = "linux")]
use branchdiff::gitignore::GitignoreFilter;
use branchdiff::input::{handle_event, AppAction};
use branchdiff::limits;
use branchdiff::message::{
    FetchResult, LoopAction, Message, RefreshOutcome, RefreshTrigger, FALLBACK_REFRESH_SECS,
};
use branchdiff::update::{update, RefreshState, Timers, UpdateConfig};
use branchdiff::vcs::{self, ComparisonContext, Vcs};
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
#[cfg(target_os = "linux")]
use ignore::WalkBuilder;
use notify::RecursiveMode::{NonRecursive, Recursive};
use notify::{PollWatcher, RecommendedWatcher};
use notify_debouncer_mini::{new_debouncer_opt, Config as DebouncerConfig, Debouncer};
use ratatui::prelude::*;

/// Output mode for the application
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum OutputMode {
    /// Interactive TUI mode (default)
    #[default]
    Tui,
    /// Print branchdiff format to stdout
    Print,
    /// Output git patch format to stdout
    Diff,
}

/// Output mode arguments (flattened into Cli to avoid excessive bools)
#[derive(clap::Args)]
struct OutputArgs {
    /// Print diff to stdout and exit (non-interactive mode)
    #[arg(short = 'p', long = "print", conflicts_with = "diff")]
    print: bool,

    /// Output unified patch format to stdout (for use with git apply / patch)
    #[arg(short = 'd', long = "diff", conflicts_with = "print")]
    diff: bool,
}

impl OutputArgs {
    fn mode(&self) -> OutputMode {
        if self.print {
            OutputMode::Print
        } else if self.diff {
            OutputMode::Diff
        } else {
            OutputMode::Tui
        }
    }
}

#[derive(Parser)]
#[command(name = "branchdiff")]
#[command(about = "Terminal UI showing unified diff of current branch vs its base")]
#[command(version)]
struct Cli {
    /// Path to repository (default: current directory)
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Disable automatic fetching of base branch
    #[arg(long)]
    no_auto_fetch: bool,

    #[command(flatten)]
    output: OutputArgs,

    /// Run stress test for profiling (renders N frames with simulated input)
    #[arg(long, value_name = "FRAMES")]
    benchmark: Option<usize>,
}

/// Wrapper enum to hold either watcher type while keeping the debouncer alive.
enum AnyDebouncer {
    Recommended(Debouncer<RecommendedWatcher>),
    Poll(Debouncer<PollWatcher>),
}

impl AnyDebouncer {
    fn watcher(&mut self) -> &mut dyn notify::Watcher {
        match self {
            Self::Recommended(d) => d.watcher(),
            Self::Poll(d) => d.watcher(),
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let repo_path = cli
        .path
        .canonicalize()
        .context("Failed to resolve repository path")?;

    // Try to detect VCS - for non-TUI modes, fail immediately if not found
    let detected = match vcs::detect(&repo_path) {
        Ok(vcs) => Some(vcs),
        Err(_) => {
            if cli.output.mode() != OutputMode::Tui {
                anyhow::bail!("Not a git or jj repository");
            }
            None
        }
    };

    // Non-interactive modes (detected is always Some here due to bail above)
    if let Some(vcs) = &detected {
        match cli.output.mode() {
            OutputMode::Print => {
                let repo_root = vcs.repo_path().to_path_buf();
                let comparison = vcs.comparison_context()?;
                let cancel_flag = Arc::new(AtomicBool::new(false));
                let initial = vcs.refresh(&cancel_flag)?;
                let mut app = app::App::new(repo_root, comparison, initial);
                app.view.collapsed_files.clear();
                app.view.view_mode = app::ViewMode::Full;

                for line in &mut app.lines {
                    if line.old_content.is_some() {
                        line.ensure_inline_spans();
                    }
                }
                print::print_diff(&app)?;
                return Ok(());
            }
            OutputMode::Diff => {
                let repo_root = vcs.repo_path().to_path_buf();
                let comparison = vcs.comparison_context()?;
                let cancel_flag = Arc::new(AtomicBool::new(false));
                let initial = vcs.refresh(&cancel_flag)?;
                let app = app::App::new(repo_root, comparison, initial);
                let patch = branchdiff::patch::generate_patch(&app.lines);
                print!("{}", patch);
                return Ok(());
            }
            OutputMode::Tui => {}
        }
    }

    // TUI mode
    match detected {
        Some(vcs) => {
            let repo_root = vcs.repo_path().to_path_buf();
            if let Some(frames) = cli.benchmark {
                return run_benchmark(vcs, repo_root, frames);
            }
            run_main_app(vcs, repo_root, !cli.no_auto_fetch)
        }
        None => run_waiting_for_vcs(&repo_path, !cli.no_auto_fetch),
    }
}

/// Run in "waiting for VCS" mode until a repository is detected.
///
/// Displays a message and periodically checks if a VCS was initialized.
/// When detected, transitions to normal app operation.
fn run_waiting_for_vcs(path: &Path, auto_fetch: bool) -> Result<()> {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::widgets::{Block, Borders, Paragraph};
    use ratatui::layout::Alignment;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let check_interval = Duration::from_secs(1);
    let mut last_check = Instant::now();

    loop {
        terminal.draw(|f| {
            let area = f.area();
            let message = Paragraph::new("Not a repository.\n\nWaiting for git init or jj init...")
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::NONE));

            let y = area.height / 2;
            let centered_area = ratatui::layout::Rect {
                x: 0,
                y: y.saturating_sub(2),
                width: area.width,
                height: 4,
            };
            f.render_widget(message, centered_area);
        })?;

        if event::poll(Duration::from_millis(100))?
            && let crossterm::event::Event::Key(KeyEvent { code, modifiers, .. }) = event::read()?
        {
            match (code, modifiers) {
                (KeyCode::Char('q'), _)
                | (KeyCode::Char('c'), KeyModifiers::CONTROL)
                | (KeyCode::Esc, _) => {
                    disable_raw_mode()?;
                    execute!(
                        terminal.backend_mut(),
                        LeaveAlternateScreen,
                        DisableMouseCapture
                    )?;
                    terminal.show_cursor()?;
                    return Ok(());
                }
                _ => {}
            }
        }

        if last_check.elapsed() >= check_interval {
            last_check = Instant::now();
            if let Ok(detected) = vcs::detect(path) {
                let repo_root = detected.repo_path().to_path_buf();
                disable_raw_mode()?;
                execute!(
                    terminal.backend_mut(),
                    LeaveAlternateScreen,
                    DisableMouseCapture
                )?;
                terminal.show_cursor()?;
                return run_main_app(detected, repo_root, auto_fetch);
            }
        }
    }
}

/// Main app logic, extracted for reuse after VCS detection.
fn run_main_app(
    mut detected: Box<dyn Vcs>,
    mut repo_root: PathBuf,
    auto_fetch: bool,
) -> Result<()> {
    // Initialize image protocol picker (once — survives restarts)
    let in_multiplexer = std::env::var("ZELLIJ").is_ok()
        || std::env::var("TMUX").is_ok()
        || std::env::var("STY").is_ok();

    let mut image_picker = if in_multiplexer {
        ratatui_image::picker::Picker::halfblocks()
    } else {
        ratatui_image::picker::Picker::from_query_stdio()
            .unwrap_or_else(|_| ratatui_image::picker::Picker::halfblocks())
    };
    image_picker.set_background_color(image::Rgba([30, 30, 30, 255]));

    // Setup terminal (once — survives restarts)
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let watch_limit = limits::get_watch_limit();

    loop {
        let vcs: Arc<dyn Vcs> = Arc::from(detected);

        // Fallback labels if jj has a transient error during restart — the
        // first successful refresh will update them via apply_refresh_result.
        let comparison = vcs.comparison_context().unwrap_or_else(|_| ComparisonContext {
            from_label: "base".to_string(),
            to_label: "working copy".to_string(),
            stack_position: None,
            vcs_name: vcs.vcs_name().to_string(),
        });
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let initial = vcs.refresh(&cancel_flag)?;
        let mut app = App::new(repo_root.clone(), comparison, initial);
        app.load_images_for_markers(&*vcs);
        app.set_image_picker(image_picker.clone());

        // Setup file watcher (recreated on restart — old watcher is dropped)
        let (file_tx, file_rx) = mpsc::channel();
        let debouncer_config = DebouncerConfig::default()
            .with_timeout(Duration::from_millis(100))
            .with_notify_config(
                notify::Config::default().with_poll_interval(Duration::from_millis(500)),
            );

        let mut debouncer = if limits::is_wsl() {
            AnyDebouncer::Poll(new_debouncer_opt::<_, PollWatcher>(debouncer_config, file_tx)?)
        } else {
            AnyDebouncer::Recommended(new_debouncer_opt::<_, RecommendedWatcher>(
                debouncer_config,
                file_tx,
            )?)
        };

        let watcher_metrics = setup_watcher(debouncer.watcher(), &*vcs, watch_limit)?;

        let needs_fallback_refresh =
            limits::check_watch_warning(&watcher_metrics, watch_limit).is_some();
        if needs_fallback_refresh {
            app.performance_warning = Some(format!(
                "Large repo: refreshing every {}s",
                FALLBACK_REFRESH_SECS
            ));
        }

        let (refresh_tx, refresh_rx) = mpsc::channel::<RefreshOutcome>();

        let config = UpdateConfig {
            auto_fetch,
            needs_fallback_refresh,
            repo_path: repo_root.clone(),
            ..Default::default()
        };

        let loop_action = run_app(
            &mut terminal,
            &mut app,
            debouncer.watcher(),
            file_rx,
            refresh_tx,
            refresh_rx,
            vcs,
            config,
            watch_limit,
        )?;

        match loop_action {
            LoopAction::Quit => break,
            LoopAction::RestartVcs => {
                match vcs::detect(&repo_root) {
                    Ok(new_vcs) => {
                        repo_root = new_vcs.repo_path().to_path_buf();
                        detected = new_vcs;
                        continue;
                    }
                    Err(_) => break,
                }
            }
            LoopAction::Continue => unreachable!("run_app should not return Continue"),
        }
    }

    // Restore terminal (once)
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}

fn run_benchmark(detected: Box<dyn Vcs>, repo_root: PathBuf, frames: usize) -> Result<()> {
    use ratatui::backend::TestBackend;

    eprintln!("Loading diff from {}...", repo_root.display());
    let load_start = Instant::now();
    let comparison = detected.comparison_context()?;
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let initial = detected.refresh(&cancel_flag)?;
    let mut app = App::new(repo_root, comparison, initial);
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
    app.view.collapsed_files.clear();

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
                let new_offset = (app.view.scroll_offset + n).min(max_scroll);
                app.view.scroll_offset = new_offset;
            }
            AppAction::ScrollUp(n) => {
                app.view.scroll_offset = app.view.scroll_offset.saturating_sub(n);
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

        let items = if app.needs_inline_spans() {
            let items = app.ensure_inline_spans_for_visible(visible_height);
            app.clear_needs_inline_spans();
            Some(items)
        } else {
            None
        };

        terminal.draw(|f| {
            let frame_ctx = match items {
                Some(items) => FrameContext::with_items(items, &app),
                None => FrameContext::new(&app),
            };
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
    vcs: Arc<dyn Vcs>,
    file_path: String,
    refresh_tx: mpsc::Sender<RefreshOutcome>,
) {
    thread::spawn(move || {
        let diff = vcs.single_file_diff(&file_path);
        let revision_id = vcs.current_revision_id().ok();
        let _ = refresh_tx.send(RefreshOutcome::SingleFile { path: file_path, diff, revision_id });
    });
}

fn spawn_refresh(
    vcs: Arc<dyn Vcs>,
    refresh_tx: mpsc::Sender<RefreshOutcome>,
    cancel_flag: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        match vcs.refresh(&cancel_flag) {
            Ok(mut result) => {
                result.revision_id = vcs.current_revision_id().ok();
                let _ = refresh_tx.send(RefreshOutcome::Success(result));
            }
            Err(e) => {
                let outcome = if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                    RefreshOutcome::Cancelled
                } else {
                    RefreshOutcome::Error(format!("{e:#}"))
                };
                let _ = refresh_tx.send(outcome);
            }
        }
    });
}

fn spawn_fetch(vcs: Arc<dyn Vcs>, fetch_tx: mpsc::Sender<FetchResult>) {
    thread::spawn(move || {
        if vcs.fetch().is_ok() {
            let has_conflicts = vcs.has_conflicts().unwrap_or(false);
            let new_merge_base = vcs.base_identifier().ok();

            let _ = fetch_tx.send(FetchResult {
                has_conflicts,
                new_merge_base,
            });
        }
    });
}

#[allow(unused_variables)] // watcher and watch_limit only used on Linux
fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    watcher: &mut (impl notify::Watcher + ?Sized),
    file_events: mpsc::Receiver<Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>>,
    refresh_tx: mpsc::Sender<RefreshOutcome>,
    refresh_rx: mpsc::Receiver<RefreshOutcome>,
    vcs: Arc<dyn Vcs>,
    config: UpdateConfig,
    watch_limit: Option<usize>,
) -> Result<LoopAction>
where
    B::Error: Send + Sync + 'static,
{
    let mut refresh_state = RefreshState::Idle;
    let mut vcs_lock = VcsLockState::default();
    let mut timers = Timers::new(config.repo_path.join(".jj").is_dir());

    let (fetch_tx, fetch_rx) = mpsc::channel::<FetchResult>();

    // Draw initial frame before entering event loop
    // Must set viewport_height AND content_width BEFORE creating FrameContext,
    // which snapshots them for visible_range calculation
    let terminal_size = terminal.size()?;
    let status_height = ui::status_bar_height(app, terminal_size.width);
    let content_height = (terminal_size.height - status_height).saturating_sub(2) as usize;
    app.set_viewport_height(content_height);
    app.estimate_content_width(terminal_size.width);
    let items = app.ensure_inline_spans_for_visible(content_height);
    app.clear_needs_inline_spans();
    terminal.draw(|f| {
        let frame_ctx = FrameContext::with_items(items, app);
        ui::draw_with_frame(f, app, &frame_ctx)
    })?;

    loop {
        // Collect messages from all sources
        let messages = collect_messages(
            &file_events,
            &refresh_rx,
            &fetch_rx,
        )?;

        // Process each message
        #[cfg(target_os = "linux")]
        for msg in &messages {
            if let Message::FileChanged(events) = msg {
                // Watch any newly created directories (Linux only - macOS/Windows use recursive)
                watch_new_directories(watcher, vcs.repo_path(), events);

                // When .gitignore changes, add watches for newly visible directories
                let gitignore_changed = events
                    .iter()
                    .any(|e| GitignoreFilter::is_gitignore_file(&e.path));
                if gitignore_changed {
                    add_watches_for_visible_directories(watcher, vcs.repo_path(), watch_limit);
                }
            }
        }

        let mut needs_redraw = false;
        for msg in messages {
            let result = update(
                msg,
                app,
                &mut refresh_state,
                &mut vcs_lock,
                &mut timers,
                &config,
                &*vcs,
            );

            needs_redraw |= result.needs_redraw;

            if result.loop_action == LoopAction::Quit
                || result.loop_action == LoopAction::RestartVcs
            {
                return Ok(result.loop_action);
            }

            match result.refresh {
                RefreshTrigger::Full => {
                    let cancel_flag = refresh_state.start();
                    spawn_refresh(
                        vcs.clone(),
                        refresh_tx.clone(),
                        cancel_flag,
                    );
                }
                RefreshTrigger::SingleFile(file_path) => {
                    refresh_state.start_single_file();
                    spawn_single_file_refresh(
                        vcs.clone(),
                        file_path.to_string_lossy().to_string(),
                        refresh_tx.clone(),
                    );
                }
                RefreshTrigger::None => {}
            }

            if result.trigger_fetch {
                spawn_fetch(vcs.clone(), fetch_tx.clone());
            }
        }

        // Only render when state has changed
        if needs_redraw {
            let visible_height = terminal.size()?.height as usize;
            // Compute items once, reuse for both inline spans and FrameContext
            let items = if app.needs_inline_spans() {
                let items = app.ensure_inline_spans_for_visible(visible_height);
                app.clear_needs_inline_spans();
                Some(items)
            } else {
                None
            };
            terminal.draw(|f| {
                let frame_ctx = match items {
                    Some(items) => FrameContext::with_items(items, app),
                    None => FrameContext::new(app),
                };
                ui::draw_with_frame(f, app, &frame_ctx)
            })?;
        }
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
        messages.push(Message::RefreshCompleted(Box::new(outcome)));
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

/// Setup file watcher with platform-appropriate strategy.
///
/// On macOS and Windows, uses native recursive watching on the repo root.
/// On Linux, watches each non-ignored directory individually (respecting .gitignore).
///
/// Returns metrics about directories watched (meaningful on Linux only).
fn setup_watcher(
    watcher: &mut (impl notify::Watcher + ?Sized),
    vcs: &dyn Vcs,
    watch_limit: Option<usize>,
) -> Result<limits::WatcherMetrics> {
    // Watch VCS-specific paths (e.g., .git/index, .git/HEAD, .git/refs/)
    setup_vcs_watches(watcher, vcs)?;

    let repo_root = vcs.repo_path();

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        // Native recursive watching - efficient, 1 watch for entire tree.
        // Events for gitignored files are filtered in handle_file_change().
        let _ = watch_limit; // unused on these platforms
        watcher.watch(repo_root, Recursive)?;
        Ok(limits::WatcherMetrics::default())
    }

    #[cfg(target_os = "linux")]
    {
        // Linux inotify: notify-rs creates 1 watch per directory.
        // We walk with gitignore to avoid watching node_modules, target, etc.
        // This is the approach recommended by notify-rs maintainers.
        setup_linux_watches(watcher, repo_root, watch_limit)
    }

    // Other Unix platforms (FreeBSD, etc.) - use recursive as default
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "linux")))]
    {
        let _ = watch_limit;
        watcher.watch(repo_root, Recursive)?;
        Ok(limits::WatcherMetrics::default())
    }
}

/// Watch VCS-specific paths for detecting commits, branch switches, etc.
fn setup_vcs_watches(
    watcher: &mut (impl notify::Watcher + ?Sized),
    vcs: &dyn Vcs,
) -> Result<()> {
    let watch_paths = vcs.watch_paths();
    for file in &watch_paths.files {
        if file.exists() {
            watcher.watch(file, NonRecursive)?;
        }
    }
    for dir in &watch_paths.recursive_dirs {
        if dir.exists() {
            watcher.watch(dir, Recursive)?;
        }
    }
    Ok(())
}

/// Linux-specific: Watch non-ignored directories individually.
///
/// Uses `ignore::WalkBuilder` to respect .gitignore rules, avoiding watches
/// on large ignored directories like `target/` or `node_modules/`.
#[cfg(target_os = "linux")]
fn setup_linux_watches(
    watcher: &mut (impl notify::Watcher + ?Sized),
    repo_root: &Path,
    watch_limit: Option<usize>,
) -> Result<limits::WatcherMetrics> {
    let mut metrics = limits::WatcherMetrics::default();
    let mut watches_added = 0;
    let limit = watch_limit.unwrap_or(usize::MAX);

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
            if watches_added >= limit {
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
///
/// Linux only - macOS/Windows use recursive watching which handles this automatically.
#[cfg(target_os = "linux")]
fn watch_new_directories(
    watcher: &mut (impl notify::Watcher + ?Sized),
    repo_root: &Path,
    events: &[notify_debouncer_mini::DebouncedEvent],
) {
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
        // Ignore errors - directory may already be watched or was deleted between check and watch
        if is_directory_watchable(path) {
            let _ = watcher.watch(path, NonRecursive);
        }
    }
}

/// Check if a directory should be watched by verifying it's not gitignored.
///
/// Uses WalkBuilder on the parent directory to check if the target would be
/// included when respecting gitignore rules.
///
/// Linux only - used by watch_new_directories.
#[cfg(target_os = "linux")]
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

/// Re-walk the repository and add watches for any visible directories.
///
/// Called when .gitignore changes - directories that were previously ignored
/// may now be visible and need watches. Already-watched directories will
/// return an error that we ignore.
///
/// Respects watch_limit to avoid exceeding kernel inotify limits.
///
/// Linux only - macOS/Windows use recursive watching.
#[cfg(target_os = "linux")]
fn add_watches_for_visible_directories(
    watcher: &mut (impl notify::Watcher + ?Sized),
    repo_root: &Path,
    watch_limit: Option<usize>,
) {
    let limit = watch_limit.unwrap_or(usize::MAX);
    let mut watches_added = 0;

    for entry in WalkBuilder::new(repo_root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
        .flatten()
    {
        if watches_added >= limit {
            break;
        }

        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            // Ignore errors - directory may already be watched or was deleted
            if watcher.watch(entry.path(), NonRecursive).is_ok() {
                watches_added += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use tempfile::TempDir;

    // =========================================================================
    // Linux-specific watch limit tests
    // =========================================================================

    #[test]
    #[cfg(target_os = "linux")]
    fn test_setup_linux_watches_respects_limit() {
        use std::fs;

        // Given: a temp repo with 10 subdirectories
        let temp_dir = TempDir::new().unwrap();
        let repo_root = temp_dir.path();
        fs::create_dir(repo_root.join(".git")).unwrap();
        for i in 0..10 {
            fs::create_dir(repo_root.join(format!("dir{}", i))).unwrap();
        }

        let (tx, _rx) = mpsc::channel();
        let config = DebouncerConfig::default()
            .with_timeout(Duration::from_millis(100))
            .with_notify_config(
                notify::Config::default().with_poll_interval(Duration::from_millis(500)),
            );
        let mut debouncer =
            AnyDebouncer::Recommended(new_debouncer_opt::<_, RecommendedWatcher>(config, tx).unwrap());

        // When: we setup watches with a limit of 5
        let metrics = setup_linux_watches(debouncer.watcher(), repo_root, Some(5)).unwrap();

        // Then: we should have counted all directories but only watched up to limit
        // directory_count includes root (1) + 10 subdirs = 11
        assert!(metrics.directory_count >= 10);
        assert!(metrics.skipped_count >= 5, "Expected at least 5 skipped, got {}", metrics.skipped_count);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_add_watches_for_visible_directories_respects_limit() {
        use std::fs;

        // Given: a temp repo with 10 subdirectories
        let temp_dir = TempDir::new().unwrap();
        let repo_root = temp_dir.path();
        fs::create_dir(repo_root.join(".git")).unwrap();
        for i in 0..10 {
            fs::create_dir(repo_root.join(format!("dir{}", i))).unwrap();
        }

        let (tx, _rx) = mpsc::channel();
        let config = DebouncerConfig::default()
            .with_timeout(Duration::from_millis(100))
            .with_notify_config(
                notify::Config::default().with_poll_interval(Duration::from_millis(500)),
            );
        let mut debouncer =
            AnyDebouncer::Recommended(new_debouncer_opt::<_, RecommendedWatcher>(config, tx).unwrap());

        // When: we add watches with a limit of 3
        // Then: the function should complete without panic (limit is enforced)
        // Note: We can't directly count watches added, but setup_linux_watches
        // tests verify the limit logic which add_watches_for_visible_directories shares
        add_watches_for_visible_directories(debouncer.watcher(), repo_root, Some(3));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_add_watches_for_visible_directories_no_limit() {
        use std::fs;

        // Given: a temp repo with 5 subdirectories
        let temp_dir = TempDir::new().unwrap();
        let repo_root = temp_dir.path();
        fs::create_dir(repo_root.join(".git")).unwrap();
        for i in 0..5 {
            fs::create_dir(repo_root.join(format!("dir{}", i))).unwrap();
        }

        let (tx, _rx) = mpsc::channel();
        let config = DebouncerConfig::default()
            .with_timeout(Duration::from_millis(100))
            .with_notify_config(
                notify::Config::default().with_poll_interval(Duration::from_millis(500)),
            );
        let mut debouncer =
            AnyDebouncer::Recommended(new_debouncer_opt::<_, RecommendedWatcher>(config, tx).unwrap());

        // When: we add watches with no limit (None)
        // Then: function should complete without panic
        add_watches_for_visible_directories(debouncer.watcher(), repo_root, None);
    }

    // =========================================================================
    // Debouncer tests (all platforms)
    // =========================================================================

    #[test]
    fn test_any_debouncer_poll_variant_creates_working_watcher() {
        // Given: a channel for file events and a temp directory to watch
        let (tx, _rx) = mpsc::channel();
        let temp_dir = TempDir::new().unwrap();

        let config = DebouncerConfig::default()
            .with_timeout(Duration::from_millis(100))
            .with_notify_config(
                notify::Config::default().with_poll_interval(Duration::from_millis(500)),
            );

        // When: we create a Poll variant debouncer
        let mut debouncer =
            AnyDebouncer::Poll(new_debouncer_opt::<_, PollWatcher>(config, tx).unwrap());

        // Then: we can watch a directory through the trait object
        let result = debouncer.watcher().watch(temp_dir.path(), NonRecursive);
        assert!(result.is_ok());
    }

    #[test]
    fn test_any_debouncer_recommended_variant_creates_working_watcher() {
        // Given: a channel for file events and a temp directory to watch
        let (tx, _rx) = mpsc::channel();
        let temp_dir = TempDir::new().unwrap();

        let config = DebouncerConfig::default()
            .with_timeout(Duration::from_millis(100))
            .with_notify_config(
                notify::Config::default().with_poll_interval(Duration::from_millis(500)),
            );

        // When: we create a Recommended variant debouncer
        let mut debouncer = AnyDebouncer::Recommended(
            new_debouncer_opt::<_, RecommendedWatcher>(config, tx).unwrap(),
        );

        // Then: we can watch a directory through the trait object
        let result = debouncer.watcher().watch(temp_dir.path(), NonRecursive);
        assert!(result.is_ok());
    }
}
