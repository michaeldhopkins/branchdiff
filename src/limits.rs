//! Resource limit detection and threshold checking for large repos.
//!
//! This module provides automatic detection of system resource limits
//! and generates warnings when thresholds are exceeded.

use crate::message::FALLBACK_REFRESH_SECS;

/// System resource limits detected at startup.
#[derive(Debug, Clone)]
pub struct SystemLimits {
    /// Soft limit on file descriptors (from getrlimit)
    pub fd_soft_limit: usize,
    /// Recommended maximum watches (50% of soft limit)
    pub max_recommended_watches: usize,
}

impl SystemLimits {
    /// Detect system limits at startup.
    pub fn detect() -> Self {
        let fd_soft_limit = get_fd_soft_limit();
        Self {
            fd_soft_limit,
            // Use 50% of soft limit for watches, leaving room for git processes and file reads
            max_recommended_watches: fd_soft_limit / 2,
        }
    }

    /// Check if watch metrics exceed thresholds and return a warning message.
    pub fn check_watch_warning(&self, metrics: &WatcherMetrics) -> Option<String> {
        if metrics.skipped_count > 0 || metrics.directory_count > self.max_recommended_watches {
            Some(format!("Large repo: refreshing every {}s", FALLBACK_REFRESH_SECS))
        } else {
            None
        }
    }
}

/// Get the soft limit on file descriptors.
#[cfg(unix)]
fn get_fd_soft_limit() -> usize {
    use std::mem::MaybeUninit;

    let mut rlim = MaybeUninit::<libc::rlimit>::uninit();
    // SAFETY: rlim is a valid pointer to uninitialized memory of the correct size
    let result = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, rlim.as_mut_ptr()) };

    if result == 0 {
        // SAFETY: getrlimit succeeded, so rlim is now initialized
        let rlim = unsafe { rlim.assume_init() };
        // Cap at reasonable maximum to avoid overflow issues
        (rlim.rlim_cur as usize).min(100_000)
    } else {
        // Fallback if getrlimit fails (256 is a conservative Unix default)
        256
    }
}

#[cfg(not(unix))]
fn get_fd_soft_limit() -> usize {
    // Windows doesn't have the same fd limits; use generous default
    8192
}

/// Detect if running under Windows Subsystem for Linux.
/// WSL's inotify implementation is unreliable, so we use polling instead.
pub fn is_wsl() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/version")
            .map(|v| v.to_lowercase().contains("microsoft"))
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Thresholds for diff processing warnings.
#[derive(Debug, Clone)]
pub struct DiffThresholds {
    /// Warn if total diff lines exceed this
    pub warn_line_count: usize,
    /// Warn if file count exceeds this
    pub warn_file_count: usize,
}

impl Default for DiffThresholds {
    fn default() -> Self {
        Self {
            warn_line_count: 50_000,
            warn_file_count: 500,
        }
    }
}

impl DiffThresholds {
    /// Check if diff metrics exceed thresholds and return a warning message.
    pub fn check_diff_warning(&self, metrics: &DiffMetrics) -> Option<String> {
        if metrics.total_lines > self.warn_line_count {
            let k = metrics.total_lines / 1000;
            Some(format!("Large diff: {}k lines", k))
        } else if metrics.file_count > self.warn_file_count {
            Some(format!("Many files: {}", metrics.file_count))
        } else {
            None
        }
    }
}

/// Metrics collected during file watcher setup.
#[derive(Debug, Clone, Default)]
pub struct WatcherMetrics {
    /// Total number of directories found
    pub directory_count: usize,
    /// Number of directories that couldn't be watched (beyond limit)
    pub skipped_count: usize,
}

