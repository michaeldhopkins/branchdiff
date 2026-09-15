//! Which kind of event a changed path is, in a jj repo.

use std::path::Path;

use super::VcsEventType;

/// `op_store_dir` is the resolved store, which a secondary workspace shares from outside its own
/// `.jj`. `colocated` is asked only for a path under `.git`.
pub(super) fn classify(path: &Path, repo_path: &Path, op_store_dir: &Path, colocated: impl FnOnce() -> bool) -> VcsEventType {
    let relative = path.strip_prefix(repo_path).unwrap_or(path);
    let first = relative.components().next().map(|c| c.as_os_str());
    let in_jj = first.is_some_and(|c| c == ".jj");
    let in_op_store = path.starts_with(op_store_dir);

    if (in_jj || in_op_store) && is_transient(path) {
        return VcsEventType::Lock;
    }
    if in_op_store {
        return VcsEventType::Internal;
    }
    if in_jj {
        return if relative.to_string_lossy().contains("working_copy/") {
            VcsEventType::RevisionChange
        } else {
            VcsEventType::Internal
        };
    }
    if first.is_some_and(|c| c == ".git") && colocated() {
        return VcsEventType::Internal;
    }
    VcsEventType::Source
}

/// A lock, or the temporary file of an atomic write, that jj creates and removes around every
/// command, including one that changes nothing: a plain `jj st` makes `working_copy.lock`,
/// `git_import_export.lock` and a `working_copy/.tmp*`, and nothing else. Read as a revision
/// change, that made every jj command anyone ran in the repo a full refresh. A command that does
/// change something also rewrites `working_copy/checkout` and adds an `op_heads` entry, and
/// those still refresh.
fn is_transient(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name == "lock" || name.ends_with(".lock") || name.starts_with(".tmp"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use proptest::prelude::*;

    use super::*;

    const REPO: &str = "/repo";
    const OP_STORE: &str = "/repo/.jj/repo/op_store";

    fn kind(path: &str) -> VcsEventType {
        classify(Path::new(path), Path::new(REPO), Path::new(OP_STORE), || true)
    }

    #[test]
    fn what_a_command_that_changes_nothing_leaves_behind_is_a_lock() {
        for path in [
            "/repo/.jj/working_copy/working_copy.lock",
            "/repo/.jj/working_copy/.tmpDQjqj9",
            "/repo/.jj/repo/git_import_export.lock",
            "/repo/.jj/repo/op_heads/heads/lock",
            "/repo/.jj/repo/store/extra/lock",
            "/repo/.jj/repo/op_store/operations/.tmp0ZmFDC",
        ] {
            assert_eq!(kind(path), VcsEventType::Lock, "{path}");
        }
    }

    #[test]
    fn what_a_real_change_writes_still_refreshes() {
        assert_eq!(kind("/repo/.jj/working_copy/checkout"), VcsEventType::RevisionChange);
        assert_eq!(kind("/repo/.jj/working_copy/tree_state"), VcsEventType::RevisionChange);
        assert_eq!(kind("/repo/.jj/repo/op_heads/heads/3e4a5bd52c00"), VcsEventType::Internal);
        assert_eq!(kind("/repo/.jj/repo/op_store/operations/db9b2b7411fe"), VcsEventType::Internal);
        assert_eq!(kind("/repo/.jj/repo/index/segments/8e66e76c5b50"), VcsEventType::Internal);
    }

    #[test]
    fn a_shared_op_store_outside_the_workspace_is_classified_the_same() {
        let shared = |path: &str| classify(Path::new(path), Path::new("/ws"), Path::new("/main/.jj/repo/op_store"), || false);
        assert_eq!(shared("/main/.jj/repo/op_store/operations/abc"), VcsEventType::Internal);
        assert_eq!(shared("/main/.jj/repo/op_store/views/.tmp5wPuyV"), VcsEventType::Lock);
    }

    #[test]
    fn a_source_file_named_like_a_lock_is_still_source() {
        assert_eq!(kind("/repo/Cargo.lock"), VcsEventType::Source);
        assert_eq!(kind("/repo/src/.tmpnotes"), VcsEventType::Source);
    }

    #[test]
    fn git_paths_are_internal_only_when_colocated() {
        assert_eq!(kind("/repo/.git/index"), VcsEventType::Internal);
        let alone = classify(Path::new("/repo/.git/index"), Path::new(REPO), Path::new(OP_STORE), || false);
        assert_eq!(alone, VcsEventType::Source);
    }

    fn arb_segment() -> impl Strategy<Value = String> {
        "[a-z0-9_]{1,8}(\\.[a-z]{1,4})?"
    }

    proptest! {
        /// A transient artifact anywhere under `.jj` never refreshes by itself.
        #[test]
        fn a_transient_name_under_jj_is_always_a_lock(
            dirs in prop::collection::vec(arb_segment(), 0..4),
            name in prop_oneof![Just("lock".to_string()), arb_segment().prop_map(|s| format!("{s}.lock")), arb_segment().prop_map(|s| format!(".tmp{s}"))],
        ) {
            let path: PathBuf = [REPO, ".jj"].iter().map(PathBuf::from).chain(dirs.iter().map(PathBuf::from)).chain([PathBuf::from(&name)]).collect();
            prop_assert_eq!(kind(path.to_str().unwrap()), VcsEventType::Lock);
        }

        /// Outside `.jj`, `.git` and the op store, every path is source, whatever its name.
        #[test]
        fn the_working_tree_is_always_source(dirs in prop::collection::vec(arb_segment(), 1..5)) {
            let path: PathBuf = std::iter::once(PathBuf::from(REPO)).chain(dirs.iter().map(PathBuf::from)).collect();
            prop_assert_eq!(kind(path.to_str().unwrap()), VcsEventType::Source);
        }
    }
}
