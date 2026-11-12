//! A full refresh, and the one-shot load built from its labels.

use super::*;

/// The file-link map for the first line of each rendered file.
fn file_links_for(files: &[FileDiff]) -> HashMap<String, String> {
    let file_paths: Vec<&str> = files
        .iter()
        .filter_map(|f| f.lines.first())
        .filter_map(|l| l.file_path.as_deref())
        .collect();
    crate::file_links::compute_file_links(&file_paths)
}

impl JjVcs {
    pub(super) fn refresh_anchored(&self, cancel_flag: &Arc<AtomicBool>) -> Result<RefreshResult> {
        let target_rev = self.target_rev();
        let base_rev = self.base_rev();
        // The fork point (for divergence and fork-point mode), the stack tip (is
        // @ mid-stack? if not, an orphaned empty @ left by an abandon?) and the
        // bookmark boundary (for BookmarkOnly view mode).
        let Anchors { fork_point, divergence, stack_tip, orphaned, bookmark_boundary } =
            resolve_anchors(
                &self.repo_path,
                &self.from_rev,
                self.head_rev.as_deref(),
                cancel_flag,
                || {
                    resolve_bookmark_boundary_for(
                        &self.repo_path, &self.from_rev, target_rev, cancel_flag,
                    )
                },
            );
        let effective_from = match self.load_diff_base() {
            DiffBase::ForkPoint => fork_point.as_deref().unwrap_or(base_rev.as_ref()),
            DiffBase::TrunkTip => base_rev.as_ref(),
        };
        let orphaned_tip = orphaned.is_some();

        // The commit treated as the current commit (`@`). Normally literally `@`;
        // for an orphaned empty `@` it's the sibling tip, or `--head` when passed.
        let at_rev = self
            .head_rev
            .as_deref()
            .or_else(|| orphaned.as_ref().map(|t| t.change_id.as_str()))
            .unwrap_or("@");
        // Visible tip to enumerate and diff against.
        let effective_to = self
            .head_rev
            .as_deref()
            .or_else(|| stack_tip.as_ref().or(orphaned.as_ref()).map(|t| t.change_id.as_str()))
            .unwrap_or("@");
        // The "above @" (Unstaged) layer only exists for a real stack tip above a
        // mid-stack @; an orphaned tip *is* the anchor, so nothing sits above it.
        let tip_rev = stack_tip.as_ref().map(|t| t.change_id.as_str());
        let read_from_disk = self.head_rev.is_none() && effective_to == "@";

        let bookmark_changed_files = bookmark_boundary.as_ref().map(|b| &b.changed_files);

        let (changed_files, binary_files) =
            self.discover_changes(effective_from, effective_to, read_from_disk, cancel_flag)?;

        let highlight_plan = JjHighlightPlan::resolve(
            self, effective_from, effective_to, read_from_disk, &changed_files, cancel_flag,
        );

        // Normally the "to" label is @ (where the working copy sits). When we
        // re-anchored to an orphaned sibling tip, @ is an empty throwaway commit,
        // so label the tip we actually diffed against instead.
        let to_rev = if let Some(head) = &self.head_rev {
            head.as_str()
        } else if orphaned_tip {
            effective_to
        } else {
            "@"
        };
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
                        &self.repo_path,
                        effective_from,
                        changed,
                        &binary_files,
                        tip_rev,
                        at_rev,
                        read_from_disk,
                        bookmark_changed_files,
                        highlight_plan.as_deref(),
                        cancel_flag,
                    )
                });
                (results, (joined(base), joined(to), joined(position)))
            });

        if cancel_flag.load(Ordering::Relaxed) {
            anyhow::bail!("refresh cancelled");
        }

        let assembled = assemble_results(results);
        let mut files = assembled.files;
        if let Some(plan) = &highlight_plan {
            plan.stamp_assembled_files(&mut files);
        }
        let all_lines = assembled.lines;

        let metrics = DiffMetrics { total_lines: all_lines.len(), file_count: files.len() };

        let file_links = file_links_for(&files);

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
        Ok(result)
    }

    /// Changed files and the binary subset. When the working copy is the diff
    /// target (the common case), this reads disk/git without snapshotting.
    /// Explicit `--head` uses `--ignore-working-copy`; stack/orphaned-tip uses
    /// the jj summary and the committed-diff stat.
    fn discover_changes(
        &self,
        effective_from: &str,
        effective_to: &str,
        read_from_disk: bool,
        cancel_flag: &Arc<AtomicBool>,
    ) -> Result<(Vec<ChangedFile>, HashSet<String>)> {
        let changed_files = if read_from_disk {
            self.discover_working_changes(effective_from, cancel_flag)?
        } else if self.head_rev.is_some() {
            self.get_changed_files_no_snapshot(effective_from, effective_to, cancel_flag)?
        } else {
            self.get_changed_files_with_from(effective_from, effective_to, cancel_flag)?
        };

        if cancel_flag.load(Ordering::Relaxed) {
            anyhow::bail!("refresh cancelled");
        }

        let binary_files = if read_from_disk {
            self.working_binary_files(&changed_files)
        } else {
            self.get_binary_files_set(effective_from, effective_to, cancel_flag)
        };

        if cancel_flag.load(Ordering::Relaxed) {
            anyhow::bail!("refresh cancelled");
        }
        Ok((changed_files, binary_files))
    }

    pub(super) fn single_file_diff_anchored(&self, file_path: &str) -> Option<FileDiff> {
        let cancel = Arc::new(AtomicBool::new(false));
        let base_rev = self.base_rev();
        let (fork_point, stack_tip, orphaned) = resolve_fork_and_tips(
            &self.repo_path,
            &self.from_rev,
            self.head_rev.as_deref(),
            &cancel,
        );
        let effective_from = match self.load_diff_base() {
            DiffBase::ForkPoint => fork_point.as_deref().unwrap_or(base_rev.as_ref()),
            DiffBase::TrunkTip => base_rev.as_ref(),
        };
        let at_rev = self
            .head_rev
            .as_deref()
            .or_else(|| orphaned.as_ref().map(|t| t.change_id.as_str()))
            .unwrap_or("@");
        let effective_to = self
            .head_rev
            .as_deref()
            .or_else(|| stack_tip.as_ref().or(orphaned.as_ref()).map(|t| t.change_id.as_str()))
            .unwrap_or("@");
        let read_from_disk = self.head_rev.is_none() && effective_to == "@";

        let changed_files = if read_from_disk {
            self.discover_working_changes(effective_from, &cancel).ok()?
        } else if self.head_rev.is_some() {
            self.get_changed_files_no_snapshot(effective_from, effective_to, &cancel).ok()?
        } else {
            self.get_changed_files_with_from(effective_from, effective_to, &cancel).ok()?
        };
        let changed = changed_files.iter().find(|f| f.path == file_path)?;
        let old_path = changed.old_path.as_deref();

        let base_path = old_path.unwrap_or(file_path);
        let base = file_content_no_snapshot(&self.repo_path, base_path, effective_from, &cancel);

        let at_parent = format!("({at_rev})-");
        let parent = file_content_no_snapshot(&self.repo_path, file_path, &at_parent, &cancel)
            .or_else(|| {
                old_path.and_then(|old| {
                    file_content_no_snapshot(&self.repo_path, old, &at_parent, &cancel)
                })
            });

        let index = if read_from_disk {
            crate::vcs::shared::read_working_file(&self.repo_path, file_path).ok().flatten()
        } else {
            file_content_no_snapshot(&self.repo_path, file_path, at_rev, &cancel)
        };
        let tip_content = stack_tip.as_ref().and_then(|t| {
            file_content_no_snapshot(&self.repo_path, file_path, &t.change_id, &cancel)
        });
        let has_tip = stack_tip.is_some();

        if base.is_none() && index.is_none() && tip_content.is_none() {
            return None;
        }

        let is_binary = if read_from_disk {
            crate::vcs::shared::working_file_is_binary(&self.repo_path, file_path)
        } else {
            self.get_binary_files_set(effective_from, effective_to, &cancel).contains(file_path)
        };
        if is_binary {
            return None;
        }

        let working = if has_tip { tip_content.as_deref() } else { index.as_deref() };

        let mut file_diff = compute_four_way_diff(DiffInput {
            path: file_path,
            base: base.as_deref(),
            head: parent.as_deref(),
            index: index.as_deref(),
            working,
            old_path,
        });

        if let Some(boundary) = resolve_bookmark_boundary_for(
            &self.repo_path,
            &self.from_rev,
            self.target_rev(),
            &cancel,
        ) {
            let in_bookmark = boundary.changed_files.contains(file_path);
            mark_bookmark_provenance(&mut file_diff, in_bookmark);
        }

        if let Some(plan) = JjHighlightPlan::resolve(
            self, effective_from, effective_to, read_from_disk, &changed_files, &cancel,
        ) {
            file_diff.highlight = plan.compute_for_file(
                &self.repo_path,
                &file_diff,
                file_path,
                old_path,
                base.as_deref(),
                working,
                &cancel,
            );
        }

        Some(file_diff)
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

/// Wide terminal width for `jj diff --stat` so long paths are never truncated
/// by an inherited `COLUMNS` environment variable or a narrow terminal.
const STAT_COLUMNS: &str = "10000";

pub(super) fn run_jj_stat(
    repo_path: &Path,
    args: &[&str],
    cancel: Arc<AtomicBool>,
) -> std::result::Result<vcs_runner::RunOutput, vcs_runner::RunError> {
    vcs_runner::Cmd::new("jj")
        .in_dir(repo_path)
        .env("COLUMNS", STAT_COLUMNS)
        .args(args)
        .retry(vcs_runner::RetryPolicy::default().when(is_transient_error))
        .cancel(cancel)
        .run()
}

/// The labels and the refresh a one-shot run needs.
///
/// A refresh labels the revision it diffed from and the one it treated as
/// current (the orphaned tip, when re-anchored). Those are the labels the
/// app shows once the refresh is applied, so the comparison is built from
/// them and no further jj process runs. A label the refresh could not read
/// falls back to a lookup.
pub(super) fn comparison_and_refresh(
    vcs: &JjVcs,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<(ComparisonContext, RefreshResult)> {
    let result = vcs.refresh_anchored(cancel_flag)?;
    let from_label = match &result.base_label {
        Some(label) => label.clone(),
        None => vcs.label_of(&vcs.base_rev()),
    };
    let to_label = match &result.current_branch {
        Some(label) => label.clone(),
        None => vcs.label_of(vcs.target_rev()),
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

    /// The comparison a one-shot run starts with is what the app shows after
    /// applying the refresh: the labels of the revisions the refresh diffed.
    fn comparison_labels(repo: &Path) -> (ComparisonContext, RefreshResult) {
        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let (comparison, refresh) = vcs.comparison_and_refresh(&cancel).unwrap();
        assert_eq!(Some(&comparison.from_label), refresh.base_label.as_ref());
        assert_eq!(Some(&comparison.to_label), refresh.current_branch.as_ref());
        (comparison, refresh)
    }

    #[test]
    fn labels_are_the_refreshs_when_the_stack_sits_on_trunk() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        std::fs::write(temp.path().join("s.txt"), "s\n").unwrap();
        jj(temp.path(), &["commit", "-m", "stack"]);
        let vcs = JjVcs::new(temp.path().to_path_buf()).unwrap();
        let (comparison, _) = comparison_labels(temp.path());
        assert_eq!(comparison.from_label, vcs.label_of(&vcs.from_rev));
        assert_eq!(comparison.to_label, vcs.label_of("@"));
    }

    #[test]
    fn labels_are_the_refreshs_when_trunk_moved_on_since_the_fork() {
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
        let (comparison, _) = comparison_labels(repo);
        assert_eq!(comparison.to_label, JjVcs::new(repo.to_path_buf()).unwrap().label_of("@"));
    }

    #[test]
    fn to_label_names_the_orphaned_tip_not_the_empty_working_copy() {
        if !jj_available() {
            return;
        }
        let (temp, _remote, tip) = setup_orphaned_after_abandon();
        let vcs = JjVcs::new(temp.path().to_path_buf()).unwrap();
        let (comparison, _) = comparison_labels(temp.path());
        assert_eq!(comparison.to_label, vcs.label_of(&tip));
        assert_ne!(comparison.to_label, vcs.label_of("@"));
    }

    #[test]
    fn binary_file_with_long_path_is_detected_under_columns_50_and_unset() {
        if !jj_available() {
            return;
        }
        if std::env::var_os("BRANCHDIFF_STAT_COLUMNS_PROBE").is_none() {
            let exe = std::env::current_exe().unwrap();
            let name = "vcs::jj::refresh::tests::binary_file_with_long_path_is_detected_under_columns_50_and_unset";
            for mode in ["50", "unset"] {
                let mut cmd = Command::new(&exe);
                cmd.args(["--exact", name]).env("BRANCHDIFF_STAT_COLUMNS_PROBE", mode);
                if mode == "unset" {
                    cmd.env_remove("COLUMNS");
                } else {
                    cmd.env("COLUMNS", mode);
                }
                let out = cmd.output().unwrap();
                let stdout = String::from_utf8_lossy(&out.stdout);
                assert!(
                    out.status.success() && stdout.contains("1 passed"),
                    "probe ({mode}) failed:\nstdout: {stdout}\nstderr: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        let long_rel = "deeply/nested/directory/structure/with/a/very/long/subpath/that/exceeds/fifty/and/one/hundred/twenty/columns/easily/image_asset.bin";
        assert!(long_rel.len() > 120);
        let full_path = repo.join(long_rel);
        std::fs::create_dir_all(full_path.parent().unwrap()).unwrap();
        std::fs::write(&full_path, b"\x00\x01\x02\xff\x00binary-payload").unwrap();
        jj(repo, &["commit", "-m", "add binary"]);
        std::fs::write(repo.join("tip.txt"), "tip\n").unwrap();
        jj(repo, &["commit", "-m", "stack tip"]);
        // Move `@` to the mid-stack commit so `effective_to` is the committed stack tip
        // (`!= "@"`), exercising `get_binary_files_set` (`jj diff --stat`).
        jj(repo, &["edit", "@--"]);

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let binaries = vcs.get_binary_files_set(&vcs.from_rev, "@+", &cancel);
        assert!(
            binaries.contains(long_rel),
            "expected full binary path `{long_rel}` in {binaries:?}"
        );
        let refresh = vcs.refresh(&cancel).unwrap();
        let bin_diff = refresh
            .files
            .iter()
            .find(|d| {
                d.lines.first().and_then(|l| l.file_path.as_deref()).is_some_and(|p| p == long_rel)
            })
            .expect("full long binary path must be present in refresh files");
        assert!(
            bin_diff.lines.iter().any(|l| l.content.contains("[binary file]")),
            "long-path binary file must be marked [binary file], got: {:?}",
            bin_diff.lines
        );
    }

    #[test]
    fn linked_go_handler_and_test_are_paired() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        std::fs::write(repo.join("handler.go"), "package p\n").unwrap();
        std::fs::write(repo.join("handler_test.go"), "package p\n").unwrap();
        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let refresh = vcs.refresh(&cancel).unwrap();
        assert_eq!(
            refresh.file_links.get("handler.go").map(String::as_str),
            Some("handler_test.go")
        );
        assert_eq!(
            refresh.file_links.get("handler_test.go").map(String::as_str),
            Some("handler.go")
        );
        assert!(!refresh.file_links.contains_key("xyzzy"));
        assert!(!refresh.file_links.contains_key(""));
    }

    #[test]
    fn head_at_sign_ignores_unsnapshotted_edits() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        std::fs::write(repo.join("file.txt"), "snapshotted\n").unwrap();
        jj(repo, &["describe", "-m", "snap"]);
        // After this write, no jj command: it would snapshot the dirty line into `@`.
        std::fs::write(repo.join("file.txt"), "snapshotted\ndirty-line\n").unwrap();
        let vcs = JjVcs::with_options(
            repo.to_path_buf(),
            crate::vcs::VcsOptions { base: None, head: Some("@"), highlight: None },
        )
        .unwrap();
        let single = vcs.single_file_diff("file.txt").expect("file.txt");
        assert!(
            single.lines.iter().any(|l| l.content == "snapshotted"),
            "{:?}",
            single.lines.iter().map(|l| l.content.as_str()).collect::<Vec<_>>()
        );
        assert!(
            !single.lines.iter().any(|l| l.content.contains("dirty")),
            "unsnapshotted bytes must stay out of --head @: {:?}",
            single.lines.iter().map(|l| l.content.as_str()).collect::<Vec<_>>()
        );
    }
}
