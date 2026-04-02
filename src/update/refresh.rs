use std::time::{Duration, Instant};

use crate::app::App;
use crate::message::{FetchResult, LoopAction, RefreshOutcome, RefreshTrigger, UpdateResult};
use crate::vcs::shared::is_transient_vcs_error;
use crate::vcs::Vcs;

use super::{RefreshState, Timers, UpdateConfig};

/// Delay before processing VCS internal events (500ms reduces lock collisions by ~80%)
const VCS_EVENT_DELAY_MS: u64 = 500;

/// How often to check for VCS backend changes (e.g., .jj appearing or disappearing).
const VCS_CHECK_INTERVAL_SECS: u64 = 2;

/// Initial delay before retrying after a transient VCS error.
const TRANSIENT_RETRY_BASE_MS: u64 = 1000;

/// Maximum delay between transient error retries.
const TRANSIENT_RETRY_MAX_MS: u64 = 5000;

/// Stop auto-retrying after this many consecutive transient failures.
const TRANSIENT_RETRY_MAX_ATTEMPTS: u32 = 10;

/// Handle completed refresh operations.
pub(super) fn handle_refresh(
    outcome: RefreshOutcome,
    app: &mut App,
    refresh_state: &mut RefreshState,
    timers: &mut Timers,
    config: &UpdateConfig,
    vcs: &dyn Vcs,
) -> UpdateResult {
    let mut result = UpdateResult::default();

    match outcome {
        RefreshOutcome::Success(refresh_result) => {
            let refresh_result = *refresh_result;
            // Clear pending VCS events that are likely self-triggered by our own
            // VCS commands during the refresh (e.g., jj auto-snapshot)
            timers.pending_vcs_event = None;
            timers.last_refresh_completed = Some(Instant::now());
            timers.transient_retry_at = None;
            timers.transient_retry_attempt = 0;

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

            // Detect external VCS operations (e.g., jj new) that happened during refresh
            if let Some(rev_id) = &refresh_result.revision_id {
                if timers.last_known_revision.as_ref().is_some_and(|prev| prev != rev_id) {
                    result.refresh = RefreshTrigger::Full;
                }
                timers.last_known_revision = Some(rev_id.clone());
            }

            app.apply_refresh_result(refresh_result);
            app.load_images_for_markers(vcs);
            timers.last_refresh = Instant::now();
            result.needs_redraw = true;
        }
        RefreshOutcome::SingleFile { path, diff, revision_id } => {
            timers.pending_vcs_event = None;
            timers.last_refresh_completed = Some(Instant::now());
            timers.transient_retry_at = None;
            timers.transient_retry_attempt = 0;

            if let Some(rev_id) = &revision_id {
                if timers.last_known_revision.as_ref().is_some_and(|prev| prev != rev_id) {
                    result.refresh = RefreshTrigger::Full;
                }
                timers.last_known_revision = Some(rev_id.clone());
            }

            app.update_single_file(&path, diff);
            timers.last_refresh = Instant::now();
            result.needs_redraw = true;
        }
        RefreshOutcome::Cancelled => {
            result.needs_redraw = true;
        }
        RefreshOutcome::Error(msg) => {
            if is_transient_vcs_error(&msg)
                && timers.transient_retry_attempt < TRANSIENT_RETRY_MAX_ATTEMPTS
            {
                let delay_ms = (TRANSIENT_RETRY_BASE_MS << timers.transient_retry_attempt)
                    .min(TRANSIENT_RETRY_MAX_MS);
                timers.transient_retry_at =
                    Some(Instant::now() + Duration::from_millis(delay_ms));
                timers.transient_retry_attempt += 1;
                app.error = Some(format!(
                    "{msg} (retrying in {:.0}s...)",
                    delay_ms as f64 / 1000.0
                ));
            } else {
                timers.transient_retry_at = None;
                app.error = Some(msg);
            }
            result.needs_redraw = true;
        }
    }

    if refresh_state.complete() {
        result.refresh = RefreshTrigger::Full;
    }

    result
}

/// Handle completed fetch operations.
pub(super) fn handle_fetch(
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
        && new_base != app.base_identifier
    {
        app.base_identifier = new_base;
        if refresh_state.is_idle() {
            result.refresh = RefreshTrigger::Full;
        } else {
            refresh_state.mark_pending();
        }
    }

    result
}

