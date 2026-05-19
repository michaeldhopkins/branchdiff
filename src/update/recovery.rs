//! Classification of VCS errors and the recovery actions branchdiff can offer.
//!
//! The runtime distinguishes three classes:
//! - **Transient** (e.g. `.lock` contention): auto-retry with backoff.
//! - **Actionable** (e.g. jj stale working copy): not self-healing — surface a
//!   one-key fix in the banner. The file watcher still auto-recovers if the
//!   user resolves the condition externally.
//! - **Permanent**: just display; no retry, no offered fix.

/// A concrete recovery command branchdiff knows how to run on the user's behalf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// `jj workspace update-stale` — reconciles a working copy whose recorded
    /// operation lags behind the current op log head.
    JjUpdateStale,
}

/// What to render in the banner alongside the error message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryHint {
    /// Suggested action.
    pub action: RecoveryAction,
    /// Single-character key the user presses to run it.
    pub key_hint: char,
    /// Human-readable command shown next to the key hint.
    pub command_label: &'static str,
}

impl RecoveryHint {
    pub const fn jj_update_stale() -> Self {
        Self {
            action: RecoveryAction::JjUpdateStale,
            key_hint: 'u',
            command_label: "jj workspace update-stale",
        }
    }
}

/// Outcome of classifying a refresh error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Worth auto-retrying with backoff (lock contention, etc.).
    Transient,
    /// Won't self-heal; offer the user a fix.
    Actionable(RecoveryHint),
    /// Display and stop — no retry, no offered fix.
    Permanent,
}

/// Classify a flattened error message from the refresh pipeline.
///
/// The thread boundary in `spawn_refresh` flattens `anyhow::Error` to a string,
/// so we match on the formatted text rather than a structured error type. The
/// upstream `vcs_runner::is_transient_error` does the same.
pub fn classify_error(msg: &str) -> ErrorClass {
    if msg.contains("working copy is stale") || msg.contains("workspace update-stale") {
        return ErrorClass::Actionable(RecoveryHint::jj_update_stale());
    }
    if msg.contains(".lock") {
        return ErrorClass::Transient;
    }
    ErrorClass::Permanent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_message_is_actionable_with_update_stale_hint() {
        let msg = "Error: The working copy is stale (not updated since operation 26a0bbff5afe). \
                   Hint: Run `jj workspace update-stale` to update it.";
        match classify_error(msg) {
            ErrorClass::Actionable(hint) => {
                assert_eq!(hint.action, RecoveryAction::JjUpdateStale);
                assert_eq!(hint.key_hint, 'u');
            }
            other => panic!("expected Actionable, got {other:?}"),
        }
    }

    #[test]
    fn stale_hint_alone_is_enough_to_trigger_actionable() {
        // Some jj versions phrase the error without "working copy is stale"
        // but still include the hint line.
        let msg = "Run `jj workspace update-stale` to update it.";
        assert!(matches!(classify_error(msg), ErrorClass::Actionable(_)));
    }

    #[test]
    fn lock_message_is_transient() {
        let msg = "could not acquire .git/index.lock";
        assert_eq!(classify_error(msg), ErrorClass::Transient);
    }

    #[test]
    fn other_errors_are_permanent() {
        assert_eq!(classify_error("no such revision"), ErrorClass::Permanent);
        assert_eq!(classify_error(""), ErrorClass::Permanent);
        assert_eq!(classify_error("Config error: missing setting"), ErrorClass::Permanent);
    }

    #[test]
    fn stale_takes_precedence_over_lock_if_both_mentioned() {
        // Defensive: if some future jj error embeds both, the actionable
        // classification should win so we don't pointlessly retry.
        let msg = "working copy is stale (also: .lock present)";
        assert!(matches!(classify_error(msg), ErrorClass::Actionable(_)));
    }

    #[test]
    fn recovery_hint_constructor_is_consistent() {
        let hint = RecoveryHint::jj_update_stale();
        assert_eq!(hint.action, RecoveryAction::JjUpdateStale);
        assert_eq!(hint.command_label, "jj workspace update-stale");
        assert_eq!(hint.key_hint, 'u');
    }
}
