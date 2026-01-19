//! Resource limit detection and threshold checking for large repos.
//!
//! This module provides platform-specific detection of file watching limits
//! and generates warnings when thresholds are exceeded.

use crate::message::FALLBACK_REFRESH_SECS;

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

// =============================================================================
// Linux-specific: inotify limit detection
// =============================================================================

/// Parse an inotify sysctl value from its string representation.
/// Returns None if the string is empty or not a valid number.
#[cfg(target_os = "linux")]
pub fn parse_inotify_value(s: &str) -> Option<usize> {
    s.trim().parse().ok()
}

/// Read the inotify max_user_watches limit from /proc/sys.
/// Returns None if the file can't be read or parsed.
#[cfg(target_os = "linux")]
fn read_inotify_max_watches() -> Option<usize> {
    let content = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches").ok()?;
    parse_inotify_value(&content)
}

/// Get the recommended watch limit for Linux.
/// Uses 50% of the inotify limit since it's shared across all user applications.
/// Returns None on WSL (which uses PollWatcher with no limit).
#[cfg(target_os = "linux")]
pub fn get_watch_limit() -> Option<usize> {
    if is_wsl() {
        return None; // PollWatcher has no limit
    }
    // Read the real inotify limit, use 50% (shared with VS Code, IDE, etc.)
    let max = read_inotify_max_watches().unwrap_or(8192);
    Some(max / 2)
}

/// macOS and Windows use native recursive watching with no practical per-app limit.
#[cfg(not(target_os = "linux"))]
pub fn get_watch_limit() -> Option<usize> {
    None
}

// =============================================================================
// Watch metrics and warnings (Linux only for per-directory watching)
// =============================================================================

/// Metrics collected during file watcher setup.
/// Only meaningful on Linux where we watch directories individually.
#[derive(Debug, Clone, Default)]
pub struct WatcherMetrics {
    /// Total number of directories found
    pub directory_count: usize,
    /// Number of directories that couldn't be watched (beyond limit)
    pub skipped_count: usize,
}

/// Check if watch metrics exceed thresholds and return a warning message.
/// On macOS/Windows with recursive watching, this is never called.
pub fn check_watch_warning(metrics: &WatcherMetrics, limit: Option<usize>) -> Option<String> {
    // If there's no limit (macOS/Windows/WSL), no warning needed
    let limit = limit?;

    if metrics.skipped_count > 0 || metrics.directory_count > limit {
        Some(format!(
            "Large repo: refreshing every {}s",
            FALLBACK_REFRESH_SECS
        ))
    } else {
        None
    }
}

// =============================================================================
// Diff thresholds (all platforms)
// =============================================================================

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

    // =========================================================================
    // inotify parsing tests (run on all platforms, test the parsing logic)
    // =========================================================================

    #[test]
    #[cfg(target_os = "linux")]
    fn test_parse_inotify_value_valid() {
        assert_eq!(parse_inotify_value("524288\n"), Some(524288));
        assert_eq!(parse_inotify_value("8192"), Some(8192));
        assert_eq!(parse_inotify_value("  1024  "), Some(1024));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_parse_inotify_value_invalid() {
        assert_eq!(parse_inotify_value("invalid"), None);
        assert_eq!(parse_inotify_value(""), None);
        assert_eq!(parse_inotify_value("12abc"), None);
    }

    // =========================================================================
    // Watch limit tests
    // =========================================================================

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn test_get_watch_limit_non_linux_returns_none() {
        // macOS and Windows use recursive watching, no limit needed
        assert!(get_watch_limit().is_none());
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn test_is_wsl_returns_false_on_non_linux() {
        assert!(!is_wsl());
    }

    // =========================================================================
    // Watch warning tests
    // =========================================================================

    #[test]
    fn test_check_watch_warning_no_limit_returns_none() {
        let metrics = WatcherMetrics {
            directory_count: 10000,
            skipped_count: 5000,
        };
        // When limit is None (macOS/Windows), no warning
        assert!(check_watch_warning(&metrics, None).is_none());
    }

    #[test]
    fn test_check_watch_warning_below_limit() {
        let metrics = WatcherMetrics {
            directory_count: 50,
            skipped_count: 0,
        };
        assert!(check_watch_warning(&metrics, Some(100)).is_none());
    }

    #[test]
    fn test_check_watch_warning_above_limit() {
        let metrics = WatcherMetrics {
            directory_count: 200,
            skipped_count: 0,
        };
        let warning = check_watch_warning(&metrics, Some(100));
        assert!(warning.is_some());
        assert!(warning.unwrap().contains("refreshing every"));
    }

    #[test]
    fn test_check_watch_warning_with_skipped() {
        let metrics = WatcherMetrics {
            directory_count: 50,
            skipped_count: 10,
        };
        let warning = check_watch_warning(&metrics, Some(100));
        assert!(warning.is_some());
    }

    // =========================================================================
    // Diff threshold tests
    // =========================================================================

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
        assert!(warning.unwrap().contains("lines"));
    }
}
