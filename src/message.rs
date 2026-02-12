//! Unified message types for application events.
//!
//! All application events flow through a single Message enum, enabling
//! centralized state management and easier testing.

use std::path::PathBuf;

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
    SingleFile { path: String, diff: Option<FileDiff> },
    /// Refresh was cancelled.
    Cancelled,
}

/// Unified message type for all application events.
#[derive(Debug)]
pub enum Message {
    /// User input (keyboard, mouse).
    Input(AppAction),
    /// Background refresh completed.
    RefreshCompleted(RefreshOutcome),
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

/// Whether to continue or quit the event loop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LoopAction {
    /// Continue running.
    #[default]
    Continue,
    /// Exit the application.
    Quit,
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
        let _refresh = Message::RefreshCompleted(RefreshOutcome::Cancelled);
        let _file = Message::FileChanged(vec![]);
        let _fetch = Message::FetchCompleted(FetchResult {
            has_conflicts: false,
            new_merge_base: None,
        });
        let _tick = Message::Tick;
    }

    #[test]
    fn test_refresh_outcome_variants() {
        let _cancelled = RefreshOutcome::Cancelled;
        let _single = RefreshOutcome::SingleFile {
            path: "test.rs".to_string(),
            diff: None,
        };
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
