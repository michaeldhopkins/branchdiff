//! The background auto-fetch: pull from the remote, then say whether the base moved.
//!
//! The base is read on both sides of the fetch and compared with itself. Comparing it with what
//! the last refresh recorded instead is wrong for jj: a refresh records the fork point's change
//! id while `base_identifier` reads `trunk()`'s, so once trunk moved past the fork point every
//! fetch looked like a new base and forced a full refresh, every 30 seconds, forever.

use crate::message::FetchResult;
use crate::vcs::Vcs;

/// Fetch, and report the base as it stands afterwards only when the fetch moved it. `None`
/// when the fetch itself failed.
pub fn fetch_and_compare(vcs: &dyn Vcs) -> Option<FetchResult> {
    let before = vcs.base_identifier().ok();
    vcs.fetch().ok()?;
    let has_conflicts = vcs.has_conflicts().unwrap_or(false);
    let after = vcs.base_identifier().ok();
    Some(FetchResult { has_conflicts, new_merge_base: after.filter(|a| before.as_ref() != Some(a)) })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use anyhow::{Result, anyhow};

    use super::*;
    use crate::diff::FileDiff;
    use crate::vcs::{ComparisonContext, RefreshResult, VcsBackend, VcsEventType, VcsWatchPaths};

    /// A backend whose base reads come from a script, one per call, and whose fetch can fail.
    struct ScriptedBase {
        reads: Mutex<Vec<Option<&'static str>>>,
        fetch_works: bool,
    }

    impl ScriptedBase {
        fn new(reads: &[Option<&'static str>], fetch_works: bool) -> Self {
            Self { reads: Mutex::new(reads.iter().rev().copied().collect()), fetch_works }
        }
    }

    impl Vcs for ScriptedBase {
        fn repo_path(&self) -> &Path { Path::new("/repo") }
        fn comparison_context(&self) -> Result<ComparisonContext> { unimplemented!() }
        fn refresh(&self, _: &Arc<AtomicBool>) -> Result<RefreshResult> { unimplemented!() }
        fn single_file_diff(&self, _: &str) -> Option<FileDiff> { unimplemented!() }
        fn base_identifier(&self) -> Result<String> {
            let next = self.reads.lock().unwrap().pop().expect("more base reads than scripted");
            next.map(str::to_string).ok_or_else(|| anyhow!("unreadable"))
        }
        fn base_file_bytes(&self, _: &str) -> Result<Option<Vec<u8>>> { unimplemented!() }
        fn working_file_bytes(&self, _: &str) -> Result<Option<Vec<u8>>> { unimplemented!() }
        fn fetch(&self) -> Result<()> { if self.fetch_works { Ok(()) } else { Err(anyhow!("offline")) } }
        fn has_conflicts(&self) -> Result<bool> { Ok(false) }
        fn is_locked(&self) -> bool { false }
        fn watch_paths(&self) -> VcsWatchPaths { VcsWatchPaths { files: vec![], recursive_dirs: vec![PathBuf::new()] } }
        fn classify_event(&self, _: &Path) -> VcsEventType { VcsEventType::Source }
        fn backend(&self) -> VcsBackend { VcsBackend::Jj }
        fn current_revision_id(&self) -> Result<String> { unimplemented!() }
    }

    #[test]
    fn a_fetch_that_brings_nothing_reports_no_new_base() {
        let result = fetch_and_compare(&ScriptedBase::new(&[Some("trunk1"), Some("trunk1")], true)).unwrap();
        assert_eq!(result.new_merge_base, None, "an unmoved base must not force a refresh");
    }

    #[test]
    fn a_fetch_that_moves_the_base_reports_where_it_went() {
        let result = fetch_and_compare(&ScriptedBase::new(&[Some("trunk1"), Some("trunk2")], true)).unwrap();
        assert_eq!(result.new_merge_base.as_deref(), Some("trunk2"));
    }

    #[test]
    fn a_failed_fetch_reports_nothing() {
        assert!(fetch_and_compare(&ScriptedBase::new(&[Some("trunk1")], false)).is_none());
    }

    #[test]
    fn a_base_unreadable_before_the_fetch_counts_as_moved() {
        let result = fetch_and_compare(&ScriptedBase::new(&[None, Some("trunk1")], true)).unwrap();
        assert_eq!(result.new_merge_base.as_deref(), Some("trunk1"), "unknown before, so refresh once to be safe");
        let result = fetch_and_compare(&ScriptedBase::new(&[Some("trunk1"), None], true)).unwrap();
        assert_eq!(result.new_merge_base, None, "nothing to report when it cannot be read after");
    }
}
