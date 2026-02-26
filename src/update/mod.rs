//! Message processing and state updates.
//!
//! Central location for all state transitions. Each handler function is pure
//! in the sense that it only reads/modifies the state passed to it and returns
//! an UpdateResult indicating side effects to perform.

mod file_change;
mod input;
mod refresh;

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

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            fetch_interval: Duration::from_secs(30),
            refresh_fallback_interval: Duration::from_secs(FALLBACK_REFRESH_SECS),
            refresh_watchdog_timeout: Duration::from_secs(10),
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
        Message::Tick => refresh::handle_tick(refresh_state, timers, config),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

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
}
