//! Message processing and state updates.
//!
//! Central location for all state transitions. Each handler function is pure
//! in the sense that it only reads/modifies the state passed to it and returns
//! an UpdateResult indicating side effects to perform.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify_debouncer_mini::DebouncedEventKind;

use crate::app::App;
use crate::gitignore::GitignoreFilter;
use crate::input::AppAction;
use crate::limits::DiffThresholds;
use crate::message::{
    FetchResult, LoopAction, Message, RefreshOutcome, RefreshTrigger, UpdateResult,
    FALLBACK_REFRESH_SECS,
};

/// Timer state for periodic operations.
pub struct Timers {
    pub last_refresh: Instant,
    pub last_fetch: Instant,
    pub fetch_in_progress: bool,
}

impl Default for Timers {
    fn default() -> Self {
        Self {
            last_refresh: Instant::now(),
            last_fetch: Instant::now(),
            fetch_in_progress: false,
        }
    }
}

/// State machine for background refresh operations.
pub enum RefreshState {
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
    pub fn is_idle(&self) -> bool {
        matches!(self, RefreshState::Idle)
    }

    pub fn started_at(&self) -> Option<Instant> {
        match self {
            RefreshState::Idle => None,
            RefreshState::InProgress { started_at, .. } => Some(*started_at),
            RefreshState::InProgressPending { started_at, .. } => Some(*started_at),
        }
    }

    pub fn has_pending(&self) -> bool {
        matches!(self, RefreshState::InProgressPending { .. })
    }

    pub fn mark_pending(&mut self) {
        if let RefreshState::InProgress {
            started_at,
            cancel_flag,
        } = self
        {
            *self = RefreshState::InProgressPending {
                started_at: *started_at,
                cancel_flag: cancel_flag.clone(),
            };
        }
    }

    pub fn cancel_and_mark_pending(&mut self) {
        match self {
            RefreshState::InProgress {
                started_at,
                cancel_flag,
            } => {
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

    pub fn start(&mut self) -> Arc<AtomicBool> {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        *self = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: cancel_flag.clone(),
        };
        cancel_flag
    }

    pub fn start_single_file(&mut self) {
        *self = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
    }

    pub fn complete(&mut self) -> bool {
        let had_pending = self.has_pending();
        *self = RefreshState::Idle;
        had_pending
    }
}

/// Configuration for update behavior.
pub struct UpdateConfig {
    pub fetch_interval: Duration,
    pub refresh_fallback_interval: Duration,
    pub refresh_watchdog_timeout: Duration,
    pub auto_fetch: bool,
    pub diff_thresholds: DiffThresholds,
    /// Whether to use fallback periodic refresh (for large repos exceeding file watch limits)
    pub needs_fallback_refresh: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            fetch_interval: Duration::from_secs(30),
            refresh_fallback_interval: Duration::from_secs(FALLBACK_REFRESH_SECS),
            refresh_watchdog_timeout: Duration::from_secs(10),
            auto_fetch: true,
            diff_thresholds: DiffThresholds::default(),
            needs_fallback_refresh: false,
        }
    }
}

/// Process a message and update application state.
pub fn update(
    msg: Message,
    app: &mut App,
    refresh_state: &mut RefreshState,
    timers: &mut Timers,
    config: &UpdateConfig,
    repo_root: &Path,
) -> UpdateResult {
    match msg {
        Message::Input(action) => handle_input(action, app, refresh_state),
        Message::RefreshCompleted(outcome) => {
            handle_refresh(outcome, app, refresh_state, timers, config)
        }
        Message::FileChanged(events) => {
            handle_file_change(events, app, refresh_state, repo_root)
        }
        Message::FetchCompleted(result) => handle_fetch(result, app, refresh_state, timers),
        Message::Tick => handle_tick(refresh_state, timers, config),
    }
}

// Multi-click detection constants
const MULTI_CLICK_MS: u128 = 500;
const POSITION_TOLERANCE: u16 = 2;

/// Determine click count for multi-click detection (double/triple click).
fn detect_click_count(app: &App, x: u16, y: u16) -> u8 {
    if let Some((last_time, last_x, last_y, count)) = app.last_click {
        let elapsed = last_time.elapsed().as_millis();
        let close_enough =
            x.abs_diff(last_x) <= POSITION_TOLERANCE && y.abs_diff(last_y) <= POSITION_TOLERANCE;

        if elapsed < MULTI_CLICK_MS && close_enough {
            return count + 1;
        }
    }
    1
}

