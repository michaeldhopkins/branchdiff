//! Unified message types for application events.
//!
//! All application events flow through a single Message enum, enabling
//! centralized state management and easier testing.

use std::path::PathBuf;
use std::sync::mpsc;

use crossterm::event::Event;
use notify_debouncer_mini::DebouncedEvent;

use crate::vcs::RefreshResult;
use crate::diff::FileDiff;

/// User input actions (from keyboard/mouse).
/// Re-exported from input module for convenience.
pub use crate::input::AppAction;

/// Fallback refresh interval in seconds for large repos where file watching is limited.
pub const FALLBACK_REFRESH_SECS: u64 = 5;

/// Result of a background fetch operation.
#[derive(Debug)]
pub struct FetchResult {
    /// Whether the remote has conflicting changes.
    pub has_conflicts: bool,
    /// The base as it stands after the fetch, when the fetch moved it.
    pub new_merge_base: Option<String>,
}

/// Result of a background refresh operation.
#[derive(Debug)]
pub enum RefreshOutcome {
    /// Full refresh completed successfully.
    Success(Box<RefreshResult>),
    /// Single file refresh completed.
    SingleFile { path: String, diff: Option<FileDiff>, revision_id: Option<String> },
    /// Refresh was cancelled (e.g., by watchdog restart). Not a user-facing error.
    Cancelled,
    /// Refresh failed with an error.
    Error(String),
}

impl RefreshOutcome {
    pub fn success(result: RefreshResult) -> Self {
        Self::Success(Box::new(result))
    }
}

/// Unified message type for all application events.
#[derive(Debug)]
pub enum Message {
    /// User input (keyboard, mouse).
    Input(AppAction),
    /// Raw input event routed to the search handler when search bar is active.
    SearchInput(Event),
    /// Background refresh completed.
    RefreshCompleted(Box<RefreshOutcome>),
    /// File system change detected.
    FileChanged(Vec<DebouncedEvent>),
    /// Remote fetch completed.
    FetchCompleted(FetchResult),
    /// Periodic tick (handles timer-based logic).
    Tick,
}

/// What type of refresh to trigger (if any).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RefreshTrigger {
    /// No refresh needed.
    #[default]
    None,
    /// Full refresh of all files.
    Full,
    /// Refresh only this specific file.
    SingleFile(PathBuf),
}

/// Whether to continue, quit, or restart the event loop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LoopAction {
    /// Continue running.
    #[default]
    Continue,
    /// Exit the application.
    Quit,
    /// Re-detect VCS backend and restart (e.g., after jj init or .jj removal).
    RestartVcs,
}

/// What the editor should be pointed at, set by an input handler and launched by
/// the main loop (which owns the terminal it may need to suspend).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenTarget {
    /// Absolute path of a single file to open.
    File(PathBuf),
    /// The repository root, opened as a folder/project.
    Repo,
}

/// Result of processing a message.
#[derive(Debug, Default)]
pub struct UpdateResult {
    /// Whether to continue or quit.
    pub loop_action: LoopAction,
    /// What type of refresh to trigger.
    pub refresh: RefreshTrigger,
    /// Should trigger a fetch.
    pub trigger_fetch: bool,
    /// Whether to spawn the recovery action that was offered in the banner.
    pub trigger_recovery: Option<crate::update::RecoveryAction>,
    /// What to open in the editor, if anything (launched by the main loop).
    pub open_editor: Option<OpenTarget>,
    /// Whether the UI needs to be redrawn.
    pub needs_redraw: bool,
    /// How much of the screen the next draw must rewrite.
    pub repaint: Repaint,
}

/// How much of the screen the next draw must rewrite.
///
/// ratatui writes only the cells that differ from its in-memory copy of the
/// previous frame. That is right until the terminal's contents change without
/// us — display sleep, the terminal repainting itself, returning from a
/// full-screen editor — at which point that copy is a lie and a diffed draw
/// leaves stale cells on screen.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Repaint {
    /// Diff against the previous frame; write only what changed.
    #[default]
    Diff,
    /// Drop the previous-frame buffer first, so every cell is rewritten.
    Full,
}

