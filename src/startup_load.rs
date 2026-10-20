//! Loading a one-shot run's data.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Result;

use crate::vcs::{ComparisonContext, RefreshResult, Vcs};

/// The labels and the full refresh a one-shot run needs.
///
/// They do not depend on each other, and each is mostly waiting on `git` or
/// `jj`, so they run side by side. A failed comparison is reported before a
/// failed refresh, as when they ran one after the other.
pub fn load_comparison_and_refresh(
    vcs: &dyn Vcs,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<(ComparisonContext, RefreshResult)> {
    let (comparison, refresh) = crate::vcs::vcs_thread_pool()
        .install(|| rayon::join(|| vcs.comparison_context(), || vcs.refresh(cancel_flag)));
    Ok((comparison?, refresh?))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::vcs::{VcsBackend, VcsEventType, VcsWatchPaths};

    /// Answers `comparison_context` and `refresh` from scripted outcomes.
    struct Scripted {
        path: PathBuf,
        comparison_fails: bool,
        refresh_fails: bool,
    }

    impl Vcs for Scripted {
        fn repo_path(&self) -> &Path {
            &self.path
        }
        fn comparison_context(&self) -> Result<ComparisonContext> {
            if self.comparison_fails {
                anyhow::bail!("comparison failed");
            }
            Ok(ComparisonContext {
                from_label: "main".to_string(),
                to_label: "feature".to_string(),
                stack_position: None,
                vcs_backend: VcsBackend::Git,
                bookmark_name: None,
                divergence: None,
            })
        }
        fn refresh(&self, _: &Arc<AtomicBool>) -> Result<RefreshResult> {
            if self.refresh_fails {
                anyhow::bail!("refresh failed");
            }
            let mut result = RefreshResult::empty();
            result.base_identifier = "abc123".to_string();
            Ok(result)
        }
        fn single_file_diff(&self, _: &str) -> Option<crate::diff::FileDiff> {
            None
        }
        fn base_identifier(&self) -> Result<String> {
            anyhow::bail!("unused")
        }
        fn base_file_bytes(&self, _: &str) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn working_file_bytes(&self, _: &str) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn fetch(&self) -> Result<()> {
            Ok(())
        }
        fn has_conflicts(&self) -> Result<bool> {
            Ok(false)
        }
        fn is_locked(&self) -> bool {
            false
        }
        fn watch_paths(&self) -> VcsWatchPaths {
            VcsWatchPaths { files: vec![], recursive_dirs: vec![] }
        }
        fn classify_event(&self, _: &Path) -> VcsEventType {
            VcsEventType::Source
        }
        fn backend(&self) -> VcsBackend {
            VcsBackend::Git
        }
        fn current_revision_id(&self) -> Result<String> {
            Ok("x".to_string())
        }
    }

    fn load(comparison_fails: bool, refresh_fails: bool) -> Result<(ComparisonContext, RefreshResult)> {
        let vcs = Scripted { path: PathBuf::from("/repo"), comparison_fails, refresh_fails };
        load_comparison_and_refresh(&vcs, &Arc::new(AtomicBool::new(false)))
    }

    #[test]
    fn returns_both_halves() {
        let (comparison, refresh) = load(false, false).unwrap();
        assert_eq!(comparison.from_label, "main");
        assert_eq!(refresh.base_identifier, "abc123");
    }

    #[test]
    fn a_failed_refresh_fails_the_load() {
        assert_eq!(load(false, true).unwrap_err().to_string(), "refresh failed");
    }

    #[test]
    fn a_failed_comparison_is_reported_before_a_failed_refresh() {
        assert_eq!(load(true, true).unwrap_err().to_string(), "comparison failed");
    }
}