/// Handle click actions based on click count (single/double/triple).
fn handle_click(app: &mut App, x: u16, y: u16, click_count: u8) {
    match click_count {
        2 => {
            // Double-click: select word
            if app.get_file_header_at(x, y).is_none() {
                app.select_word_at(x, y);
            }
        }
        3 => {
            // Triple-click: select line
            if app.get_file_header_at(x, y).is_none() {
                app.select_line_at(x, y);
            }
        }
        _ => {
            // Single click (or 4+, which resets to single-click behavior)
            if let Some(file_path) = app.get_file_header_at(x, y) {
                app.toggle_file_collapsed(&file_path);
            } else {
                app.start_selection(x, y);
            }
        }
    }
}

/// Handle navigation actions (scrolling, file navigation).
fn handle_navigation(action: &AppAction, app: &mut App) {
    match action {
        AppAction::ScrollUp(n) => app.scroll_up(*n),
        AppAction::ScrollDown(n) => app.scroll_down(*n),
        AppAction::PageUp => app.page_up(),
        AppAction::PageDown => app.page_down(),
        AppAction::GoToTop => app.go_to_top(),
        AppAction::GoToBottom => app.go_to_bottom(),
        AppAction::NextFile => app.next_file(),
        AppAction::PrevFile => app.prev_file(),
        _ => {}
    }
}

/// Handle clipboard operations.
fn handle_clipboard(action: &AppAction, app: &mut App) -> Option<LoopAction> {
    match action {
        AppAction::Copy => {
            let _ = app.copy_selection();
        }
        AppAction::CopyPath => {
            let _ = app.copy_current_path();
        }
        AppAction::CopyDiff => {
            let _ = app.copy_diff();
        }
        AppAction::CopyPatch => {
            let _ = app.copy_patch();
        }
        AppAction::CopyOrQuit => {
            if app.has_selection() {
                let _ = app.copy_selection();
            } else if app.should_quit() {
                return Some(LoopAction::Quit);
            }
        }
        _ => {}
    }
    None
}

/// Handle user input actions.
fn handle_input(
    action: AppAction,
    app: &mut App,
    refresh_state: &mut RefreshState,
) -> UpdateResult {
    let mut result = UpdateResult {
        needs_redraw: !matches!(action, AppAction::None),
        ..Default::default()
    };

    match &action {
        // Control actions
        AppAction::Quit => {
            if app.should_quit() {
                result.loop_action = LoopAction::Quit;
            }
        }
        AppAction::Refresh => {
            if refresh_state.is_idle() {
                result.refresh = RefreshTrigger::Full;
            } else {
                refresh_state.cancel_and_mark_pending();
            }
        }

        // Navigation actions
        AppAction::ScrollUp(_)
        | AppAction::ScrollDown(_)
        | AppAction::PageUp
        | AppAction::PageDown
        | AppAction::GoToTop
        | AppAction::GoToBottom
        | AppAction::NextFile
        | AppAction::PrevFile => handle_navigation(&action, app),

        // View actions
        AppAction::ToggleHelp => app.toggle_help(),
        AppAction::CycleViewMode => app.cycle_view_mode(),

        // Selection actions
        AppAction::StartSelection(x, y) => {
            let click_count = detect_click_count(app, *x, *y);
            app.last_click = Some((Instant::now(), *x, *y, click_count));
            handle_click(app, *x, *y, click_count);
        }
        AppAction::UpdateSelection(x, y) => {
            app.update_selection(*x, *y);
            app.last_click = None; // Clear to prevent false double-clicks during drag
        }
        AppAction::EndSelection => app.end_selection(),

        // Clipboard actions
        AppAction::Copy
        | AppAction::CopyPath
        | AppAction::CopyDiff
        | AppAction::CopyPatch
        | AppAction::CopyOrQuit => {
            if let Some(loop_action) = handle_clipboard(&action, app) {
                result.loop_action = loop_action;
            }
        }

        // No-op actions
        AppAction::Resize | AppAction::None => {}
    }

    result
}

