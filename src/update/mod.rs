//! Message processing and state updates.
//!
//! Central location for all state transitions. Each handler function is pure
//! in the sense that it only reads/modifies the state passed to it and returns
//! an UpdateResult indicating side effects to perform.

mod file_change;
mod input;
pub mod recovery;
mod refresh;
mod search;

pub use recovery::{classify_error, ErrorClass, RecoveryAction, RecoveryHint};

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::app::App;
use crate::file_events::VcsLockState;
use crate::limits::DiffThresholds;
use crate::message::{Message, UpdateResult, FALLBACK_REFRESH_SECS};
use crate::vcs::Vcs;

/// Timer state for periodic operations.
pub struct Timers {
    pub last_refresh: Instant,
    pub last_fetch: Instant,
    pub fetch_in_progress: bool,
    /// Timestamp of last VCS internal event for delayed processing
    pub pending_vcs_event: Option<Instant>,
    /// Timestamp of last VCS backend detection check
    pub last_vcs_check: Instant,
    /// Whether .jj directory existed at last check
    pub jj_present: bool,
    /// When the last refresh completed (for suppressing self-triggered events)
    pub last_refresh_completed: Option<Instant>,
    /// Working revision ID from the last completed refresh (for staleness checks)
    pub last_known_revision: Option<String>,
    /// When to fire the next transient-error retry (None = no retry scheduled)
    pub transient_retry_at: Option<Instant>,
    /// Current backoff exponent for transient retries
    pub transient_retry_attempt: u32,
}

impl Timers {
    pub fn new(jj_present: bool) -> Self {
        Self {
            last_refresh: Instant::now(),
            last_fetch: Instant::now(),
            fetch_in_progress: false,
            pending_vcs_event: None,
            last_vcs_check: Instant::now(),
            jj_present,
            last_refresh_completed: None,
            last_known_revision: None,
            transient_retry_at: None,
            transient_retry_attempt: 0,
        }
    }
}

impl Default for Timers {
    fn default() -> Self {
        Self::new(false)
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

    /// Signal the in-flight refresh to abort but leave the state machine alone.
    ///
    /// The point of the watchdog: nudge a slow refresh to bail without spawning
    /// a replacement on top of it. If we changed state to Idle here, a fresh
    /// trigger could spawn a second refresh while the cancelled-but-still-alive
    /// thread keeps burning CPU — exactly the stacking bug we're escaping.
    /// Holding state until the thread actually reports its outcome means the
    /// next trigger is queued via `mark_pending` and only fires after cleanup.
    pub fn signal_cancel(&self) {
        match self {
            RefreshState::InProgress { cancel_flag, .. }
            | RefreshState::InProgressPending { cancel_flag, .. } => {
                cancel_flag.store(true, Ordering::Relaxed);
            }
            RefreshState::Idle => {}
        }
    }

    pub fn start(&mut self) -> Arc<AtomicBool> {
        match self {
            RefreshState::InProgress { cancel_flag, .. }
            | RefreshState::InProgressPending { cancel_flag, .. } => {
                cancel_flag.store(true, Ordering::Relaxed);
            }
            RefreshState::Idle => {}
        }
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
    /// Repository root path (for VCS backend detection)
    pub repo_path: PathBuf,
}

/// Default time before the watchdog signals an in-flight refresh to abort.
pub const DEFAULT_WATCHDOG_TIMEOUT: Duration = Duration::from_secs(10);

/// Env var that overrides `DEFAULT_WATCHDOG_TIMEOUT` (whole seconds).
/// Useful in giant repos where a legitimate refresh exceeds 10s.
pub const WATCHDOG_TIMEOUT_ENV: &str = "BRANCHDIFF_WATCHDOG_TIMEOUT_SECS";

/// Parse a watchdog-timeout env value, falling back to the default on missing
/// or unparseable input. Split out from the `std::env::var` reader so tests
/// can exercise the parsing without touching process-global env state.
pub fn parse_watchdog_timeout(value: Option<&str>) -> Duration {
    value
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_WATCHDOG_TIMEOUT)
}

/// Read the watchdog-timeout override from the environment.
pub fn watchdog_timeout_from_env() -> Duration {
    parse_watchdog_timeout(std::env::var(WATCHDOG_TIMEOUT_ENV).ok().as_deref())
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            fetch_interval: Duration::from_secs(30),
            refresh_fallback_interval: Duration::from_secs(FALLBACK_REFRESH_SECS),
            refresh_watchdog_timeout: DEFAULT_WATCHDOG_TIMEOUT,
            auto_fetch: true,
            diff_thresholds: DiffThresholds::default(),
            needs_fallback_refresh: false,
            repo_path: PathBuf::new(),
        }
    }
}

