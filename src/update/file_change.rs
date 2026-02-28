use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use notify_debouncer_mini::{DebouncedEvent, DebouncedEventKind};

use crate::app::App;
use crate::file_events::VcsLockState;
use crate::gitignore::GitignoreFilter;
use crate::message::{RefreshTrigger, UpdateResult};
use crate::vcs::{Vcs, VcsEventType};

use super::{RefreshState, Timers};

/// Cooldown after refresh completion during which VCS-only events are suppressed.
/// Must exceed the debouncer timeout (100ms) plus filesystem event delivery latency
/// to catch self-triggered events from our own jj/git commands.
const POST_REFRESH_COOLDOWN_MS: u64 = 300;

fn is_noisy_path(path_str: &str) -> bool {
    path_str.contains("/tmp/")
        || path_str.contains("/node_modules/")
        || path_str.contains("/vendor/bundle/")
        || path_str.contains("/.bundle/")
        || path_str.contains("/log/")
        || path_str.ends_with(".lock")
}

fn is_vcs_path(path: &Path, repo_root: &Path) -> bool {
    let relative = path.strip_prefix(repo_root).unwrap_or(path);
    relative
        .components()
        .next()
        .is_some_and(|c| c.as_os_str() == ".jj" || c.as_os_str() == ".git")
}