/// Handle periodic tick for timer-based operations.
pub(super) fn handle_tick(
    refresh_state: &mut RefreshState,
    timers: &mut Timers,
    config: &UpdateConfig,
) -> UpdateResult {
    let mut result = UpdateResult::default();

    // Detect VCS backend changes (e.g., jj init --colocate or rm -rf .jj)
    if timers.last_vcs_check.elapsed() >= Duration::from_secs(VCS_CHECK_INTERVAL_SECS) {
        timers.last_vcs_check = Instant::now();
        let jj_now = config.repo_path.join(".jj").is_dir();
        if jj_now != timers.jj_present {
            timers.jj_present = jj_now;
            result.loop_action = LoopAction::RestartVcs;
            return result;
        }
    }

    // Trigger periodic fetch if enabled
    if config.auto_fetch
        && !timers.fetch_in_progress
        && timers.last_fetch.elapsed() >= config.fetch_interval
    {
        timers.fetch_in_progress = true;
        timers.last_fetch = Instant::now();
        result.trigger_fetch = true;
    }

    // Process delayed VCS internal events (differentiated debouncing)
    if let Some(pending_time) = timers.pending_vcs_event
        && pending_time.elapsed() >= Duration::from_millis(VCS_EVENT_DELAY_MS)
        && refresh_state.is_idle()
    {
        timers.pending_vcs_event = None;
        result.refresh = RefreshTrigger::Full;
        return result;
    }

    // Transient error retry
    if let Some(retry_at) = timers.transient_retry_at
        && Instant::now() >= retry_at
        && refresh_state.is_idle()
    {
        timers.transient_retry_at = None;
        result.refresh = RefreshTrigger::Full;
        return result;
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
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use crate::message::FALLBACK_REFRESH_SECS;
    use crate::test_support::{base_line, StubVcs, TestAppBuilder};

    #[test]
    fn test_handle_refresh_success() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        timers.last_refresh = Instant::now() - Duration::from_secs(60);
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let outcome = RefreshOutcome::success(crate::vcs::RefreshResult {
            files: vec![],
            lines: vec![base_line("new content")],
            base_identifier: "def456".to_string(),
            base_label: None,
            current_branch: Some("feature".to_string()),
            metrics: crate::limits::DiffMetrics::default(),
            file_links: std::collections::HashMap::new(),
            stack_position: None, bookmark_name: None,
            revision_id: None,
            divergence: None,
        });

        let config = UpdateConfig::default();
        let result = handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs);

        assert_eq!(result.refresh, RefreshTrigger::None);
        assert!(refresh_state.is_idle());
        assert_eq!(app.base_identifier, "def456");
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
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let outcome = RefreshOutcome::Error("test error".to_string());
        let config = UpdateConfig::default();
        let result = handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs);

        assert_eq!(result.refresh, RefreshTrigger::Full);
        assert!(refresh_state.is_idle());
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
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let outcome = RefreshOutcome::success(crate::vcs::RefreshResult {
            files: vec![],
            lines: vec![base_line("content")],
            base_identifier: "abc".to_string(),
            base_label: None,
            current_branch: Some("main".to_string()),
            metrics: crate::limits::DiffMetrics::default(),
            file_links: std::collections::HashMap::new(),
            stack_position: None, bookmark_name: None,
            revision_id: None,
            divergence: None,
        });

        let result = handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs);
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
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let outcome = RefreshOutcome::SingleFile {
            path: "test.rs".to_string(),
            diff: None,
            revision_id: None,
        };

        let result = handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs);
        assert!(result.needs_redraw);
    }

    #[test]
    fn test_handle_refresh_single_file_sets_last_refresh_completed() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        assert!(timers.last_refresh_completed.is_none());

        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let outcome = RefreshOutcome::SingleFile {
            path: "test.rs".to_string(),
            diff: None,
            revision_id: None,
        };

        handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs);

        assert!(
            timers.last_refresh_completed.is_some(),
            "SingleFile completion should set last_refresh_completed for cooldown"
        );
    }

    #[test]
    fn test_handle_refresh_single_file_clears_pending_vcs_event() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        timers.pending_vcs_event = Some(Instant::now() - Duration::from_millis(100));

        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let outcome = RefreshOutcome::SingleFile {
            path: "test.rs".to_string(),
            diff: None,
            revision_id: None,
        };

        handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs);

        assert!(
            timers.pending_vcs_event.is_none(),
            "SingleFile completion should clear pending VCS events (likely self-triggered)"
        );
    }

    #[test]
    fn test_handle_refresh_clears_pending_vcs_event() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        // Simulate a pending VCS event from just before refresh completed
        timers.pending_vcs_event = Some(Instant::now() - Duration::from_millis(100));
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let outcome = RefreshOutcome::success(crate::vcs::RefreshResult {
            files: vec![],
            lines: vec![base_line("content")],
            base_identifier: "abc".to_string(),
            base_label: None,
            current_branch: Some("main".to_string()),
            metrics: crate::limits::DiffMetrics::default(),
            file_links: std::collections::HashMap::new(),
            stack_position: None, bookmark_name: None,
            revision_id: None,
            divergence: None,
        });

        let config = UpdateConfig::default();
        handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs);

        assert!(
            timers.pending_vcs_event.is_none(),
            "successful refresh should clear pending VCS events (likely self-triggered)"
        );
    }

    #[test]
    fn test_handle_refresh_error_sets_app_error_and_redraws() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let result = handle_refresh(
            RefreshOutcome::Error("jj diff failed: Config error".to_string()),
            &mut app,
            &mut refresh_state,
            &mut timers,
            &config,
            &vcs,
        );

        assert!(result.needs_redraw);
        assert_eq!(app.error, Some("jj diff failed: Config error".to_string()));
        assert!(refresh_state.is_idle());
    }

    #[test]
    fn test_handle_refresh_error_cleared_by_success() {
        let mut app = TestAppBuilder::new().build();
        app.error = Some("previous error".to_string());
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let outcome = RefreshOutcome::success(crate::vcs::RefreshResult {
            files: vec![],
            lines: vec![base_line("content")],
            base_identifier: "abc".to_string(),
            base_label: None,
            current_branch: Some("main".to_string()),
            metrics: crate::limits::DiffMetrics::default(),
            file_links: std::collections::HashMap::new(),
            stack_position: None, bookmark_name: None,
            revision_id: None,
            divergence: None,
        });

        handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs);
        assert!(app.error.is_none());
    }

    #[test]
    fn test_handle_refresh_cancelled_does_not_set_error() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let result = handle_refresh(
            RefreshOutcome::Cancelled,
            &mut app, &mut refresh_state, &mut timers, &config, &vcs,
        );

        assert!(result.needs_redraw);
        assert!(app.error.is_none());
        assert!(refresh_state.is_idle());
    }

    #[test]
    fn test_handle_refresh_cancelled_with_pending_triggers_rerefresh() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgressPending {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(true)),
        };
        let mut timers = Timers::default();
        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let result = handle_refresh(
            RefreshOutcome::Cancelled,
            &mut app, &mut refresh_state, &mut timers, &config, &vcs,
        );

        assert_eq!(result.refresh, RefreshTrigger::Full);
        assert!(app.error.is_none());
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
        app.base_identifier = "old_base".to_string();
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
        assert_eq!(app.base_identifier, "new_base");
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
    fn test_handle_tick_processes_pending_vcs_event_after_delay() {
        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers::default();
        timers.pending_vcs_event = Some(Instant::now() - Duration::from_millis(600));

        let config = UpdateConfig::default();
        let result = handle_tick(&mut refresh_state, &mut timers, &config);

        assert_eq!(result.refresh, RefreshTrigger::Full);
        assert!(timers.pending_vcs_event.is_none());
    }

    #[test]
    fn test_handle_tick_does_not_process_pending_vcs_event_too_early() {
        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers::default();
        timers.pending_vcs_event = Some(Instant::now());

        let config = UpdateConfig::default();
        let result = handle_tick(&mut refresh_state, &mut timers, &config);

        assert_eq!(result.refresh, RefreshTrigger::None);
        assert!(timers.pending_vcs_event.is_some());
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
    fn test_handle_tick_detects_vcs_change() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        std::fs::create_dir(temp.path().join(".jj")).unwrap();

        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers::new(false); // started without .jj
        timers.last_vcs_check = Instant::now() - Duration::from_secs(3);

        let config = UpdateConfig {
            repo_path: temp.path().to_path_buf(),
            auto_fetch: false,
            ..Default::default()
        };

        let result = handle_tick(&mut refresh_state, &mut timers, &config);
        assert_eq!(result.loop_action, LoopAction::RestartVcs);
        assert!(timers.jj_present);
    }

    #[test]
    fn test_handle_tick_detects_vcs_removal() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        // No .jj directory exists

        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers::new(true); // started with .jj
        timers.last_vcs_check = Instant::now() - Duration::from_secs(3);

        let config = UpdateConfig {
            repo_path: temp.path().to_path_buf(),
            auto_fetch: false,
            ..Default::default()
        };

        let result = handle_tick(&mut refresh_state, &mut timers, &config);
        assert_eq!(result.loop_action, LoopAction::RestartVcs);
        assert!(!timers.jj_present);
    }

    #[test]
    fn test_handle_tick_no_vcs_change() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        // No .jj directory, and jj_present is false — no change

        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers::new(false);
        timers.last_vcs_check = Instant::now() - Duration::from_secs(3);

        let config = UpdateConfig {
            repo_path: temp.path().to_path_buf(),
            auto_fetch: false,
            ..Default::default()
        };

        let result = handle_tick(&mut refresh_state, &mut timers, &config);
        assert_eq!(result.loop_action, LoopAction::Continue);
    }

    #[test]
    fn test_handle_tick_no_vcs_change_jj_still_present() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        std::fs::create_dir(temp.path().join(".jj")).unwrap();

        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers::new(true); // started with .jj, still present
        timers.last_vcs_check = Instant::now() - Duration::from_secs(3);

        let config = UpdateConfig {
            repo_path: temp.path().to_path_buf(),
            auto_fetch: false,
            ..Default::default()
        };

        let result = handle_tick(&mut refresh_state, &mut timers, &config);
        assert_eq!(result.loop_action, LoopAction::Continue);
    }

    #[test]
    fn test_handle_refresh_detects_revision_change() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        timers.last_known_revision = Some("old_rev".to_string());
        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let outcome = RefreshOutcome::success(crate::vcs::RefreshResult {
            files: vec![],
            lines: vec![base_line("content")],
            base_identifier: "abc".to_string(),
            base_label: None,
            current_branch: Some("main".to_string()),
            metrics: crate::limits::DiffMetrics::default(),
            file_links: std::collections::HashMap::new(),
            stack_position: None, bookmark_name: None,
            revision_id: Some("new_rev".to_string()),
            divergence: None,
        });

        let result = handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs);

        assert_eq!(result.refresh, RefreshTrigger::Full,
            "revision change should trigger follow-up Full refresh");
        assert_eq!(timers.last_known_revision.as_deref(), Some("new_rev"));
    }

    #[test]
    fn test_handle_refresh_same_revision_no_followup() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        timers.last_known_revision = Some("same_rev".to_string());
        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let outcome = RefreshOutcome::success(crate::vcs::RefreshResult {
            files: vec![],
            lines: vec![base_line("content")],
            base_identifier: "abc".to_string(),
            base_label: None,
            current_branch: Some("main".to_string()),
            metrics: crate::limits::DiffMetrics::default(),
            file_links: std::collections::HashMap::new(),
            stack_position: None, bookmark_name: None,
            revision_id: Some("same_rev".to_string()),
            divergence: None,
        });

        let result = handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs);

        assert_eq!(result.refresh, RefreshTrigger::None,
            "same revision should not trigger follow-up refresh");
    }

    #[test]
    fn test_handle_refresh_single_file_detects_revision_change() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        timers.last_known_revision = Some("old_rev".to_string());
        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let outcome = RefreshOutcome::SingleFile {
            path: "test.rs".to_string(),
            diff: None,
            revision_id: Some("new_rev".to_string()),
        };

        let result = handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs);

        assert_eq!(result.refresh, RefreshTrigger::Full,
            "SingleFile with revision change should trigger Full refresh");
        assert_eq!(timers.last_known_revision.as_deref(), Some("new_rev"));
    }

    #[test]
    fn test_handle_refresh_first_refresh_sets_revision() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        assert!(timers.last_known_revision.is_none());
        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let outcome = RefreshOutcome::success(crate::vcs::RefreshResult {
            files: vec![],
            lines: vec![base_line("content")],
            base_identifier: "abc".to_string(),
            base_label: None,
            current_branch: Some("main".to_string()),
            metrics: crate::limits::DiffMetrics::default(),
            file_links: std::collections::HashMap::new(),
            stack_position: None, bookmark_name: None,
            revision_id: Some("first_rev".to_string()),
            divergence: None,
        });

        let result = handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs);

        assert_eq!(result.refresh, RefreshTrigger::None,
            "first refresh should just store revision, not trigger follow-up");
        assert_eq!(timers.last_known_revision.as_deref(), Some("first_rev"));
    }

    #[test]
    fn test_transient_error_schedules_retry() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let outcome = RefreshOutcome::Error(
            "jj diff --summary failed: The working copy is stale".to_string(),
        );
        let result = handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs);

        assert!(result.needs_redraw);
        assert!(timers.transient_retry_at.is_some());
        assert_eq!(timers.transient_retry_attempt, 1);
        assert!(app.error.as_ref().unwrap().contains("retrying"));
    }

    #[test]
    fn test_non_transient_error_no_retry() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let outcome = RefreshOutcome::Error("Config error: no such revision".to_string());
        handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs);

        assert!(timers.transient_retry_at.is_none());
        assert_eq!(timers.transient_retry_attempt, 0);
        assert!(!app.error.as_ref().unwrap().contains("retrying"));
    }

    #[test]
    fn test_transient_retry_backoff_increases() {
        let mut app = TestAppBuilder::new().build();
        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));
        let mut timers = Timers::default();

        let mut delays = Vec::new();
        for _ in 0..5 {
            let mut refresh_state = RefreshState::InProgress {
                started_at: Instant::now(),
                cancel_flag: Arc::new(AtomicBool::new(false)),
            };
            let outcome = RefreshOutcome::Error(
                "The working copy is stale".to_string(),
            );
            handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs);
            let retry_at = timers.transient_retry_at.unwrap();
            let delay = retry_at.duration_since(Instant::now());
            delays.push(delay);
        }

        // Delays should increase: 1s, 2s, 4s, 5s(cap), 5s(cap)
        assert!(delays[1] > delays[0], "second delay should exceed first");
        assert!(delays[2] > delays[1], "third delay should exceed second");
        // 4th and 5th should be capped at 5s
        let cap = Duration::from_millis(TRANSIENT_RETRY_MAX_MS + 100);
        assert!(delays[3] < cap, "delay should be capped");
        assert!(delays[4] < cap, "delay should be capped");
    }

    #[test]
    fn test_transient_retry_max_attempts_exceeded() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        timers.transient_retry_attempt = TRANSIENT_RETRY_MAX_ATTEMPTS;
        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let outcome = RefreshOutcome::Error(
            "The working copy is stale".to_string(),
        );
        handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs);

        assert!(timers.transient_retry_at.is_none(), "should not schedule retry past max attempts");
        assert!(!app.error.as_ref().unwrap().contains("retrying"));
    }

    #[test]
    fn test_handle_tick_fires_transient_retry() {
        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers::default();
        timers.transient_retry_at = Some(Instant::now() - Duration::from_millis(100));
        let config = UpdateConfig::default();

        let result = handle_tick(&mut refresh_state, &mut timers, &config);

        assert_eq!(result.refresh, RefreshTrigger::Full);
        assert!(timers.transient_retry_at.is_none());
    }

    #[test]
    fn test_handle_tick_no_transient_retry_when_not_idle() {
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        timers.transient_retry_at = Some(Instant::now() - Duration::from_millis(100));
        let config = UpdateConfig::default();

        let result = handle_tick(&mut refresh_state, &mut timers, &config);

        assert_eq!(result.refresh, RefreshTrigger::None);
        assert!(timers.transient_retry_at.is_some(), "should preserve retry timer when busy");
    }

    #[test]
    fn test_handle_tick_no_transient_retry_when_future() {
        let mut refresh_state = RefreshState::Idle;
        let mut timers = Timers::default();
        timers.transient_retry_at = Some(Instant::now() + Duration::from_secs(60));
        let config = UpdateConfig::default();

        let result = handle_tick(&mut refresh_state, &mut timers, &config);

        assert_eq!(result.refresh, RefreshTrigger::None);
        assert!(timers.transient_retry_at.is_some());
    }

    #[test]
    fn test_success_clears_transient_retry_state() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut timers = Timers::default();
        timers.transient_retry_at = Some(Instant::now() + Duration::from_secs(5));
        timers.transient_retry_attempt = 3;
        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        let outcome = RefreshOutcome::success(crate::vcs::RefreshResult {
            files: vec![],
            lines: vec![base_line("content")],
            base_identifier: "abc".to_string(),
            base_label: None,
            current_branch: Some("main".to_string()),
            metrics: crate::limits::DiffMetrics::default(),
            file_links: std::collections::HashMap::new(),
            stack_position: None, bookmark_name: None,
            revision_id: None,
            divergence: None,
        });

        handle_refresh(outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs);

        assert!(timers.transient_retry_at.is_none());
        assert_eq!(timers.transient_retry_attempt, 0);
    }

    #[test]
    fn test_cancelled_preserves_transient_retry_state() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(true)),
        };
        let mut timers = Timers::default();
        let retry_at = Instant::now() + Duration::from_secs(2);
        timers.transient_retry_at = Some(retry_at);
        timers.transient_retry_attempt = 3;
        let config = UpdateConfig::default();
        let vcs = StubVcs::new(PathBuf::from("/tmp/test"));

        handle_refresh(RefreshOutcome::Cancelled, &mut app, &mut refresh_state, &mut timers, &config, &vcs);

        assert!(
            timers.transient_retry_at.is_some(),
            "Cancelled should not clear transient retry timer"
        );
        assert_eq!(timers.transient_retry_attempt, 3);
    }
}
