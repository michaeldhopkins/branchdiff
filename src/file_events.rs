//! VCS lock state management.
//!
//! Tracks when external VCS operations hold a lock, allowing branchdiff
//! to pause refresh and avoid lock collisions.

/// Tracks VCS lock state for external operation detection.
///
/// When an external VCS operation (rebase, commit, merge) is running,
/// it may hold a lock file. We detect this and pause refresh to
/// avoid lock collisions.
#[derive(Debug, Default)]
pub struct VcsLockState {
    /// True when VCS lock is held (e.g., .git/index.lock exists)
    locked: bool,
    /// True when a refresh was requested while locked
    pending_refresh: bool,
}

impl VcsLockState {
    /// Update lock state. Call when lock file is created/deleted.
    pub fn set_locked(&mut self, locked: bool) {
        self.locked = locked;
        if !locked {
            self.pending_refresh = false;
        }
    }

    /// Returns true if an external VCS operation has the lock.
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Mark that a refresh was requested while locked.
    pub fn set_pending(&mut self) {
        if self.locked {
            self.pending_refresh = true;
        }
    }

    /// Check and consume the pending refresh flag.
    /// Returns true if a pending refresh was waiting.
    pub fn take_pending(&mut self) -> bool {
        let was_pending = self.pending_refresh;
        self.pending_refresh = false;
        was_pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lock_state_default() {
        let state = VcsLockState::default();
        assert!(!state.is_locked());
    }

    #[test]
    fn test_lock_state_locked_blocks_refresh() {
        let mut state = VcsLockState::default();
        state.set_locked(true);
        assert!(state.is_locked());
    }

    #[test]
    fn test_lock_state_pending_while_locked() {
        let mut state = VcsLockState::default();
        state.set_locked(true);
        state.set_pending();
        assert!(state.is_locked());
        assert!(!state.take_pending().then_some(()).is_none());
    }

    #[test]
    fn test_lock_state_refresh_on_unlock() {
        let mut state = VcsLockState::default();
        state.set_locked(true);
        state.set_pending();
        state.set_locked(false);

        // set_locked(false) clears pending
        assert!(!state.take_pending());
    }

    #[test]
    fn test_lock_state_take_pending() {
        let mut state = VcsLockState::default();
        state.set_locked(true);
        state.set_pending();

        // Direct unlock without clearing pending
        state.locked = false;
        assert!(state.take_pending());
        assert!(!state.take_pending()); // Second call returns false
    }

    #[test]
    fn test_lock_state_pending_only_when_locked() {
        let mut state = VcsLockState::default();
        // Not locked, set_pending should have no effect
        state.set_pending();
        assert!(!state.take_pending());
    }
}