/// Handle completed refresh operations.
fn handle_refresh(
    outcome: RefreshOutcome,
    app: &mut App,
    refresh_state: &mut RefreshState,
    timers: &mut Timers,
    config: &UpdateConfig,
) -> UpdateResult {
    let mut result = UpdateResult::default();

    match outcome {
        RefreshOutcome::Success(refresh_result) => {
            // Check for diff-related warnings
            let diff_warning = config.diff_thresholds.check_diff_warning(&refresh_result.metrics);

            // Update performance warning (prefer watch warning if set, else use diff warning)
            if app.performance_warning.as_ref().is_some_and(|w| w.contains("repo")) {
                // Keep existing watch warning, but append diff warning if present
                if let Some(dw) = diff_warning {
                    app.performance_warning = Some(format!(
                        "{} | {}",
                        app.performance_warning.as_ref().unwrap(),
                        dw
                    ));
                }
            } else {
                // Set or clear diff warning
                app.performance_warning = diff_warning;
            }

            app.apply_refresh_result(refresh_result);
            timers.last_refresh = Instant::now();
            result.needs_redraw = true;
        }
        RefreshOutcome::SingleFile { path, diff } => {
            app.update_single_file(&path, diff);
            timers.last_refresh = Instant::now();
            result.needs_redraw = true;
        }
        RefreshOutcome::Cancelled => {}
    }

    if refresh_state.complete() {
        result.refresh = RefreshTrigger::Full;
    }

    result
}

/// Git event classification.
enum GitEventType {
    /// .git/index changes
    Index,
    /// .git/HEAD or .git/refs/ changes
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

/// Handle file system change events.
fn handle_file_change(
    events: Vec<notify_debouncer_mini::DebouncedEvent>,
    app: &mut App,
    refresh_state: &mut RefreshState,
    repo_root: &Path,
) -> UpdateResult {
    let mut result = UpdateResult::default();

    // Deduplicate paths before processing (rapid saves can generate multiple events per file)
    let unique_paths: HashSet<_> = events
        .iter()
        .filter(|e| e.kind == DebouncedEventKind::Any)
        .map(|e| &e.path)
        .collect();

    // Rebuild gitignore matcher if any .gitignore file changed
    let gitignore_changed = unique_paths
        .iter()
        .any(|p| GitignoreFilter::is_gitignore_file(p));
    if gitignore_changed {
        app.gitignore_filter.rebuild();
    }

    let filtered_paths: Vec<_> = unique_paths
        .into_iter()
        .filter(|p| !is_noisy_path(&p.to_string_lossy()))
        .filter(|p| !app.gitignore_filter.is_ignored(p))
        .collect();

    let mut should_refresh = false;
    let mut has_git_change = false;
    let mut source_files = Vec::new();

    for path in &filtered_paths {
        let path_str = path.to_string_lossy();
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
                    source_files.push(*path);
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
            let can_use_single_file =
                !has_git_change && source_files.len() == 1 && !app.files.is_empty();

            if can_use_single_file {
                let file_path = source_files[0]
                    .strip_prefix(repo_root)
                    .unwrap_or(source_files[0])
                    .to_string_lossy()
                    .to_string();

                result.refresh = RefreshTrigger::SingleFile(PathBuf::from(file_path));
            } else {
                result.refresh = RefreshTrigger::Full;
            }
        }
    }

    result
}

/// Handle completed fetch operations.
fn handle_fetch(
    fetch_result: FetchResult,
    app: &mut App,
    refresh_state: &mut RefreshState,
    timers: &mut Timers,
) -> UpdateResult {
    let mut result = UpdateResult::default();
    timers.fetch_in_progress = false;

    // Track if conflict warning changed
    let old_conflict = app.conflict_warning.is_some();
    if fetch_result.has_conflicts {
        app.conflict_warning = Some("Merge conflicts detected with remote".to_string());
    } else {
        app.conflict_warning = None;
    }
    let new_conflict = app.conflict_warning.is_some();

    // Redraw if conflict status changed
    if old_conflict != new_conflict {
        result.needs_redraw = true;
    }

    if let Some(new_base) = fetch_result.new_merge_base
        && new_base != app.merge_base
    {
        app.merge_base = new_base;
        if refresh_state.is_idle() {
            result.refresh = RefreshTrigger::Full;
        } else {
            refresh_state.mark_pending();
        }
    }

    result
}

