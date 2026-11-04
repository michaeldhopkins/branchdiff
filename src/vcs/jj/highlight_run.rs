//! The jj calls behind a highlight plan. A failure here must reach the caller:
//! a plan built from a failed call is partial and must never be cached.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Result;
use vcs_runner::{is_transient_error, run_jj_with_retry_cancellable};

#[cfg(test)]
static INJECTED_FAILURES: std::sync::Mutex<Vec<(std::path::PathBuf, usize)>> =
    std::sync::Mutex::new(Vec::new());

/// Make the next `count` highlight jj calls in `repo_path` fail (tests only).
#[cfg(test)]
pub(in crate::vcs::jj) fn inject_failures(repo_path: &Path, count: usize) {
    let mut guard = INJECTED_FAILURES.lock().unwrap_or_else(|e| e.into_inner());
    guard.retain(|(p, _)| p != repo_path);
    guard.push((repo_path.to_path_buf(), count));
}

#[cfg(test)]
fn take_injected_failure(repo_path: &Path) -> bool {
    let mut guard = INJECTED_FAILURES.lock().unwrap_or_else(|e| e.into_inner());
    match guard.iter_mut().find(|(p, n)| p == repo_path && *n > 0) {
        Some((_, n)) => {
            *n -= 1;
            true
        }
        None => false,
    }
}

/// Run jj, retrying transient errors, and return stdout.
pub(super) fn run_highlight_jj(
    repo_path: &Path,
    args: &[&str],
    cancel: &Arc<AtomicBool>,
) -> Result<String> {
    #[cfg(test)]
    if take_injected_failure(repo_path) {
        anyhow::bail!("injected jj failure");
    }
    let output =
        run_jj_with_retry_cancellable(repo_path, args, is_transient_error, Arc::clone(cancel))?;
    Ok(output.stdout_lossy().into_owned())
}
