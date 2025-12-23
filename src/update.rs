//! Message processing and state updates.
//!
//! Central location for all state transitions. Each handler function is pure
//! in the sense that it only reads/modifies the state passed to it and returns
//! an UpdateResult indicating side effects to perform.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify_debouncer_mini::DebouncedEventKind;

use crate::app::App;
use crate::gitignore::GitignoreFilter;
use crate::input::AppAction;
use crate::limits::DiffThresholds;
use crate::message::{FetchResult, Message, RefreshOutcome, RefreshTrigger, UpdateResult};

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
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            fetch_interval: Duration::from_secs(30),
            refresh_fallback_interval: Duration::from_secs(5),
            refresh_watchdog_timeout: Duration::from_secs(10),
            auto_fetch: true,
            diff_thresholds: DiffThresholds::default(),
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

/// Handle user input actions.
fn handle_input(
    action: AppAction,
    app: &mut App,
    refresh_state: &mut RefreshState,
) -> UpdateResult {
    let mut result = UpdateResult::default();

    match action {
        AppAction::Quit => {
            if app.should_quit() {
                result.quit = true;
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
                result.refresh = RefreshTrigger::Full;
            } else {
                refresh_state.cancel_and_mark_pending();
            }
        }
        AppAction::ToggleHelp => app.toggle_help(),
        AppAction::CycleViewMode => app.cycle_view_mode(),
        AppAction::StartSelection(x, y) => {
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
        AppAction::CopyPath => {
            let _ = app.copy_current_path();
        }
        AppAction::CopyDiff => {
            let _ = app.copy_diff();
        }
        AppAction::CopyOrQuit => {
            if app.has_selection() {
                let _ = app.copy_selection();
            } else if app.should_quit() {
                result.quit = true;
            }
        }
        AppAction::None => {}
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
        }
        RefreshOutcome::SingleFile { path, diff } => {
            app.update_single_file(&path, diff);
            timers.last_refresh = Instant::now();
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

    // Rebuild gitignore matcher if any .gitignore file changed
    let gitignore_changed = events
        .iter()
        .any(|e| GitignoreFilter::is_gitignore_file(&e.path));
    if gitignore_changed {
        app.gitignore_filter.rebuild();
    }

    let dominated_events: Vec<_> = events
        .iter()
        .filter(|e| e.kind == DebouncedEventKind::Any)
        .filter(|e| !is_noisy_path(&e.path.to_string_lossy()))
        .filter(|e| !app.gitignore_filter.is_ignored(&e.path))
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

    if fetch_result.has_conflicts {
        app.conflict_warning = Some("Merge conflicts detected with remote".to_string());
    } else {
        app.conflict_warning = None;
    }

    if let Some(new_base) = fetch_result.new_merge_base {
        if new_base != app.merge_base {
            app.merge_base = new_base;
            if refresh_state.is_idle() {
                result.refresh = RefreshTrigger::Full;
            } else {
                refresh_state.mark_pending();
            }
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
    if let Some(started) = refresh_state.started_at() {
        if started.elapsed() >= config.refresh_watchdog_timeout {
            result.refresh = RefreshTrigger::Full;
        }
    }

    // Periodic fallback refresh
    if refresh_state.is_idle()
        && timers.last_refresh.elapsed() >= config.refresh_fallback_interval
    {
        result.refresh = RefreshTrigger::Full;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ViewMode;
    use crate::diff::{DiffLine, LineSource};
    use std::collections::HashSet;

    fn create_test_app(lines: Vec<DiffLine>) -> App {
        let repo_path = PathBuf::from("/tmp/test");
        App {
            gitignore_filter: GitignoreFilter::new(&repo_path),
            repo_path,
            base_branch: "main".to_string(),
            merge_base: "abc123".to_string(),
            current_branch: Some("feature".to_string()),
            files: Vec::new(),
            lines,
            scroll_offset: 0,
            viewport_height: 10,
            error: None,
            show_help: false,
            view_mode: ViewMode::Full,
            selection: None,
            content_offset: (1, 1),
            line_num_width: 0,
            content_width: 80,
            conflict_warning: None,
            performance_warning: None,
            row_map: Vec::new(),
            collapsed_files: HashSet::new(),
            manually_toggled: HashSet::new(),
            needs_inline_spans: true,
            path_copied_at: None,
        }
    }

    fn base_line(content: &str) -> DiffLine {
        DiffLine::new(LineSource::Base, content.to_string(), ' ', None)
    }

    #[test]
    fn test_handle_input_quit() {
        let mut app = create_test_app(vec![]);
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::Quit, &mut app, &mut refresh_state);
        assert!(result.quit);
    }

    #[test]
    fn test_handle_input_scroll_down() {
        let lines: Vec<_> = (0..20).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = create_test_app(lines);
        let mut refresh_state = RefreshState::Idle;

        handle_input(AppAction::ScrollDown(5), &mut app, &mut refresh_state);
        assert_eq!(app.scroll_offset, 5);
    }

    #[test]
    fn test_handle_input_refresh_when_idle() {
        let mut app = create_test_app(vec![]);
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::Refresh, &mut app, &mut refresh_state);
        assert_eq!(result.refresh, RefreshTrigger::Full);
    }

    #[test]
    fn test_handle_input_refresh_when_busy_marks_pending() {
        let mut app = create_test_app(vec![]);
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
        let mut app = create_test_app(vec![]);
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
        let mut app = create_test_app(vec![]);
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
        let mut app = create_test_app(vec![]);
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
        let mut app = create_test_app(vec![]);
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
        let mut app = create_test_app(vec![]);
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
            last_refresh: Instant::now() - Duration::from_secs(10),
            ..Default::default()
        };
        let config = UpdateConfig {
            refresh_fallback_interval: Duration::from_secs(5),
            ..Default::default()
        };

        let result = handle_tick(&mut refresh_state, &mut timers, &config);
        assert_eq!(result.refresh, RefreshTrigger::Full);
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
}
