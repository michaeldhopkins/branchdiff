//! Unified message types for application events.
//!
//! All application events flow through a single Message enum, enabling
//! centralized state management and easier testing.

use std::path::PathBuf;

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
    /// New merge base if it changed after fetch.
    pub new_merge_base: Option<String>,
}

/// Result of a background refresh operation.
#[derive(Debug)]
pub enum RefreshOutcome {
    /// Full refresh completed successfully.
    Success(RefreshResult),
    /// Single file refresh completed.
    SingleFile { path: String, diff: Option<FileDiff>, revision_id: Option<String> },
    /// Refresh was cancelled (e.g., by watchdog restart). Not a user-facing error.
    Cancelled,
    /// Refresh failed with an error.
    Error(String),
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

/// Result of processing a message.
#[derive(Debug, Default)]
pub struct UpdateResult {
    /// Whether to continue or quit.
    pub loop_action: LoopAction,
    /// What type of refresh to trigger.
    pub refresh: RefreshTrigger,
    /// Should trigger a fetch.
    pub trigger_fetch: bool,
    /// Whether the UI needs to be redrawn.
    pub needs_redraw: bool,
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
}