/// Handle periodic tick for timer-based operations.
fn handle_tick(
    refresh_state: &mut RefreshState,
    timers: &mut Timers,
    config: &UpdateConfig,
) -> UpdateResult {
    let mut result = UpdateResult::default();

    // Trigger periodic fetch if enabled
    if config.auto_fetch
        && !timers.fetch_in_progress
        && timers.last_fetch.elapsed() >= config.fetch_interval
    {
        timers.fetch_in_progress = true;
        timers.last_fetch = Instant::now();
        result.trigger_fetch = true;
    }

    // Watchdog: reset stuck refresh
    if let Some(started) = refresh_state.started_at()
        && started.elapsed() >= config.refresh_watchdog_timeout
    {
        result.refresh = RefreshTrigger::Full;
    }

    // Periodic fallback refresh (only when file watching is insufficient)
    if config.needs_fallback_refresh
        && refresh_state.is_idle()
        && timers.last_refresh.elapsed() >= config.refresh_fallback_interval
    {
        result.refresh = RefreshTrigger::Full;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{base_line, TestAppBuilder};

    #[test]
    fn test_handle_input_quit() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::Quit, &mut app, &mut refresh_state);
        assert_eq!(result.loop_action, LoopAction::Quit);
    }

    #[test]
    fn test_handle_input_scroll_down() {
        let lines: Vec<_> = (0..20).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        let mut refresh_state = RefreshState::Idle;

        handle_input(AppAction::ScrollDown(5), &mut app, &mut refresh_state);
        assert_eq!(app.scroll_offset, 5);
    }

    #[test]
    fn test_handle_input_refresh_when_idle() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::Refresh, &mut app, &mut refresh_state);
        assert_eq!(result.refresh, RefreshTrigger::Full);
    }

    #[test]
    fn test_handle_input_refresh_when_busy_marks_pending() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };

        let result = handle_input(AppAction::Refresh, &mut app, &mut refresh_state);
        assert_eq!(result.refresh, RefreshTrigger::None);
        assert!(refresh_state.has_pending());
    }

    #[test]
    fn test_handle_refresh_success() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        timers.last_refresh = Instant::now() - Duration::from_secs(60);

        let outcome = RefreshOutcome::Success(crate::app::RefreshResult {
            files: vec![],
            lines: vec![base_line("new content")],
            merge_base: "def456".to_string(),
            current_branch: Some("feature".to_string()),
            metrics: crate::limits::DiffMetrics::default(),
            file_links: std::collections::HashMap::new(),
        });

        let config = UpdateConfig::default();
        let result = handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config);

        assert_eq!(result.refresh, RefreshTrigger::None);
        assert!(refresh_state.is_idle());
        assert_eq!(app.merge_base, "def456");
        assert!(timers.last_refresh.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn test_handle_refresh_triggers_pending() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgressPending {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();

        let outcome = RefreshOutcome::Cancelled;
        let config = UpdateConfig::default();
        let result = handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config);

        assert_eq!(result.refresh, RefreshTrigger::Full);
        assert!(refresh_state.is_idle());
    }

    #[test]
    fn test_handle_fetch_with_conflicts() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers {
            fetch_in_progress: true,
            ..Default::default()
        };

        let fetch_result = FetchResult {
            has_conflicts: true,
            new_merge_base: None,
        };

        handle_fetch(fetch_result, &mut app, &mut refresh_state, &mut timers);

        assert!(app.conflict_warning.is_some());
        assert!(!timers.fetch_in_progress);
    }

    #[test]
    fn test_handle_fetch_clears_conflicts() {
        let mut app = TestAppBuilder::new().build();
        app.conflict_warning = Some("old warning".to_string());
        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers {
            fetch_in_progress: true,
            ..Default::default()
        };

        let fetch_result = FetchResult {
            has_conflicts: false,
            new_merge_base: None,
        };

        handle_fetch(fetch_result, &mut app, &mut refresh_state, &mut timers);
        assert!(app.conflict_warning.is_none());
    }

    #[test]
    fn test_handle_fetch_new_merge_base_triggers_refresh() {
        let mut app = TestAppBuilder::new().build();
        app.merge_base = "old_base".to_string();
        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers {
            fetch_in_progress: true,
            ..Default::default()
        };

        let fetch_result = FetchResult {
            has_conflicts: false,
            new_merge_base: Some("new_base".to_string()),
        };

        let result = handle_fetch(fetch_result, &mut app, &mut refresh_state, &mut timers);

        assert_eq!(result.refresh, RefreshTrigger::Full);
        assert_eq!(app.merge_base, "new_base");
    }

    #[test]
    fn test_handle_tick_triggers_fetch_after_interval() {
        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers {
            last_fetch: Instant::now() - Duration::from_secs(60),
            fetch_in_progress: false,
            ..Default::default()
        };
        let config = UpdateConfig {
            fetch_interval: Duration::from_secs(30),
            auto_fetch: true,
            ..Default::default()
        };

        let result = handle_tick(&mut refresh_state, &mut timers, &config);

        assert!(result.trigger_fetch);
        assert!(timers.fetch_in_progress);
    }

    #[test]
    fn test_handle_tick_no_fetch_when_disabled() {
        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers {
            last_fetch: Instant::now() - Duration::from_secs(60),
            fetch_in_progress: false,
            ..Default::default()
        };
        let config = UpdateConfig {
            auto_fetch: false,
            ..Default::default()
        };

        let result = handle_tick(&mut refresh_state, &mut timers, &config);
        assert!(!result.trigger_fetch);
    }

    #[test]
    fn test_handle_tick_watchdog_resets_stuck_refresh() {
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now() - Duration::from_secs(15),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        let config = UpdateConfig {
            refresh_watchdog_timeout: Duration::from_secs(10),
            ..Default::default()
        };

        let result = handle_tick(&mut refresh_state, &mut timers, &config);
        assert_eq!(result.refresh, RefreshTrigger::Full);
    }

    #[test]
    fn test_handle_tick_fallback_refresh() {
        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers {
            last_refresh: Instant::now() - Duration::from_secs(FALLBACK_REFRESH_SECS + 5),
            ..Default::default()
        };
        let config = UpdateConfig {
            needs_fallback_refresh: true,
            ..Default::default()
        };

        let result = handle_tick(&mut refresh_state, &mut timers, &config);
        assert_eq!(result.refresh, RefreshTrigger::Full);
    }

    #[test]
    fn test_handle_tick_no_fallback_when_not_needed() {
        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers {
            last_refresh: Instant::now() - Duration::from_secs(FALLBACK_REFRESH_SECS + 5),
            ..Default::default()
        };
        // Default: needs_fallback_refresh = false
        let config = UpdateConfig::default();

        let result = handle_tick(&mut refresh_state, &mut timers, &config);
        // Should NOT trigger fallback when not needed
        assert_eq!(result.refresh, RefreshTrigger::None);
    }

    #[test]
    fn test_refresh_state_lifecycle() {
        let mut state = RefreshState::Idle;
        assert!(state.is_idle());

        let cancel_flag = state.start();
        assert!(!state.is_idle());
        assert!(state.started_at().is_some());

        state.mark_pending();
        assert!(state.has_pending());

        let had_pending = state.complete();
        assert!(had_pending);
        assert!(state.is_idle());

        // Cancel flag should be usable
        assert!(!cancel_flag.load(Ordering::Relaxed));
    }

    #[test]
    fn test_classify_git_event() {
        assert!(matches!(
            classify_git_event("/repo/.git/HEAD"),
            Some(GitEventType::BranchSwitch)
        ));
        assert!(matches!(
            classify_git_event("/repo/.git/refs/heads/main"),
            Some(GitEventType::BranchSwitch)
        ));
        assert!(matches!(
            classify_git_event("/repo/.git/index"),
            Some(GitEventType::Index)
        ));
        assert!(classify_git_event("/repo/src/main.rs").is_none());
        assert!(classify_git_event("/repo/.git/objects/ab").is_none());
    }

    #[test]
    fn test_is_noisy_path() {
        assert!(is_noisy_path("/tmp/file.txt"));
        assert!(is_noisy_path("/project/node_modules/pkg/file.js"));
        assert!(is_noisy_path("/project/file.lock"));
        assert!(!is_noisy_path("/project/src/main.rs"));
    }

    #[test]
    fn test_duplicate_file_events_are_deduplicated() {
        use notify_debouncer_mini::DebouncedEvent;

        // Given: 5 events for only 2 unique paths
        let events = vec![
            DebouncedEvent::new(PathBuf::from("/repo/src/main.rs"), DebouncedEventKind::Any),
            DebouncedEvent::new(PathBuf::from("/repo/src/main.rs"), DebouncedEventKind::Any),
            DebouncedEvent::new(PathBuf::from("/repo/src/lib.rs"), DebouncedEventKind::Any),
            DebouncedEvent::new(PathBuf::from("/repo/src/main.rs"), DebouncedEventKind::Any),
            DebouncedEvent::new(PathBuf::from("/repo/src/lib.rs"), DebouncedEventKind::Any),
        ];

        // When: we collect unique paths (same logic as handle_file_change)
        let unique_paths: HashSet<_> = events
            .iter()
            .filter(|e| e.kind == DebouncedEventKind::Any)
            .map(|e| &e.path)
            .collect();

        // Then: 5 events become 2 unique paths
        assert_eq!(unique_paths.len(), 2);
        assert!(unique_paths.contains(&PathBuf::from("/repo/src/main.rs")));
        assert!(unique_paths.contains(&PathBuf::from("/repo/src/lib.rs")));
    }

    // === Tests for needs_redraw flag ===

    #[test]
    fn test_handle_input_sets_needs_redraw_for_scroll() {
        let mut app = TestAppBuilder::new()
            .with_lines(vec![base_line("line")])
            .build();
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::ScrollDown(1), &mut app, &mut refresh_state);
        assert!(result.needs_redraw);
    }

    #[test]
    fn test_handle_input_sets_needs_redraw_for_resize() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::Resize, &mut app, &mut refresh_state);
        assert!(result.needs_redraw);
    }

    #[test]
    fn test_handle_input_no_redraw_for_none_action() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::None, &mut app, &mut refresh_state);
        assert!(!result.needs_redraw);
    }

    #[test]
    fn test_handle_refresh_success_sets_needs_redraw() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        let config = UpdateConfig::default();

        let outcome = RefreshOutcome::Success(crate::app::RefreshResult {
            files: vec![],
            lines: vec![base_line("content")],
            merge_base: "abc".to_string(),
            current_branch: Some("main".to_string()),
            metrics: crate::limits::DiffMetrics::default(),
            file_links: std::collections::HashMap::new(),
        });

        let result = handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config);
        assert!(result.needs_redraw);
    }

    #[test]
    fn test_handle_refresh_single_file_sets_needs_redraw() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        let config = UpdateConfig::default();

        let outcome = RefreshOutcome::SingleFile {
            path: "test.rs".to_string(),
            diff: None,
        };

        let result = handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config);
        assert!(result.needs_redraw);
    }

    #[test]
    fn test_handle_refresh_cancelled_no_redraw() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        let config = UpdateConfig::default();

        let result = handle_refresh(
            RefreshOutcome::Cancelled,
            &mut app,
            &mut refresh_state,
            &mut timers,
            &config,
        );
        assert!(!result.needs_redraw);
    }

    #[test]
    fn test_handle_fetch_conflict_change_sets_needs_redraw() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers {
            fetch_in_progress: true,
            ..Default::default()
        };

        // No conflict -> conflict = change
        let result = handle_fetch(
            FetchResult {
                has_conflicts: true,
                new_merge_base: None,
            },
            &mut app,
            &mut refresh_state,
            &mut timers,
        );
        assert!(result.needs_redraw);
    }

    #[test]
    fn test_handle_fetch_no_change_no_redraw() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers {
            fetch_in_progress: true,
            ..Default::default()
        };

        // No conflict before, no conflict after = no change
        let result = handle_fetch(
            FetchResult {
                has_conflicts: false,
                new_merge_base: None,
            },
            &mut app,
            &mut refresh_state,
            &mut timers,
        );
        assert!(!result.needs_redraw);
    }

    #[test]
    fn test_handle_tick_no_redraw() {
        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers::default();
        let config = UpdateConfig {
            auto_fetch: false,
            ..Default::default()
        };

        let result = handle_tick(&mut refresh_state, &mut timers, &config);
        assert!(!result.needs_redraw);
    }

    #[test]
    fn test_handle_file_change_no_redraw() {
        use notify_debouncer_mini::DebouncedEvent;

        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;
        let repo_root = PathBuf::from("/repo");

        let events = vec![DebouncedEvent::new(
            PathBuf::from("/repo/src/main.rs"),
            DebouncedEventKind::Any,
        )];

        let result = handle_file_change(events, &mut app, &mut refresh_state, &repo_root);
        // File changes trigger background refresh, not immediate redraw
        assert!(!result.needs_redraw);
    }

    #[test]
    fn test_double_click_selects_word() {
        use crate::ui::ScreenRowInfo;

        // With line_num_width=3, prefix_len = 3 + 1 + 4 = 8
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.content_offset = (1, 1);
        app.row_map = vec![ScreenRowInfo {
            content: "hello world".to_string(),
            is_file_header: false,
            file_path: None,
            is_continuation: false,
        }];

        let mut refresh_state = RefreshState::Idle;

        // First click - starts selection
        // Click on 'w' in "world" - content col 6, screen col = 6 + prefix(8) + offset(1) = 15
        handle_input(AppAction::StartSelection(15, 1), &mut app, &mut refresh_state);
        assert!(app.last_click.is_some());
        // Should have started a point selection
        assert!(app.selection.is_some());

        // Second click at same position (simulate double-click by keeping last_click recent)
        // last_click is already set from first click, and time elapsed is negligible
        handle_input(AppAction::StartSelection(15, 1), &mut app, &mut refresh_state);

        // Should have selected the word "world"
        let sel = app.selection.as_ref().expect("Should have selection");
        assert_eq!(sel.start.col, 14); // "world" starts at content col 6 + prefix 8
        assert_eq!(sel.end.col, 19); // "world" ends at content col 11 + prefix 8
    }

    #[test]
    fn test_triple_click_selects_line() {
        use crate::ui::ScreenRowInfo;

        // With line_num_width=3, prefix_len = 3 + 1 + 4 = 8
        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.content_offset = (1, 1);
        app.row_map = vec![ScreenRowInfo {
            content: "hello world".to_string(),
            is_file_header: false,
            file_path: None,
            is_continuation: false,
        }];

        let mut refresh_state = RefreshState::Idle;

        // First click - screen_x = 0 + 8 + 1 = 9
        handle_input(AppAction::StartSelection(10, 1), &mut app, &mut refresh_state);
        // Second click (double-click)
        handle_input(AppAction::StartSelection(10, 1), &mut app, &mut refresh_state);
        // Third click (triple-click)
        handle_input(AppAction::StartSelection(10, 1), &mut app, &mut refresh_state);

        // Should have selected the entire line
        let sel = app.selection.as_ref().expect("Should have selection");
        assert_eq!(sel.start.row, 0);
        assert_eq!(sel.end.row, 0);
        // Line selection starts at prefix_len = 8
        assert_eq!(sel.start.col, 8);
        // Line selection ends at content length + prefix_len (11 + 8 = 19)
        assert_eq!(sel.end.col, 19);
        // Line selection anchor should be set
        assert!(app.line_selection_anchor.is_some());
    }

    #[test]
    fn test_single_click_does_not_select_word() {
        use crate::ui::ScreenRowInfo;

        let mut app = TestAppBuilder::new().build();
        app.line_num_width = 3;
        app.content_offset = (1, 1);
        app.row_map = vec![ScreenRowInfo {
            content: "hello world".to_string(),
            is_file_header: false,
            file_path: None,
            is_continuation: false,
        }];

        let mut refresh_state = RefreshState::Idle;

        // Single click
        handle_input(AppAction::StartSelection(13, 1), &mut app, &mut refresh_state);

        // Should have a point selection, not a word selection
        let sel = app.selection.as_ref().expect("Should have selection");
        // Point selection has start == end (or very close)
        assert_eq!(sel.start.col, sel.end.col);
    }

    #[test]
    fn test_drag_clears_last_click() {
        let mut app = TestAppBuilder::new().build();
        app.last_click = Some((Instant::now(), 10, 10, 1));

        let mut refresh_state = RefreshState::Idle;

        // Drag action should clear last_click
        handle_input(AppAction::UpdateSelection(15, 10), &mut app, &mut refresh_state);

        assert!(app.last_click.is_none());
    }

    #[test]
    fn test_handle_input_copy_patch_sets_needs_redraw() {
        let mut app = TestAppBuilder::new()
            .with_lines(vec![base_line("content")])
            .build();
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::CopyPatch, &mut app, &mut refresh_state);

        // CopyPatch should trigger redraw (to show "Copied" flash)
        assert!(result.needs_redraw);
    }

    #[test]
    fn test_handle_input_copy_diff_sets_needs_redraw() {
        let mut app = TestAppBuilder::new()
            .with_lines(vec![base_line("content")])
            .build();
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::CopyDiff, &mut app, &mut refresh_state);

        assert!(result.needs_redraw);
    }

    #[test]
    fn test_handle_input_copy_path_sets_needs_redraw() {
        let mut app = TestAppBuilder::new()
            .with_lines(vec![base_line("content")])
            .build();
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::CopyPath, &mut app, &mut refresh_state);

        assert!(result.needs_redraw);
    }
}