/// Process a message and update application state.
pub fn update(
    msg: Message,
    app: &mut App,
    refresh_state: &mut RefreshState,
    vcs_lock: &mut VcsLockState,
    timers: &mut Timers,
    config: &UpdateConfig,
    vcs: &dyn Vcs,
) -> UpdateResult {
    match msg {
        Message::Input(action) => input::handle_input(action, app, refresh_state),
        Message::SearchInput(event) => search::handle_search_input(event, app),
        Message::RefreshCompleted(outcome) => {
            let outcome = *outcome;
            refresh::handle_refresh(outcome, app, refresh_state, timers, config, vcs)
        }
        Message::FileChanged(events) => {
            file_change::handle_file_change(events, app, refresh_state, vcs_lock, timers, vcs)
        }
        Message::FetchCompleted(result) => {
            refresh::handle_fetch(result, app, refresh_state, timers)
        }
        Message::Tick => {
            let mut result = refresh::handle_tick(refresh_state, timers, config);
            if app.check_and_execute_pending_copy() {
                result.needs_redraw = true;
            }
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn test_parse_watchdog_timeout_uses_default_on_missing() {
        assert_eq!(parse_watchdog_timeout(None), DEFAULT_WATCHDOG_TIMEOUT);
    }

    #[test]
    fn test_parse_watchdog_timeout_uses_default_on_unparseable() {
        assert_eq!(parse_watchdog_timeout(Some("")), DEFAULT_WATCHDOG_TIMEOUT);
        assert_eq!(parse_watchdog_timeout(Some("abc")), DEFAULT_WATCHDOG_TIMEOUT);
        assert_eq!(parse_watchdog_timeout(Some("-5")), DEFAULT_WATCHDOG_TIMEOUT);
        assert_eq!(parse_watchdog_timeout(Some("1.5")), DEFAULT_WATCHDOG_TIMEOUT);
    }

    #[test]
    fn test_parse_watchdog_timeout_accepts_valid_seconds() {
        assert_eq!(parse_watchdog_timeout(Some("30")), Duration::from_secs(30));
        assert_eq!(parse_watchdog_timeout(Some("0")), Duration::from_secs(0));
        assert_eq!(parse_watchdog_timeout(Some("3600")), Duration::from_secs(3600));
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
    fn test_start_cancels_existing_in_progress_refresh() {
        let mut state = RefreshState::Idle;
        let old_flag = state.start();
        assert!(!old_flag.load(Ordering::Relaxed));

        let new_flag = state.start();
        assert!(old_flag.load(Ordering::Relaxed), "old cancel flag should be set");
        assert!(!new_flag.load(Ordering::Relaxed), "new cancel flag should be fresh");
    }

    #[test]
    fn test_start_cancels_existing_in_progress_pending_refresh() {
        let mut state = RefreshState::Idle;
        let old_flag = state.start();
        state.mark_pending();
        assert!(state.has_pending());

        let new_flag = state.start();
        assert!(old_flag.load(Ordering::Relaxed), "old cancel flag should be set");
        assert!(!new_flag.load(Ordering::Relaxed), "new cancel flag should be fresh");
        assert!(!state.has_pending());
    }

    #[test]
    fn test_start_single_file_transitions_from_idle() {
        let mut state = RefreshState::Idle;
        state.start_single_file();
        assert!(!state.is_idle());
        assert!(state.started_at().is_some());
        assert!(!state.has_pending());
    }

    #[test]
    fn test_start_single_file_replaces_without_cancelling() {
        let mut state = RefreshState::Idle;
        let old_flag = state.start();
        assert!(!old_flag.load(Ordering::Relaxed));

        state.start_single_file();
        assert!(!old_flag.load(Ordering::Relaxed), "start_single_file should not cancel previous");
        assert!(!state.is_idle());
    }

    #[test]
    fn test_cancel_and_mark_pending_from_idle_is_noop() {
        let mut state = RefreshState::Idle;
        state.cancel_and_mark_pending();
        assert!(state.is_idle());
    }

    #[test]
    fn test_cancel_and_mark_pending_from_in_progress_pending() {
        let mut state = RefreshState::Idle;
        let flag = state.start();
        state.mark_pending();
        assert!(state.has_pending());

        state.cancel_and_mark_pending();
        assert!(flag.load(Ordering::Relaxed), "cancel flag should be set");
        assert!(state.has_pending(), "should remain InProgressPending");
    }

    /// End-to-end through the `update()` boundary: stale error arrives, the
    /// recovery hint appears, user presses 'u', and a follow-up success clears
    /// the banner. This is the recovery loop a user actually sees — the unit
    /// tests for individual handlers don't exercise it together.
    #[test]
    fn end_to_end_stale_recovery_flow_through_update_boundary() {
        use crate::file_events::VcsLockState;
        use crate::input::AppAction;
        use crate::message::{Message, RefreshOutcome};
        use crate::test_support::{base_line, StubVcs, TestAppBuilder};
        use std::path::PathBuf;

        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        // Step 1: stale error arrives.
        let stale = Message::RefreshCompleted(Box::new(RefreshOutcome::Error(
            "The working copy is stale. Hint: Run `jj workspace update-stale`.".to_string(),
        )));
        update(stale, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &config, &vcs);
        assert!(app.pending_recovery.is_some(), "stale should surface recovery hint");
        assert!(
            timers.transient_retry_at.is_none(),
            "stale must not schedule a transient retry"
        );

        // Step 2: user presses 'u' to accept the fix.
        let press = Message::Input(AppAction::RunRecovery);
        let press_result = update(
            press,
            &mut app,
            &mut refresh_state,
            &mut vcs_lock,
            &mut timers,
            &config,
            &vcs,
        );
        assert!(
            press_result.trigger_recovery.is_some(),
            "RunRecovery must escalate to a recovery spawn"
        );
        assert!(app.pending_recovery.is_none(), "hint consumed on accept");

        // Step 3: recovery + follow-up refresh succeed (the spawned thread
        // would post a Success outcome). Banner must clear.
        refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let success = Message::RefreshCompleted(Box::new(RefreshOutcome::success(
            crate::vcs::RefreshResult {
                files: vec![],
                lines: vec![base_line("recovered")],
                base_identifier: "rec".to_string(),
                base_label: None,
                current_branch: Some("main".to_string()),
                metrics: crate::limits::DiffMetrics::default(),
                file_links: std::collections::HashMap::new(),
                stack_position: None,
                bookmark_name: None,
                revision_id: None,
                divergence: None,
            },
        )));
        update(
            success,
            &mut app,
            &mut refresh_state,
            &mut vcs_lock,
            &mut timers,
            &config,
            &vcs,
        );
        assert!(app.error.is_none());
        assert!(app.pending_recovery.is_none());
    }

    /// The "fix it from another terminal" path: stale error is showing, a
    /// file event arrives (the user just ran `jj workspace update-stale` in
    /// another tab), the resulting refresh succeeds — the banner must clear
    /// without the user having to press anything. This is the auto-recovery
    /// flow that the initial-refresh-non-fatal change was designed to enable.
    #[test]
    fn file_event_driven_auto_recovery_after_stale() {
        use crate::file_events::VcsLockState;
        use crate::message::{Message, RefreshOutcome, RefreshTrigger};
        use crate::test_support::{base_line, StubVcs, TestAppBuilder};
        use notify_debouncer_mini::{DebouncedEvent, DebouncedEventKind};
        use std::path::PathBuf;

        let mut app = TestAppBuilder::new().build();
        // Seed state as if a startup or runtime refresh had just failed with stale.
        app.error = Some("The working copy is stale".to_string());
        app.pending_recovery = Some(crate::update::RecoveryHint::jj_update_stale());

        let mut refresh_state = RefreshState::Idle;
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        // Step A: the user runs `jj workspace update-stale` in another tab.
        // That mutates `.git/HEAD` (StubVcs classifies it as RevisionChange),
        // which the file watcher debouncer delivers as a FileChanged message.
        let file_event = Message::FileChanged(vec![DebouncedEvent::new(
            PathBuf::from("/tmp/test/.git/HEAD"),
            DebouncedEventKind::Any,
        )]);
        let r = update(
            file_event,
            &mut app,
            &mut refresh_state,
            &mut vcs_lock,
            &mut timers,
            &config,
            &vcs,
        );
        // The watcher should have asked us to refresh. Without this, no
        // auto-recovery would ever happen — guard against a future change
        // that breaks the wiring.
        assert_eq!(
            r.refresh,
            RefreshTrigger::Full,
            "revision-change event must trigger a refresh"
        );

        // Banner is unchanged yet — the refresh hasn't completed.
        assert!(app.error.is_some());
        assert!(app.pending_recovery.is_some());

        // Step B: simulate the spawned refresh completing successfully (the
        // user's external fix worked). The banner and hint must clear.
        refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let success = Message::RefreshCompleted(Box::new(RefreshOutcome::success(
            crate::vcs::RefreshResult {
                files: vec![],
                lines: vec![base_line("recovered")],
                base_identifier: "rec".to_string(),
                base_label: None,
                current_branch: Some("main".to_string()),
                metrics: crate::limits::DiffMetrics::default(),
                file_links: std::collections::HashMap::new(),
                stack_position: None,
                bookmark_name: None,
                revision_id: None,
                divergence: None,
            },
        )));
        update(
            success,
            &mut app,
            &mut refresh_state,
            &mut vcs_lock,
            &mut timers,
            &config,
            &vcs,
        );
        assert!(app.error.is_none(), "external fix must clear the banner");
        assert!(
            app.pending_recovery.is_none(),
            "external fix must drop the now-stale hint"
        );
    }

    /// Counterpart to the happy path: if the recovery command itself fails,
    /// the new error must replace the old one — and the now-stale hint must
    /// not linger and re-offer the same broken fix.
    #[test]
    fn recovery_failure_clears_hint_and_shows_new_error() {
        use crate::file_events::VcsLockState;
        use crate::message::{Message, RefreshOutcome};
        use crate::test_support::{StubVcs, TestAppBuilder};
        use std::path::PathBuf;

        let mut app = TestAppBuilder::new().build();
        app.error = Some("Running `jj workspace update-stale`...".to_string());
        // pending_recovery was already cleared by RunRecovery; the spawn
        // thread now reports back with a failure.
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let failure = Message::RefreshCompleted(Box::new(RefreshOutcome::Error(
            "Recovery action failed: jj exited with code 2".to_string(),
        )));
        update(
            failure,
            &mut app,
            &mut refresh_state,
            &mut vcs_lock,
            &mut timers,
            &config,
            &vcs,
        );

        assert!(
            app.error.as_ref().unwrap().contains("Recovery action failed"),
            "got: {:?}",
            app.error
        );
        assert!(
            app.pending_recovery.is_none(),
            "a generic failure isn't actionable — don't re-offer the broken fix"
        );
    }
}
