use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use vcs_runner::{run_jj, run_jj_cancellable};

use crate::diff::FileDiff;
use crate::highlight::{FileHighlight, HighlightSpan, compute_file_highlight_spans};

use super::super::jj_revset::jj_error_detail;
use super::fingerprint::op_head_key;
use super::parse::{
    ChangedFile, HighlightLogEntry, SpanTarget, coalesce_highlight_entries,
    parse_batched_file_show, parse_highlight_log,
};
use super::stack::joined;
use super::{JjVcs, no_snapshot};

/// Template used by `query_highlight_log` and the versioned fixture tests.
pub(super) const HIGHLIGHT_LOG_TEMPLATE: &str = r#"commit_id ++ "\t" ++ if(self.contained_in("@"), "@", ".") ++ "\t" ++ parents.map(|p| p.commit_id()).join(",") ++ "\n""#;

const FILE_SHOW_TEMPLATE: &str = r#""\x00\x00branchdiff-file:" ++ path.display() ++ "\x00""#;

/// Validate `--highlight <REVSET>` at `JjVcs` initialization time.
///
/// An empty revset result (0 commits, e.g. `feature@origin..feature` right after a push)
/// is valid and marks nothing; syntax errors or unknown symbols fail startup.
pub(super) fn validate_highlight_revset(repo_path: &Path, revset: &str) -> Result<String> {
    let trimmed = revset.trim();
    if trimmed.is_empty() {
        anyhow::bail!("--highlight revset cannot be empty");
    }
    let args = no_snapshot(&["log", "-r", trimmed, "--no-graph", "--limit", "1", "-T", ""]);
    if let Err(err) = run_jj(repo_path, &args) {
        let detail = jj_error_detail(&err);
        anyhow::bail!("--highlight {revset:?} is not a valid revset in this jj repo: {detail}");
    }
    Ok(revset.to_string())
}

#[derive(Debug, Clone)]
pub(super) struct JjHighlightSpan {
    pub from_revs: Vec<String>,
    pub to_target: SpanTarget,
    /// Map from `path` at `to_target` -> optional `old_path` at `from_revs`.
    pub changed_files: HashMap<String, Option<String>>,
    /// Map from `path` at shown head -> `old_path` at `to_target`.
    pub post_renames: HashMap<String, String>,
}

impl JjHighlightSpan {
    pub(super) fn paths_for_file<'a>(
        &'a self,
        file_path: &'a str,
        branch_old_path: Option<&'a str>,
    ) -> Option<(&'a str, &'a str)> {
        let after_candidate = self
            .post_renames
            .get(file_path)
            .map(String::as_str)
            .or_else(|| {
                branch_old_path.and_then(|old| self.post_renames.get(old).map(String::as_str))
            })
            .unwrap_or(file_path);
        if let Some(span_old) = self.changed_files.get(after_candidate) {
            return Some((span_old.as_deref().unwrap_or(after_candidate), after_candidate));
        }
        if let Some(span_old) = self.changed_files.get(file_path) {
            return Some((span_old.as_deref().unwrap_or(file_path), file_path));
        }
        if let Some(old) = branch_old_path
            && let Some(span_old) = self.changed_files.get(old)
        {
            return Some((span_old.as_deref().unwrap_or(old), old));
        }
        if self.to_target == SpanTarget::WorkingCopy {
            return Some((branch_old_path.unwrap_or(file_path), file_path));
        }
        None
    }
}

#[derive(Debug, Clone)]
pub(super) struct JjHighlightPlan {
    pub label: String,
    pub spans: Vec<JjHighlightSpan>,
    pub changed_paths: HashSet<String>,
    rev_contents: HashMap<String, HashMap<String, String>>,
    #[cfg(test)]
    pub spawn_count: usize,
}

fn is_root_commit_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b == b'0')
}

pub(super) fn query_highlight_log(
    repo_path: &Path,
    scoped_revset: &str,
    cancel: &Arc<AtomicBool>,
) -> Vec<HighlightLogEntry> {
    let args =
        no_snapshot(&["log", "-r", scoped_revset, "--no-graph", "-T", HIGHLIGHT_LOG_TEMPLATE]);
    run_jj_cancellable(repo_path, &args, Arc::clone(cancel))
        .ok()
        .map(|o| parse_highlight_log(&o.stdout_lossy()))
        .unwrap_or_default()
}

