//! A full refresh, and the one-shot load that reuses its labels.

use super::*;

/// A refresh with the two revisions it labelled.
pub(super) struct Refreshed {
    pub(super) result: RefreshResult,
    /// The revision the base label was read from.
    pub(super) effective_from: String,
    /// The revision the current label was read from (`@`, unless re-anchored).
    pub(super) to_rev: String,
}

impl JjVcs {
    pub(super) fn refresh_anchored(&self, cancel_flag: &Arc<AtomicBool>) -> Result<Refreshed> {
        // The fork point (for divergence and fork-point mode), the stack tip (is
        // @ mid-stack? if not, an orphaned empty @ left by an abandon?) and the
        // bookmark boundary (for BookmarkOnly view mode).
        let Anchors { fork_point, divergence, stack_tip, orphaned, bookmark_boundary } =
            resolve_anchors(&self.repo_path, &self.from_rev, cancel_flag, || {
                resolve_bookmark_boundary(&self.repo_path, &self.from_rev, cancel_flag)
            });
        let effective_from = match self.load_diff_base() {
            DiffBase::ForkPoint => fork_point.as_deref().unwrap_or(&self.from_rev),
            DiffBase::TrunkTip => &self.from_rev,
        };
        let orphaned_tip = orphaned.is_some();

        // The commit treated as the current commit (`@`). Normally literally `@`;
        // for an orphaned empty `@` it's the sibling tip, so that tip's changes
        // color as the current commit and show in the [commit] view.
        let at_rev = orphaned.as_ref().map(|t| t.change_id.as_str()).unwrap_or("@");
        // Visible tip to enumerate and diff against.
        let effective_to =
            stack_tip.as_ref().or(orphaned.as_ref()).map(|t| t.change_id.as_str()).unwrap_or("@");
        // The "above @" (Unstaged) layer only exists for a real stack tip above a
        // mid-stack @; an orphaned tip *is* the anchor, so nothing sits above it.
        let tip_rev = stack_tip.as_ref().map(|t| t.change_id.as_str());

        let bookmark_changed_files = bookmark_boundary.as_ref().map(|b| &b.changed_files);

        // Discover changed files. When the working copy is the diff target (the
        // common case), do it from disk/git without snapshotting. The stack /
        // orphaned-tip cases target a committed rev and still use the jj summary.
        let changed_files = if effective_to == "@" {
            self.discover_working_changes(effective_from, cancel_flag)?
        } else {
            self.get_changed_files_with_from(effective_from, effective_to, cancel_flag)?
        };

        if cancel_flag.load(Ordering::Relaxed) {
            anyhow::bail!("refresh cancelled");
        }

        // Binary detection: from disk for the working target (no snapshot), else
        // the committed-diff stat for the stack/orphaned cases.
        let binary_files = if effective_to == "@" {
            self.working_binary_files(&changed_files)
        } else {
            self.get_binary_files_set(effective_from, effective_to, cancel_flag)
        };

        if cancel_flag.load(Ordering::Relaxed) {
            anyhow::bail!("refresh cancelled");
        }

        // Normally the "to" label is @ (where the working copy sits). When we
        // re-anchored to an orphaned sibling tip, @ is an empty throwaway commit,
        // so label the tip we actually diffed against instead.
        let to_rev = if orphaned_tip { effective_to } else { "@" };
        // The labels and stack position need only the revisions, so their jj
        // calls run while the files are read.
        let (results, ((base_identifier, base_label_str), (_, current_branch_str), stack_position)) =
            std::thread::scope(|scope| {
                let base = scope.spawn(|| self.rev_metadata_no_snapshot(effective_from));
                let to = scope.spawn(|| self.rev_metadata_no_snapshot(to_rev));
                let position = scope.spawn(|| {
                    stack_tip.as_ref().and_then(|tip| {
                        let (current, total) = compute_stack_position(
                            &self.repo_path, &tip.change_id, &self.from_rev,
                        )?;
                        Some(StackPosition { current, total, head_count: tip.head_count })
                    })
                });
                let results = process_files_parallel(&changed_files, cancel_flag, |changed| {
                    process_jj_file(
                        &self.repo_path, effective_from, changed, &binary_files, tip_rev, at_rev,
                        bookmark_changed_files, cancel_flag,
                    )
                });
                (results, (joined(base), joined(to), joined(position)))
            });

        if cancel_flag.load(Ordering::Relaxed) {
            anyhow::bail!("refresh cancelled");
        }

        let assembled = assemble_results(results);
        let files = assembled.files;
        let all_lines = assembled.lines;

        let metrics = DiffMetrics { total_lines: all_lines.len(), file_count: files.len() };

        let file_paths: Vec<&str> = files
            .iter()
            .filter_map(|f| f.lines.first())
            .filter_map(|l| l.file_path.as_deref())
            .collect();
        let file_links = crate::file_links::compute_file_links(&file_paths);

        let labelled_from = effective_from.to_string();
        let result = RefreshResult {
            files,
            lines: all_lines,
            base_identifier,
            base_label: Some(base_label_str),
            current_branch: Some(current_branch_str),
            metrics,
            file_links,
            stack_position,
            bookmark_name: bookmark_boundary.map(|b| b.bookmark_name),
            revision_id: None,
            divergence,
            // Populated by the worker thread / initial-load path, not the backend.
        };
        Ok(Refreshed { result, effective_from: labelled_from, to_rev: to_rev.to_string() })
    }