impl LoopAction {
    /// Whether the event loop stops here, to quit or to restart on a newly detected VCS.
    pub fn ends_loop(self) -> bool {
        matches!(self, Self::Quit | Self::RestartVcs)
    }
}

/// File-watcher output as the debouncer delivers it.
pub type FileEvents = mpsc::Receiver<Result<Vec<DebouncedEvent>, notify::Error>>;

/// Gather what happened since the last loop iteration, always ending with a `Tick`.
///
/// `input` is the terminal event the caller polled for, if any, so that this
/// never touches the terminal itself. The channels are drained without blocking.
pub fn collect_messages(
    input: Option<Event>,
    file_events: &FileEvents,
    refresh_rx: &mpsc::Receiver<RefreshOutcome>,
    fetch_rx: &mpsc::Receiver<FetchResult>,
    search_input_active: bool,
) -> Vec<Message> {
    let mut messages = Vec::new();

    if let Some(event) = input {
        if search_input_active {
            messages.push(Message::SearchInput(event));
        } else {
            let action = crate::input::handle_event(event);
            if action != AppAction::None {
                messages.push(Message::Input(action));
            }
        }
    }

    if let Ok(outcome) = refresh_rx.try_recv() {
        messages.push(Message::RefreshCompleted(Box::new(outcome)));
    }

    if let Ok(Ok(events)) = file_events.try_recv()
        && !events.is_empty()
    {
        messages.push(Message::FileChanged(events));
    }

    if let Ok(result) = fetch_rx.try_recv() {
        messages.push(Message::FetchCompleted(result));
    }

    messages.push(Message::Tick);
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_update_result_default() {
        let result = UpdateResult::default();
        assert_eq!(result.loop_action, LoopAction::Continue);
        assert_eq!(result.refresh, RefreshTrigger::None);
        assert!(!result.trigger_fetch);
        assert!(!result.needs_redraw);
    }

    #[test]
    fn test_message_variants() {
        // Verify all message variants can be constructed
        let _input = Message::Input(AppAction::Quit);
        let _refresh = Message::RefreshCompleted(Box::new(RefreshOutcome::Error("test".to_string())));
        let _file = Message::FileChanged(vec![]);
        let _fetch = Message::FetchCompleted(FetchResult {
            has_conflicts: false,
            new_merge_base: None,
        });
        let _tick = Message::Tick;
    }

    #[test]
    fn test_refresh_outcome_variants() {
        let _single = RefreshOutcome::SingleFile {
            path: "test.rs".to_string(),
            diff: None,
            revision_id: None,
        };
        let _cancelled = RefreshOutcome::Cancelled;
        let _error = RefreshOutcome::Error("something failed".to_string());
    }

    #[test]
    fn test_fetch_result_with_conflicts() {
        let result = FetchResult {
            has_conflicts: true,
            new_merge_base: Some("abc123".to_string()),
        };
        assert!(result.has_conflicts);
        assert_eq!(result.new_merge_base, Some("abc123".to_string()));
    }

    #[test]
    fn only_quit_and_restart_end_the_loop() {
        assert!(!LoopAction::Continue.ends_loop());
        assert!(LoopAction::Quit.ends_loop());
        assert!(LoopAction::RestartVcs.ends_loop());
    }

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use notify_debouncer_mini::DebouncedEventKind;
    use std::path::Path;

    /// The four sources a loop iteration drains, with their senders kept alive
    /// so an empty channel reads as "nothing yet" rather than "disconnected".
    struct Sources {
        file_tx: mpsc::Sender<Result<Vec<DebouncedEvent>, notify::Error>>,
        file_rx: FileEvents,
        refresh_tx: mpsc::Sender<RefreshOutcome>,
        refresh_rx: mpsc::Receiver<RefreshOutcome>,
        fetch_tx: mpsc::Sender<FetchResult>,
        fetch_rx: mpsc::Receiver<FetchResult>,
    }

    impl Sources {
        fn new() -> Self {
            let (file_tx, file_rx) = mpsc::channel();
            let (refresh_tx, refresh_rx) = mpsc::channel();
            let (fetch_tx, fetch_rx) = mpsc::channel();
            Self { file_tx, file_rx, refresh_tx, refresh_rx, fetch_tx, fetch_rx }
        }

        fn collect(&self, input: Option<Event>, search_input_active: bool) -> Vec<Message> {
            collect_messages(input, &self.file_rx, &self.refresh_rx, &self.fetch_rx, search_input_active)
        }
    }

    fn key(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    fn file_event(path: &str) -> DebouncedEvent {
        DebouncedEvent::new(PathBuf::from(path), DebouncedEventKind::Any)
    }

    #[test]
    fn a_quiet_iteration_still_ticks() {
        let messages = Sources::new().collect(None, false);
        assert!(matches!(messages.as_slice(), [Message::Tick]), "got {messages:?}");
    }

    #[test]
    fn a_key_becomes_its_action_before_the_tick() {
        let messages = Sources::new().collect(Some(key('q')), false);
        assert!(
            matches!(messages.as_slice(), [Message::Input(AppAction::Quit), Message::Tick]),
            "got {messages:?}"
        );
    }

    #[test]
    fn an_event_with_no_action_is_dropped() {
        let messages = Sources::new().collect(Some(Event::FocusLost), false);
        assert!(matches!(messages.as_slice(), [Message::Tick]), "got {messages:?}");
    }

    #[test]
    fn while_searching_the_raw_event_goes_to_the_search_box() {
        let messages = Sources::new().collect(Some(key('q')), true);
        assert!(
            matches!(messages.as_slice(), [Message::SearchInput(e), Message::Tick] if *e == key('q')),
            "got {messages:?}"
        );
    }

    #[test]
    fn every_ready_source_is_drained_in_order() {
        let sources = Sources::new();
        sources.refresh_tx.send(RefreshOutcome::Cancelled).unwrap();
        sources.file_tx.send(Ok(vec![file_event("/repo/a.rs")])).unwrap();
        sources.fetch_tx.send(FetchResult { has_conflicts: true, new_merge_base: None }).unwrap();

        let messages = sources.collect(Some(key('q')), false);
        assert!(
            matches!(
                messages.as_slice(),
                [
                    Message::Input(AppAction::Quit),
                    Message::RefreshCompleted(outcome),
                    Message::FileChanged(events),
                    Message::FetchCompleted(FetchResult { has_conflicts: true, .. }),
                    Message::Tick,
                ] if matches!(**outcome, RefreshOutcome::Cancelled)
                    && events.len() == 1
                    && events[0].path == Path::new("/repo/a.rs")
            ),
            "got {messages:?}"
        );
    }

    #[test]
    fn an_empty_or_failed_file_batch_is_not_a_change() {
        let sources = Sources::new();
        sources.file_tx.send(Ok(vec![])).unwrap();
        assert!(matches!(sources.collect(None, false).as_slice(), [Message::Tick]));

        sources.file_tx.send(Err(notify::Error::generic("watch failed"))).unwrap();
        assert!(matches!(sources.collect(None, false).as_slice(), [Message::Tick]));
    }

    #[test]
    fn each_channel_yields_one_message_per_iteration() {
        let sources = Sources::new();
        sources.refresh_tx.send(RefreshOutcome::Cancelled).unwrap();
        sources.refresh_tx.send(RefreshOutcome::Error("second".to_string())).unwrap();

        let first = sources.collect(None, false);
        assert!(matches!(first.as_slice(), [Message::RefreshCompleted(o), Message::Tick] if matches!(**o, RefreshOutcome::Cancelled)));
        let second = sources.collect(None, false);
        assert!(matches!(second.as_slice(), [Message::RefreshCompleted(o), Message::Tick] if matches!(**o, RefreshOutcome::Error(_))));
    }
}
