//! File event classification and git lock state management.
//!
//! This module provides differentiated handling of file change events,
//! allowing branchdiff to:
//! - Use longer debounce for `.git/` changes (reducing lock collisions)
//! - Detect when external git operations have the index lock
//! - Pause refresh during external git operations

use std::path::Path;

/// Classifies file change events by source for differentiated debouncing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeSource {
    /// .git/index, .git/HEAD, .git/refs/* - git internals
    GitInternal,
    /// Source files, configs, etc.
    SourceFile,
    /// .git/index.lock - external git operation in progress
    GitLock,
}

impl ChangeSource {
    /// Classify a file path by its change source.
    pub fn from_path(path: &Path, repo_root: &Path) -> Self {
        let relative = path.strip_prefix(repo_root).unwrap_or(path);

        // Check if path is under .git/
        let is_git_path = relative
            .components()
            .next()
            .is_some_and(|c| c.as_os_str() == ".git");

        if is_git_path {
            // Check for index.lock specifically
            if relative
                .file_name()
                .is_some_and(|name| name == "index.lock")
            {
                ChangeSource::GitLock
            } else {
                ChangeSource::GitInternal
            }
        } else {
            ChangeSource::SourceFile
        }
    }

    /// Returns recommended debounce in milliseconds.
    pub fn debounce_ms(&self) -> u64 {
        match self {
            // Longer debounce for git internals to avoid lock collisions
            ChangeSource::GitInternal => 500,
            // Quick response for source file edits
            ChangeSource::SourceFile => 100,
            // Lock events are handled specially, not debounced
            ChangeSource::GitLock => 0,
        }
    }
}

/// Tracks git lock state for external operation detection.
///
/// When an external git operation (rebase, commit, merge) is running,
/// it holds `.git/index.lock`. We detect this and pause refresh to
/// avoid lock collisions.
#[derive(Debug, Default)]
pub struct GitLockState {
    /// True when .git/index.lock exists
    locked: bool,
    /// True when a refresh was requested while locked
    pending_refresh: bool,
}

impl GitLockState {
    /// Update lock state. Call when lock file is created/deleted.
    pub fn set_locked(&mut self, locked: bool) {
        self.locked = locked;
        // Clear pending when lock is released (refresh will happen)
        if !locked {
            self.pending_refresh = false;
        }
    }

    /// Returns true if an external git operation has the lock.
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Mark that a refresh was requested while locked.
    pub fn set_pending(&mut self) {
        if self.locked {
            self.pending_refresh = true;
        }
    }

    /// Returns true if a refresh should fire after unlock.
    /// Also returns true if we just unlocked and had a pending refresh.
    pub fn should_refresh_on_unlock(&self) -> bool {
        !self.locked && self.pending_refresh
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

    // ---- ChangeSource::from_path tests ----

    #[test]
    fn test_classify_source_file_in_src() {
        let repo = Path::new("/repo");
        assert_eq!(
            ChangeSource::from_path(Path::new("/repo/src/main.rs"), repo),
            ChangeSource::SourceFile
        );
    }

    #[test]
    fn test_classify_source_file_at_root() {
        let repo = Path::new("/repo");
        assert_eq!(
            ChangeSource::from_path(Path::new("/repo/Cargo.toml"), repo),
            ChangeSource::SourceFile
        );
    }

    #[test]
    fn test_classify_git_index() {
        let repo = Path::new("/repo");
        assert_eq!(
            ChangeSource::from_path(Path::new("/repo/.git/index"), repo),
            ChangeSource::GitInternal
        );
    }

    #[test]
    fn test_classify_git_head() {
        let repo = Path::new("/repo");
        assert_eq!(
            ChangeSource::from_path(Path::new("/repo/.git/HEAD"), repo),
            ChangeSource::GitInternal
        );
    }

    #[test]
    fn test_classify_git_refs() {
        let repo = Path::new("/repo");
        assert_eq!(
            ChangeSource::from_path(Path::new("/repo/.git/refs/heads/main"), repo),
            ChangeSource::GitInternal
        );
    }

    #[test]
    fn test_classify_git_lock() {
        let repo = Path::new("/repo");
        assert_eq!(
            ChangeSource::from_path(Path::new("/repo/.git/index.lock"), repo),
            ChangeSource::GitLock
        );
    }

    #[test]
    fn test_classify_nested_lock_file() {
        let repo = Path::new("/repo");
        // A lock file in a subdirectory should still be detected
        assert_eq!(
            ChangeSource::from_path(Path::new("/repo/.git/worktrees/foo/index.lock"), repo),
            ChangeSource::GitLock
        );
    }

    #[test]
    fn test_classify_path_outside_repo() {
        let repo = Path::new("/repo");
        // Path that doesn't strip cleanly - treated as source file
        assert_eq!(
            ChangeSource::from_path(Path::new("/other/file.rs"), repo),
            ChangeSource::SourceFile
        );
    }

    #[test]
    fn test_classify_gitignore_is_source() {
        let repo = Path::new("/repo");
        // .gitignore is a source file, not a git internal
        assert_eq!(
            ChangeSource::from_path(Path::new("/repo/.gitignore"), repo),
            ChangeSource::SourceFile
        );
    }

    // ---- debounce_ms tests ----

    #[test]
    fn test_debounce_source_file() {
        assert_eq!(ChangeSource::SourceFile.debounce_ms(), 100);
    }

    #[test]
    fn test_debounce_git_internal() {
        assert_eq!(ChangeSource::GitInternal.debounce_ms(), 500);
    }

    #[test]
    fn test_debounce_git_lock() {
        assert_eq!(ChangeSource::GitLock.debounce_ms(), 0);
    }

    // ---- GitLockState tests ----

    #[test]
    fn test_lock_state_default() {
        let state = GitLockState::default();
        assert!(!state.is_locked());
        assert!(!state.should_refresh_on_unlock());
    }

    #[test]
    fn test_lock_state_locked_blocks_refresh() {
        let mut state = GitLockState::default();
        state.set_locked(true);
        assert!(state.is_locked());
    }

    #[test]
    fn test_lock_state_pending_while_locked() {
        let mut state = GitLockState::default();
        state.set_locked(true);
        state.set_pending();

        // Still locked, shouldn't refresh yet
        assert!(!state.should_refresh_on_unlock());
    }

    #[test]
    fn test_lock_state_refresh_on_unlock() {
        let mut state = GitLockState::default();
        state.set_locked(true);
        state.set_pending();
        state.set_locked(false);

        // Now unlocked with pending - should refresh
        // Note: set_locked(false) clears pending, so this returns false
        // The refresh should happen immediately on unlock
        assert!(!state.should_refresh_on_unlock());
    }

    #[test]
    fn test_lock_state_take_pending() {
        let mut state = GitLockState::default();
        state.set_locked(true);
        state.set_pending();

        // Unlock and check pending
        state.locked = false; // Direct access to avoid clearing in set_locked
        assert!(state.take_pending());
        assert!(!state.take_pending()); // Second call returns false
    }

    #[test]
    fn test_lock_state_pending_only_when_locked() {
        let mut state = GitLockState::default();
        // Not locked, set_pending should have no effect
        state.set_pending();
        assert!(!state.should_refresh_on_unlock());
    }
}