/// Handle file system change events.
pub(super) fn handle_file_change(
    events: Vec<DebouncedEvent>,
    app: &mut App,
    refresh_state: &mut RefreshState,
    vcs_lock: &mut VcsLockState,
    timers: &mut Timers,
    vcs: &dyn Vcs,
) -> UpdateResult {
    let mut result = UpdateResult::default();

    // Deduplicate paths before processing (rapid saves can generate multiple events per file)
    let unique_paths: HashSet<_> = events
        .iter()
        .filter(|e| e.kind == DebouncedEventKind::Any)
        .map(|e| &e.path)
        .collect();

    // Check for VCS lock events BEFORE filtering
    let has_lock_event = unique_paths
        .iter()
        .any(|p| vcs.classify_event(p) == VcsEventType::Lock);

    if has_lock_event {
        let currently_locked = vcs.is_locked();
        let was_locked = vcs_lock.is_locked();

        // If we just unlocked and had pending refresh, trigger it
        // Must take pending BEFORE set_locked(false) which clears it
        if was_locked && !currently_locked && vcs_lock.take_pending() {
            vcs_lock.set_locked(false);
            result.refresh = RefreshTrigger::Full;
            return result;
        }

        vcs_lock.set_locked(currently_locked);
    }

    // Rebuild gitignore matcher if any .gitignore file changed
    let gitignore_changed = unique_paths
        .iter()
        .any(|p| GitignoreFilter::is_gitignore_file(p));
    if gitignore_changed {
        app.gitignore_filter.rebuild();
    }

    let repo_root = vcs.repo_path();
    let filtered_paths: Vec<_> = unique_paths
        .into_iter()
        .filter(|p| {
            let relative = p.strip_prefix(repo_root).unwrap_or(p);
            !is_noisy_path(&relative.to_string_lossy())
        })
        .filter(|p| is_vcs_path(p, repo_root) || !app.gitignore_filter.is_ignored(p))
        .collect();

    let mut should_refresh = false;
    let mut has_vcs_change = false;
    let mut source_files = Vec::new();

    for path in &filtered_paths {
        match vcs.classify_event(path) {
            VcsEventType::RevisionChange => {
                should_refresh = true;
                has_vcs_change = true;
            }
            VcsEventType::Internal => {
                should_refresh = true;
            }
            VcsEventType::Lock => {
                // Already handled above
            }
            VcsEventType::Source => {
                should_refresh = true;
                source_files.push(path);
            }
        }
    }

    if should_refresh {
        // If VCS is locked by external process, defer refresh
        if vcs_lock.is_locked() {
            vcs_lock.set_pending();
            return result;
        }

        // Differentiated debouncing: VCS internal events get delayed processing
        let has_only_vcs_changes = source_files.is_empty();

        if has_only_vcs_changes {
            if has_vcs_change {
                if !refresh_state.is_idle() {
                    // Suppress VCS-only RevisionChange during active refresh.
                    // Almost always self-triggered by our own jj/git commands
                    // (e.g., jj auto-snapshot modifying .jj/working_copy/).
                    // Using mark_pending() here would cause infinite Full refresh
                    // loops since each Full's jj commands trigger auto-snapshot.
                    // Trade-off: a real concurrent `jj new` during the 1-3s refresh
                    // window is missed until the next file edit or manual 'r'.
                    return result;
                }
                if let Some(completed) = timers.last_refresh_completed
                    && completed.elapsed() < Duration::from_millis(POST_REFRESH_COOLDOWN_MS)
                {
                    return result;
                }
                result.refresh = RefreshTrigger::Full;
                return result;
            }

            if !refresh_state.is_idle() {
                return result;
            }
            if let Some(completed) = timers.last_refresh_completed
                && completed.elapsed() < Duration::from_millis(POST_REFRESH_COOLDOWN_MS)
            {
                return result;
            }
            timers.pending_vcs_event = Some(Instant::now());
            return result;
        }

        let had_pending_vcs = timers.pending_vcs_event.take().is_some();
        if !refresh_state.is_idle() {
            refresh_state.mark_pending();
        } else {
            let can_use_single_file =
                !has_vcs_change && !had_pending_vcs && source_files.len() == 1 && !app.files.is_empty();

            if can_use_single_file {
                let file_path = source_files[0]
                    .strip_prefix(vcs.repo_path())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use crate::test_support::{StubVcs, TestAppBuilder};

    #[test]
    fn test_is_noisy_path() {
        assert!(is_noisy_path("/tmp/file.txt"));
        assert!(is_noisy_path("/project/node_modules/pkg/file.js"));
        assert!(is_noisy_path("/project/file.lock"));
        assert!(!is_noisy_path("/project/src/main.rs"));
    }

    #[test]
    fn test_duplicate_file_events_are_deduplicated() {
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

    #[test]
    fn test_handle_file_change_no_redraw() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        let vcs = StubVcs::new(PathBuf::from("/repo"));

        let events = vec![DebouncedEvent::new(
            PathBuf::from("/repo/src/main.rs"),
            DebouncedEventKind::Any,
        )];

        let result = handle_file_change(events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs);
        // File changes trigger background refresh, not immediate redraw
        assert!(!result.needs_redraw);
    }

    #[test]
    fn test_handle_file_change_skips_refresh_when_locked() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("index.lock"), "").unwrap();

        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        let vcs = StubVcs::new(temp.path().to_path_buf());

        // First, trigger lock detection by sending a lock file event
        let lock_events = vec![DebouncedEvent::new(
            git_dir.join("index.lock"),
            DebouncedEventKind::Any,
        )];
        let _ = handle_file_change(lock_events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs);

        assert!(vcs_lock.is_locked());

        // Now send a source file change event
        let events = vec![DebouncedEvent::new(
            temp.path().join("src/main.rs"),
            DebouncedEventKind::Any,
        )];
        let result = handle_file_change(events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs);

        // Should NOT trigger refresh because we're locked
        assert_eq!(result.refresh, RefreshTrigger::None);
    }

    #[test]
    fn test_handle_file_change_triggers_refresh_on_unlock() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let lock_path = git_dir.join("index.lock");
        std::fs::write(&lock_path, "").unwrap();

        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        let vcs = StubVcs::new(temp.path().to_path_buf());

        // First, detect the lock
        let lock_events = vec![DebouncedEvent::new(
            lock_path.clone(),
            DebouncedEventKind::Any,
        )];
        let _ = handle_file_change(lock_events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs);
        assert!(vcs_lock.is_locked());

        // Send a source file change while locked - should mark pending
        let source_events = vec![DebouncedEvent::new(
            temp.path().join("src/main.rs"),
            DebouncedEventKind::Any,
        )];
        let _ = handle_file_change(source_events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs);

        // Remove lock file to simulate VCS operation completing
        std::fs::remove_file(&lock_path).unwrap();

        // Send lock file event again (deletion is also an event)
        let unlock_events = vec![DebouncedEvent::new(
            lock_path,
            DebouncedEventKind::Any,
        )];
        let result = handle_file_change(unlock_events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs);

        assert!(!vcs_lock.is_locked());
        assert_eq!(result.refresh, RefreshTrigger::Full);
    }

    #[test]
    fn test_vcs_events_deferred_for_differentiated_debouncing() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        let vcs = StubVcs::new(temp.path().to_path_buf());

        // Send a .git/index event (VCS internal change, no source files)
        let events = vec![DebouncedEvent::new(
            git_dir.join("index"),
            DebouncedEventKind::Any,
        )];
        let result = handle_file_change(events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs);

        // Should NOT trigger immediate refresh - should be deferred
        assert_eq!(result.refresh, RefreshTrigger::None);
        assert!(timers.pending_vcs_event.is_some());
    }

    #[test]
    fn test_source_file_events_trigger_immediate_refresh() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join(".git")).unwrap();

        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        let vcs = StubVcs::new(temp.path().to_path_buf());

        let events = vec![DebouncedEvent::new(
            temp.path().join("src/main.rs"),
            DebouncedEventKind::Any,
        )];
        let result = handle_file_change(events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs);

        assert_eq!(result.refresh, RefreshTrigger::Full);
    }

    #[test]
    fn test_mixed_vcs_and_source_events_trigger_immediate_refresh() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        let vcs = StubVcs::new(temp.path().to_path_buf());

        let events = vec![
            DebouncedEvent::new(git_dir.join("index"), DebouncedEventKind::Any),
            DebouncedEvent::new(temp.path().join("src/main.rs"), DebouncedEventKind::Any),
        ];
        let result = handle_file_change(events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs);

        // Source files take priority — immediate refresh
        assert_eq!(result.refresh, RefreshTrigger::Full);
        assert!(timers.pending_vcs_event.is_none());
    }

    #[test]
    fn test_source_file_event_clears_stale_pending_vcs_timer() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join(".git")).unwrap();

        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            cancel_flag: Arc::new(AtomicBool::new(false)),
            started_at: Instant::now(),
        };
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        timers.pending_vcs_event = Some(Instant::now() - Duration::from_millis(300));
        let vcs = StubVcs::new(temp.path().to_path_buf());

        let events = vec![
            DebouncedEvent::new(temp.path().join("src/main.rs"), DebouncedEventKind::Any),
        ];
        let _result = handle_file_change(events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs);

        assert!(
            timers.pending_vcs_event.is_none(),
            "source file event should clear stale pending_vcs_event"
        );
    }

    #[test]
    fn test_revision_change_during_refresh_marks_pending_without_cancel() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            cancel_flag: cancel_flag.clone(),
            started_at: Instant::now(),
        };
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        let vcs = StubVcs::new(temp.path().to_path_buf());

        let events = vec![
            DebouncedEvent::new(git_dir.join("HEAD"), DebouncedEventKind::Any),
            DebouncedEvent::new(temp.path().join("src/main.rs"), DebouncedEventKind::Any),
        ];
        let _result = handle_file_change(events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs);

        assert!(matches!(refresh_state, RefreshState::InProgressPending { .. }));
        assert!(
            !cancel_flag.load(Ordering::Relaxed),
            "cancel flag should not be set during active refresh"
        );
    }

    #[test]
    fn test_revision_change_only_triggers_immediate_full() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        timers.last_refresh_completed = Some(Instant::now() - Duration::from_secs(5));
        let vcs = StubVcs::new(temp.path().to_path_buf());

        let events = vec![
            DebouncedEvent::new(git_dir.join("HEAD"), DebouncedEventKind::Any),
        ];
        let result = handle_file_change(events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs);

        assert_eq!(result.refresh, RefreshTrigger::Full);
        assert!(timers.pending_vcs_event.is_none());
    }

    #[test]
    fn test_vcs_only_event_during_in_progress_refresh_does_not_cancel() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let mut app = TestAppBuilder::new().build();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let mut refresh_state = RefreshState::InProgress {
            cancel_flag: cancel_flag.clone(),
            started_at: Instant::now(),
        };
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        let vcs = StubVcs::new(temp.path().to_path_buf());

        let events = vec![
            DebouncedEvent::new(git_dir.join("index"), DebouncedEventKind::Any),
        ];
        let result = handle_file_change(events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs);

        assert_eq!(result.refresh, RefreshTrigger::None);
        // VCS-only events during active refresh are suppressed entirely
        // (likely side-effects of our own VCS commands like jj auto-snapshot)
        assert!(timers.pending_vcs_event.is_none());
        assert!(!cancel_flag.load(Ordering::Relaxed), "cancel flag should not be set");
        assert!(
            matches!(refresh_state, RefreshState::InProgress { .. }),
            "should remain InProgress, not InProgressPending"
        );
    }

    #[test]
    fn test_vcs_only_events_ignored_during_active_refresh() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            cancel_flag: Arc::new(AtomicBool::new(false)),
            started_at: Instant::now(),
        };
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        let vcs = StubVcs::new(temp.path().to_path_buf());

        // Send VCS-only event while refresh is in progress
        let events = vec![DebouncedEvent::new(
            git_dir.join("index"),
            DebouncedEventKind::Any,
        )];
        let result = handle_file_change(events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs);

        assert_eq!(result.refresh, RefreshTrigger::None);
        assert!(
            timers.pending_vcs_event.is_none(),
            "VCS-only events during active refresh should be suppressed entirely"
        );
    }

    #[test]
    fn test_vcs_event_suppressed_during_post_refresh_cooldown() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        timers.last_refresh_completed = Some(Instant::now() - Duration::from_millis(100));

        let vcs = StubVcs::new(temp.path().to_path_buf());

        let events = vec![DebouncedEvent::new(
            git_dir.join("index"),
            DebouncedEventKind::Any,
        )];
        let result = handle_file_change(
            events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs,
        );

        assert_eq!(result.refresh, RefreshTrigger::None);
        assert!(
            timers.pending_vcs_event.is_none(),
            "VCS event during cooldown should be suppressed"
        );
    }

    #[test]
    fn test_vcs_event_accepted_after_cooldown_expires() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        timers.last_refresh_completed = Some(Instant::now() - Duration::from_millis(500));

        let vcs = StubVcs::new(temp.path().to_path_buf());

        let events = vec![DebouncedEvent::new(
            git_dir.join("index"),
            DebouncedEventKind::Any,
        )];
        let result = handle_file_change(
            events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs,
        );

        assert_eq!(result.refresh, RefreshTrigger::None);
        assert!(
            timers.pending_vcs_event.is_some(),
            "VCS event after cooldown should be accepted as pending"
        );
    }

    #[test]
    fn test_revision_change_alone_triggers_immediate_full_refresh() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        timers.last_refresh_completed = Some(Instant::now() - Duration::from_secs(5));
        let vcs = StubVcs::new(temp.path().to_path_buf());

        let events = vec![DebouncedEvent::new(
            git_dir.join("HEAD"),
            DebouncedEventKind::Any,
        )];
        let result = handle_file_change(
            events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs,
        );

        assert_eq!(result.refresh, RefreshTrigger::Full);
        assert!(timers.pending_vcs_event.is_none());
    }

    #[test]
    fn test_revision_change_during_refresh_suppressed() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            cancel_flag: cancel_flag.clone(),
            started_at: Instant::now(),
        };
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        let vcs = StubVcs::new(temp.path().to_path_buf());

        let events = vec![DebouncedEvent::new(
            git_dir.join("HEAD"),
            DebouncedEventKind::Any,
        )];
        let result = handle_file_change(
            events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs,
        );

        assert_eq!(result.refresh, RefreshTrigger::None);
        assert!(
            matches!(refresh_state, RefreshState::InProgress { .. }),
            "should remain InProgress — VCS events during refresh are suppressed"
        );
        assert!(
            !cancel_flag.load(Ordering::Relaxed),
            "cancel flag should not be set"
        );
    }

    #[test]
    fn test_revision_change_within_cooldown_suppressed() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        timers.last_refresh_completed = Some(Instant::now() - Duration::from_millis(100));
        let vcs = StubVcs::new(temp.path().to_path_buf());

        let events = vec![DebouncedEvent::new(
            git_dir.join("HEAD"),
            DebouncedEventKind::Any,
        )];
        let result = handle_file_change(
            events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs,
        );

        assert_eq!(result.refresh, RefreshTrigger::None);
    }

    #[test]
    fn test_source_events_with_pending_vcs_force_full_refresh() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let dummy = crate::diff::FileDiff {
            lines: vec![crate::diff::DiffLine::file_header("src/lib.rs")],
        };
        let mut app = TestAppBuilder::new().with_files(vec![dummy]).build();
        let mut refresh_state = RefreshState::Idle;
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        timers.pending_vcs_event = Some(Instant::now());
        let vcs = StubVcs::new(temp.path().to_path_buf());

        let events = vec![DebouncedEvent::new(
            temp.path().join("src/main.rs"),
            DebouncedEventKind::Any,
        )];
        let result = handle_file_change(
            events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs,
        );

        assert_eq!(result.refresh, RefreshTrigger::Full);
        assert!(timers.pending_vcs_event.is_none());
    }

    #[test]
    fn test_single_source_event_without_pending_vcs_uses_single_file() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let dummy = crate::diff::FileDiff {
            lines: vec![crate::diff::DiffLine::file_header("src/lib.rs")],
        };
        let mut app = TestAppBuilder::new().with_files(vec![dummy]).build();
        let mut refresh_state = RefreshState::Idle;
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        let vcs = StubVcs::new(temp.path().to_path_buf());

        let events = vec![DebouncedEvent::new(
            temp.path().join("src/main.rs"),
            DebouncedEventKind::Any,
        )];
        let result = handle_file_change(
            events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs,
        );

        assert!(
            matches!(result.refresh, RefreshTrigger::SingleFile(_)),
            "single source event without pending VCS should use SingleFile, got {:?}",
            result.refresh,
        );
    }

    #[test]
    fn test_internal_only_events_still_delayed() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        timers.last_refresh_completed = Some(Instant::now() - Duration::from_secs(5));
        let vcs = StubVcs::new(temp.path().to_path_buf());

        let events = vec![DebouncedEvent::new(
            git_dir.join("index"),
            DebouncedEventKind::Any,
        )];
        let result = handle_file_change(
            events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs,
        );

        assert_eq!(result.refresh, RefreshTrigger::None);
        assert!(
            timers.pending_vcs_event.is_some(),
            "Internal-only events should be delayed via pending_vcs_event"
        );
    }

    #[test]
    fn test_vcs_paths_bypass_gitignore_filter() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let jj_dir = temp.path().join(".jj");
        std::fs::create_dir_all(jj_dir.join("working_copy")).unwrap();
        std::fs::create_dir_all(temp.path().join(".git")).unwrap();
        std::fs::create_dir_all(temp.path().join(".git/info")).unwrap();
        std::fs::write(temp.path().join(".gitignore"), ".jj/\n").unwrap();

        let mut app = TestAppBuilder::new().build();
        app.gitignore_filter = crate::gitignore::GitignoreFilter::new(temp.path());
        let mut refresh_state = RefreshState::Idle;
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        timers.last_refresh_completed = Some(Instant::now() - Duration::from_secs(5));
        let vcs = StubVcs::new(temp.path().to_path_buf());

        let events = vec![DebouncedEvent::new(
            temp.path().join(".git/HEAD"),
            DebouncedEventKind::Any,
        )];
        let result = handle_file_change(
            events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs,
        );

        assert_eq!(
            result.refresh,
            RefreshTrigger::Full,
            ".git/HEAD should bypass gitignore filter and trigger refresh"
        );
    }

    #[test]
    fn test_single_file_cooldown_suppresses_post_completion_vcs_event() {
        use tempfile::TempDir;

        use crate::message::RefreshOutcome;
        use crate::update::refresh::handle_refresh;
        use crate::update::UpdateConfig;

        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            cancel_flag: Arc::new(AtomicBool::new(false)),
            started_at: Instant::now(),
        };
        let mut vcs_lock = VcsLockState::default();
        let mut timers = Timers::default();
        let vcs = StubVcs::new(temp.path().to_path_buf());
        let config = UpdateConfig::default();

        // Step 1: SingleFile refresh completes (sets cooldown timer)
        let outcome = RefreshOutcome::SingleFile {
            path: "src/main.rs".to_string(),
            diff: None,
            revision_id: None,
        };
        let refresh_result = handle_refresh(
            outcome, &mut app, &mut refresh_state, &mut timers, &config, &vcs,
        );
        assert_eq!(refresh_result.refresh, RefreshTrigger::None);
        assert!(timers.last_refresh_completed.is_some());

        // Step 2: Self-triggered VCS event arrives within cooldown window
        // (simulates jj auto-snapshot event arriving after SingleFile completes)
        let events = vec![DebouncedEvent::new(
            git_dir.join("HEAD"),
            DebouncedEventKind::Any,
        )];
        let result = handle_file_change(
            events, &mut app, &mut refresh_state, &mut vcs_lock, &mut timers, &vcs,
        );

        assert_eq!(
            result.refresh,
            RefreshTrigger::None,
            "VCS event within cooldown after SingleFile should be suppressed"
        );
    }
}