    /// The display label of `rev`, or `rev` itself when jj cannot say.
    pub(super) fn label_of(&self, rev: &str) -> String {
        let template =
            r#"bookmarks ++ "\0" ++ change_id.short(12) ++ "\0" ++ change_id.shortest(4)"#;
        // No snapshot: the working side is read from disk (see `get_changed_files_with_from` for the exception).
        let args = no_snapshot(&["log", "-r", rev, "-T", template, "--no-graph", "--limit", "1"]);
        match run_jj_with_retry(&self.repo_path, &args, is_transient_error) {
            Ok(output) => parse_rev_metadata(&output.stdout_lossy()).1,
            Err(_) => rev.to_string(),
        }
    }

    pub(super) fn comparison_with(
        &self,
        from_label: String,
        to_label: String,
    ) -> ComparisonContext {
        // stack_position is computed by refresh() and applied via
        // apply_refresh_result — no need to resolve it here too.
        ComparisonContext {
            from_label,
            to_label,
            stack_position: None,
            vcs_backend: VcsBackend::Jj,
            bookmark_name: None,
            divergence: None,
        }
    }
}

/// The labels and the refresh a one-shot run needs.
///
/// A refresh already read the label of the revision it diffed from and of
/// the one it labelled as current, so those are reused when they are the
/// revisions the comparison names (the trunk base, and `@` unless
/// re-anchored). The base label is looked up while the refresh runs, for
/// when the refresh diffed from a fork point instead.
pub(super) fn comparison_and_refresh(
    vcs: &JjVcs,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<(ComparisonContext, RefreshResult)> {
    let (refreshed, from_label) = std::thread::scope(|scope| {
        let from = scope.spawn(|| vcs.label_of(&vcs.from_rev));
        (vcs.refresh_anchored(cancel_flag), joined(from))
    });
    let Refreshed { result, effective_from, to_rev } = refreshed?;
    let from_label = match &result.base_label {
        Some(label) if effective_from == vcs.from_rev => label.clone(),
        _ => from_label,
    };
    let to_label = match &result.current_branch {
        Some(label) if to_rev == "@" => label.clone(),
        _ => vcs.label_of("@"),
    };
    Ok((vcs.comparison_with(from_label, to_label), result))
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::super::tests::{
        jj_available, jj_out, setup_orphaned_after_abandon, setup_repo_with_remote,
    };
    use super::*;
    use crate::vcs::Vcs;

    fn jj(repo: &Path, args: &[&str]) {
        let output = Command::new("jj").args(args).current_dir(repo).output().unwrap();
        assert!(output.status.success(), "jj {args:?} failed");
    }

    /// The shared-label load must answer what the two separate calls answer.
    fn assert_same_as_separate_calls(repo: &Path) {
        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let expected = vcs.comparison_context().unwrap();
        let (comparison, refresh) = vcs.comparison_and_refresh(&cancel).unwrap();
        assert_eq!(comparison.from_label, expected.from_label);
        assert_eq!(comparison.to_label, expected.to_label);
        assert_eq!(refresh.base_identifier, vcs.refresh(&cancel).unwrap().base_identifier);
    }

    #[test]
    fn labels_match_when_the_stack_sits_on_trunk() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        std::fs::write(temp.path().join("s.txt"), "s\n").unwrap();
        jj(temp.path(), &["commit", "-m", "stack"]);
        assert_same_as_separate_calls(temp.path());
    }

    #[test]
    fn labels_match_when_trunk_moved_on_since_the_fork() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        std::fs::write(repo.join("s.txt"), "s\n").unwrap();
        jj(repo, &["commit", "-m", "stack"]);
        let stack = jj_out(repo, &["log", "-r", "@-", "--no-graph", "-T", "change_id.short(12)"]);
        jj(repo, &["new", "main", "-m", "upstream"]);
        std::fs::write(repo.join("up.txt"), "up\n").unwrap();
        jj(repo, &["bookmark", "set", "main", "-r", "@"]);
        jj(repo, &["git", "push", "--bookmark", "main"]);
        jj(repo, &["new", &stack]);
        assert_same_as_separate_calls(repo);
    }

    #[test]
    fn labels_match_when_an_orphaned_tip_stands_in_for_at() {
        if !jj_available() {
            return;
        }
        let (temp, _remote, _tip) = setup_orphaned_after_abandon();
        assert_same_as_separate_calls(temp.path());
    }
}