fn span_changed_files(
    vcs: &JjVcs,
    from_revs: &[String],
    to_target: &SpanTarget,
    branch_changed_files: &[ChangedFile],
    spawns: &std::sync::atomic::AtomicUsize,
    cancel: &Arc<AtomicBool>,
) -> HashMap<String, Option<String>> {
    let mut map = HashMap::new();
    let to_rev = match to_target {
        SpanTarget::Revision(r) => r.as_str(),
        SpanTarget::WorkingCopy => {
            if from_revs.len() == 1
                && vcs.is_colocated()
                && !is_root_commit_id(&from_revs[0])
                && let Ok(git_files) =
                    crate::vcs::git::get_all_changed_files(&vcs.repo_path, &from_revs[0])
            {
                return git_files.into_iter().map(|f| (f.path, f.old_path)).collect();
            }
            for f in branch_changed_files {
                map.insert(f.path.clone(), f.old_path.clone());
            }
            "@"
        }
    };
    spawns.fetch_add(1, Ordering::Relaxed);
    let from = from_revs.first().map_or("root()", String::as_str);
    if let Ok(files) = vcs.get_changed_files_no_snapshot(from, to_rev, cancel) {
        map.extend(files.into_iter().map(|f| (f.path, f.old_path)));
    }
    map
}

fn span_post_renames(
    vcs: &JjVcs,
    to_target: &SpanTarget,
    effective_to: &str,
    read_from_disk: bool,
    spawns: &std::sync::atomic::AtomicUsize,
    cancel: &Arc<AtomicBool>,
) -> HashMap<String, String> {
    let SpanTarget::Revision(to_rev) = to_target else {
        return HashMap::new();
    };
    if read_from_disk
        && vcs.is_colocated()
        && !is_root_commit_id(to_rev)
        && let Ok(git_files) = crate::vcs::git::get_all_changed_files(&vcs.repo_path, to_rev)
    {
        return git_files.into_iter().filter_map(|f| Some((f.path, f.old_path?))).collect();
    }
    spawns.fetch_add(1, Ordering::Relaxed);
    vcs.get_changed_files_no_snapshot(to_rev, effective_to, cancel)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|f| Some((f.path, f.old_path?)))
        .collect()
}

fn batch_jj_file_contents(
    vcs: &JjVcs,
    rev: &str,
    paths: &[String],
    spawns: &std::sync::atomic::AtomicUsize,
    cancel: &Arc<AtomicBool>,
) -> HashMap<String, String> {
    if paths.is_empty() || is_root_commit_id(rev) {
        return HashMap::new();
    }
    let filesets: Vec<String> = paths.iter().map(|p| format!("root-file:{p:?}")).collect();
    let mut raw_args: Vec<&str> = Vec::with_capacity(filesets.len() + 6);
    raw_args.extend_from_slice(&["file", "show", "-r", rev, "-T", FILE_SHOW_TEMPLATE]);
    raw_args.extend(filesets.iter().map(String::as_str));
    let args = no_snapshot(&raw_args);
    spawns.fetch_add(1, Ordering::Relaxed);
    run_jj_cancellable(&vcs.repo_path, &args, Arc::clone(cancel))
        .ok()
        .map(|o| parse_batched_file_show(&o.stdout_lossy()))
        .unwrap_or_default()
}

impl JjHighlightPlan {
    pub(super) fn resolve(
        vcs: &JjVcs,
        effective_from: &str,
        effective_to: &str,
        read_from_disk: bool,
        branch_changed_files: &[ChangedFile],
        cancel: &Arc<AtomicBool>,
    ) -> Option<Arc<Self>> {
        let revset = vcs.highlight_revset.as_deref()?;
        let key = op_head_key(
            &vcs.op_store_dir, &vcs.repo_path, effective_from, effective_to, read_from_disk,
            branch_changed_files,
        );
        if let Some(ref k) = key
            && let Some((cached_k, plan)) =
                &vcs.disk_manifest.lock().unwrap_or_else(|e| e.into_inner()).highlight_cache
            && cached_k == k
        {
            return Some(Arc::clone(plan));
        }
        let plan = Arc::new(Self::build(
            vcs, revset, effective_from, effective_to, read_from_disk, branch_changed_files, cancel,
        ));
        if let Some(k) = key
            && !cancel.load(Ordering::Relaxed)
        {
            vcs.disk_manifest.lock().unwrap_or_else(|e| e.into_inner()).highlight_cache =
                Some((k, Arc::clone(&plan)));
        }
        Some(plan)
    }