/// Metrics collected during diff computation.
#[derive(Debug, Clone, Default)]
pub struct DiffMetrics {
    /// Total number of lines in the diff
    pub total_lines: usize,
    /// Number of changed files
    pub file_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_limits_detect_returns_sane_values() {
        let limits = SystemLimits::detect();
        // Should be at least 64 on any modern system
        assert!(limits.fd_soft_limit >= 64);
        assert!(limits.max_recommended_watches > 0);
        assert!(limits.max_recommended_watches <= limits.fd_soft_limit);
    }

    #[test]
    fn test_max_recommended_is_half_of_soft_limit() {
        let limits = SystemLimits::detect();
        assert_eq!(limits.max_recommended_watches, limits.fd_soft_limit / 2);
    }

    #[test]
    fn test_watch_warning_below_threshold() {
        let limits = SystemLimits {
            fd_soft_limit: 256,
            max_recommended_watches: 128,
        };
        let metrics = WatcherMetrics {
            directory_count: 50,
            skipped_count: 0,
        };
        assert!(limits.check_watch_warning(&metrics).is_none());
    }

    #[test]
    fn test_watch_warning_above_threshold_no_skipped() {
        let limits = SystemLimits {
            fd_soft_limit: 256,
            max_recommended_watches: 128,
        };
        let metrics = WatcherMetrics {
            directory_count: 200,
            skipped_count: 0,
        };
        let warning = limits.check_watch_warning(&metrics);
        assert!(warning.is_some());
        assert_eq!(warning.unwrap(), "Large repo: refreshing every 5s");
    }

    #[test]
    fn test_watch_warning_with_skipped() {
        let limits = SystemLimits {
            fd_soft_limit: 256,
            max_recommended_watches: 128,
        };
        let metrics = WatcherMetrics {
            directory_count: 200,
            skipped_count: 72,
        };
        let warning = limits.check_watch_warning(&metrics);
        assert!(warning.is_some());
        assert_eq!(warning.unwrap(), "Large repo: refreshing every 5s");
    }

    #[test]
    fn test_diff_thresholds_default() {
        let thresholds = DiffThresholds::default();
        assert_eq!(thresholds.warn_line_count, 50_000);
        assert_eq!(thresholds.warn_file_count, 500);
    }

    #[test]
    fn test_diff_warning_small_diff() {
        let thresholds = DiffThresholds::default();
        let metrics = DiffMetrics {
            total_lines: 100,
            file_count: 5,
        };
        assert!(thresholds.check_diff_warning(&metrics).is_none());
    }

    #[test]
    fn test_diff_warning_large_line_count() {
        let thresholds = DiffThresholds::default();
        let metrics = DiffMetrics {
            total_lines: 60_000,
            file_count: 10,
        };
        let warning = thresholds.check_diff_warning(&metrics);
        assert!(warning.is_some());
        let msg = warning.unwrap();
        assert!(msg.contains("60k"));
        assert!(msg.contains("lines"));
    }

    #[test]
    fn test_diff_warning_many_files() {
        let thresholds = DiffThresholds::default();
        let metrics = DiffMetrics {
            total_lines: 1000,
            file_count: 600,
        };
        let warning = thresholds.check_diff_warning(&metrics);
        assert!(warning.is_some());
        let msg = warning.unwrap();
        assert!(msg.contains("600"));
        assert!(msg.contains("files"));
    }

    #[test]
    fn test_diff_warning_line_count_takes_precedence() {
        let thresholds = DiffThresholds::default();
        let metrics = DiffMetrics {
            total_lines: 60_000,
            file_count: 600,
        };
        let warning = thresholds.check_diff_warning(&metrics);
        assert!(warning.is_some());
        // Line count warning should take precedence
        assert!(warning.unwrap().contains("lines"));
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn test_is_wsl_returns_false_on_non_linux() {
        // On macOS/Windows, is_wsl() should always return false
        assert!(!super::is_wsl());
    }

    // Note: No test for Linux because we can't control whether we're actually
    // in WSL. The function reads /proc/version which we can't mock without
    // significant complexity. The non-Linux test above provides coverage.
}