    fn build(
        vcs: &JjVcs,
        revset: &str,
        effective_from: &str,
        effective_to: &str,
        read_from_disk: bool,
        branch_changed_files: &[ChangedFile],
        cancel: &Arc<AtomicBool>,
    ) -> Self {
        let spawns = std::sync::atomic::AtomicUsize::new(1);
        let scoped = format!("({revset}) & ({effective_from})..({effective_to})");
        let entries = query_highlight_log(&vcs.repo_path, &scoped, cancel);
        let raw_spans = coalesce_highlight_entries(&entries, read_from_disk);

        let mut spans = Vec::with_capacity(raw_spans.len());
        let mut changed_paths = HashSet::new();
        let mut paths_by_rev: HashMap<String, Vec<String>> = HashMap::new();

        for (from_revs, to_target) in raw_spans {
            let changed_files = span_changed_files(
                vcs, &from_revs, &to_target, branch_changed_files, &spawns, cancel,
            );
            let post_renames =
                span_post_renames(vcs, &to_target, effective_to, read_from_disk, &spawns, cancel);

            for (path, old_path) in &changed_files {
                changed_paths.insert(path.clone());
                if let Some(old) = old_path {
                    changed_paths.insert(old.clone());
                }
                let before_path = old_path.as_deref().unwrap_or(path.as_str());
                for from in &from_revs {
                    if !is_root_commit_id(from) {
                        paths_by_rev.entry(from.clone()).or_default().push(before_path.to_string());
                    }
                }
                if let SpanTarget::Revision(to_rev) = &to_target
                    && !is_root_commit_id(to_rev)
                {
                    paths_by_rev.entry(to_rev.clone()).or_default().push(path.clone());
                }
            }
            for (head_path, span_to_path) in &post_renames {
                if changed_files.contains_key(span_to_path) {
                    changed_paths.insert(head_path.clone());
                }
            }
            spans.push(JjHighlightSpan { from_revs, to_target, changed_files, post_renames });
        }

        let rev_list: Vec<(String, Vec<String>)> = paths_by_rev
            .into_iter()
            .map(|(rev, mut ps)| {
                ps.sort_unstable();
                ps.dedup();
                (rev, ps)
            })
            .collect();
        let rev_contents: HashMap<String, HashMap<String, String>> = std::thread::scope(|scope| {
            let handles: Vec<_> = rev_list
                .iter()
                .map(|(rev, ps)| {
                    let spawns_ref = &spawns;
                    scope.spawn(move || {
                        (rev.clone(), batch_jj_file_contents(vcs, rev, ps, spawns_ref, cancel))
                    })
                })
                .collect();
            handles.into_iter().map(joined).collect()
        });

        Self {
            label: revset.to_string(),
            spans,
            changed_paths,
            rev_contents,
            #[cfg(test)]
            spawn_count: spawns.load(Ordering::Relaxed),
        }
    }

    pub(super) fn compute_for_file(
        &self,
        repo_path: &Path,
        file_diff: &FileDiff,
        file_path: &str,
        old_path: Option<&str>,
        base_content: Option<&str>,
        working_content: Option<&str>,
        _cancel: &Arc<AtomicBool>,
    ) -> FileHighlight {
        if !self.changed_paths.contains(file_path)
            && !old_path.is_some_and(|old| self.changed_paths.contains(old))
        {
            return FileHighlight { label: Some(self.label.clone()), ..FileHighlight::default() };
        }

        let mut owned_spans: Vec<(Vec<Option<String>>, Option<String>)> =
            Vec::with_capacity(self.spans.len());
        for span in &self.spans {
            let Some((before_path, after_path)) = span.paths_for_file(file_path, old_path) else {
                continue;
            };
            let after = match &span.to_target {
                SpanTarget::WorkingCopy => {
                    if after_path == file_path {
                        working_content.map(String::from)
                    } else {
                        crate::vcs::shared::read_working_file(repo_path, after_path).ok().flatten()
                    }
                }
                SpanTarget::Revision(to_rev) => {
                    self.rev_contents.get(to_rev).and_then(|m| m.get(after_path)).cloned()
                }
            };
            let befores = if span.from_revs.is_empty() {
                vec![None]
            } else {
                span.from_revs
                    .iter()
                    .map(|from| {
                        if is_root_commit_id(from) {
                            None
                        } else {
                            self.rev_contents.get(from).and_then(|m| m.get(before_path)).cloned()
                        }
                    })
                    .collect()
            };
            owned_spans.push((befores, after));
        }

        let borrowed_befores: Vec<Vec<Option<&str>>> =
            owned_spans.iter().map(|(bs, _)| bs.iter().map(Option::as_deref).collect()).collect();
        let highlight_spans: Vec<HighlightSpan<'_>> = owned_spans
            .iter()
            .zip(&borrowed_befores)
            .map(|((_, after), bs)| HighlightSpan {
                befores: bs.as_slice(),
                after: after.as_deref(),
            })
            .collect();

        compute_file_highlight_spans(
            file_diff, base_content, working_content, &self.label, &highlight_spans,
        )
    }

    pub(super) fn stamp_assembled_files(&self, files: &mut [FileDiff]) {
        for file in files {
            if file.highlight.label.is_none() {
                file.highlight.label = Some(self.label.clone());
            }
        }
    }
}
