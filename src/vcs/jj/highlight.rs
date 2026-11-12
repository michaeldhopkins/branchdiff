use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use vcs_runner::run_jj;

use crate::diff::FileDiff;
use crate::highlight::{FileHighlight, HighlightSpan, compute_file_highlight_spans};

use super::super::jj_revset::jj_error_detail;
use super::fingerprint::op_head_key;
#[path = "highlight_run.rs"]
pub(super) mod highlight_run;
use super::parse::{
    ChangedFile, HighlightLogEntry, SpanTarget, coalesce_highlight_entries,
    parse_batched_file_show, parse_highlight_log,
};
use super::stack::joined;
use super::{JjVcs, no_snapshot};
use highlight_run::run_highlight_jj;

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
) -> Result<Vec<HighlightLogEntry>> {
    let args =
        no_snapshot(&["log", "-r", scoped_revset, "--no-graph", "-T", HIGHLIGHT_LOG_TEMPLATE]);
    Ok(parse_highlight_log(&run_highlight_jj(repo_path, &args, cancel)?))
}

fn span_changed_files(
    vcs: &JjVcs,
    from_revs: &[String],
    to_target: &SpanTarget,
    branch_changed_files: &[ChangedFile],
    spawns: &std::sync::atomic::AtomicUsize,
    cancel: &Arc<AtomicBool>,
) -> Result<HashMap<String, Option<String>>> {
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
                return Ok(git_files.into_iter().map(|f| (f.path, f.old_path)).collect());
            }
            for f in branch_changed_files {
                map.insert(f.path.clone(), f.old_path.clone());
            }
            "@"
        }
    };
    spawns.fetch_add(1, Ordering::Relaxed);
    let from = from_revs.first().map_or("root()", String::as_str);
    let files = vcs.get_changed_files_no_snapshot(from, to_rev, cancel)?;
    map.extend(files.into_iter().map(|f| (f.path, f.old_path)));
    Ok(map)
}

fn span_post_renames(
    vcs: &JjVcs,
    to_target: &SpanTarget,
    effective_to: &str,
    read_from_disk: bool,
    spawns: &std::sync::atomic::AtomicUsize,
    cancel: &Arc<AtomicBool>,
) -> Result<HashMap<String, String>> {
    let SpanTarget::Revision(to_rev) = to_target else {
        return Ok(HashMap::new());
    };
    if read_from_disk
        && vcs.is_colocated()
        && !is_root_commit_id(to_rev)
        && let Ok(git_files) = crate::vcs::git::get_all_changed_files(&vcs.repo_path, to_rev)
    {
        return Ok(git_files.into_iter().filter_map(|f| Some((f.path, f.old_path?))).collect());
    }
    spawns.fetch_add(1, Ordering::Relaxed);
    Ok(vcs
        .get_changed_files_no_snapshot(to_rev, effective_to, cancel)?
        .into_iter()
        .filter_map(|f| Some((f.path, f.old_path?)))
        .collect())
}

fn batch_jj_file_contents(
    vcs: &JjVcs,
    rev: &str,
    paths: &[String],
    spawns: &std::sync::atomic::AtomicUsize,
    cancel: &Arc<AtomicBool>,
) -> Result<HashMap<String, String>> {
    if paths.is_empty() || is_root_commit_id(rev) {
        return Ok(HashMap::new());
    }
    // `file show` fails on a path the revision lacks (an added or deleted
    // file), which is expected here; list first so only real failures error.
    let filesets: Vec<String> = paths.iter().map(|p| format!("root-file:{p:?}")).collect();
    let mut list_args: Vec<&str> = Vec::with_capacity(filesets.len() + 6);
    list_args.extend_from_slice(&["file", "list", "-r", rev, "-T", r#"path.display() ++ "\n""#]);
    list_args.extend(filesets.iter().map(String::as_str));
    spawns.fetch_add(1, Ordering::Relaxed);
    let listing = run_highlight_jj(&vcs.repo_path, &no_snapshot(&list_args), cancel)?;
    let present: HashSet<&str> = listing.lines().collect();
    let existing: Vec<String> = paths
        .iter()
        .filter(|p| present.contains(p.as_str()))
        .map(|p| format!("root-file:{p:?}"))
        .collect();
    if existing.is_empty() {
        return Ok(HashMap::new());
    }
    let mut raw_args: Vec<&str> = Vec::with_capacity(existing.len() + 6);
    raw_args.extend_from_slice(&["file", "show", "-r", rev, "-T", FILE_SHOW_TEMPLATE]);
    raw_args.extend(existing.iter().map(String::as_str));
    let args = no_snapshot(&raw_args);
    spawns.fetch_add(1, Ordering::Relaxed);
    Ok(parse_batched_file_show(&run_highlight_jj(&vcs.repo_path, &args, cancel)?))
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
        let built = match Self::build(
            vcs, revset, effective_from, effective_to, read_from_disk, branch_changed_files, cancel,
        ) {
            Ok(b) => b,
            Err(_) => {
                // A plan built from a failed jj call is partial: show no marks for
                // this refresh, but do not cache it, so the next refresh tries again.
                return Some(Arc::new(Self::without_marks(revset)));
            }
        };
        let plan = Arc::new(built);
        if let Some(k) = key
            && !cancel.load(Ordering::Relaxed)
        {
            vcs.disk_manifest.lock().unwrap_or_else(|e| e.into_inner()).highlight_cache =
                Some((k, Arc::clone(&plan)));
        }
        Some(plan)
    }

    fn without_marks(revset: &str) -> Self {
        Self {
            label: revset.to_string(),
            spans: Vec::new(),
            changed_paths: HashSet::new(),
            rev_contents: HashMap::new(),
            #[cfg(test)]
            spawn_count: 0,
        }
    }

    fn build(
        vcs: &JjVcs,
        revset: &str,
        effective_from: &str,
        effective_to: &str,
        read_from_disk: bool,
        branch_changed_files: &[ChangedFile],
        cancel: &Arc<AtomicBool>,
    ) -> Result<Self> {
        let spawns = std::sync::atomic::AtomicUsize::new(1);
        let scoped = format!("({revset}) & ({effective_from})..({effective_to})");
        let entries = query_highlight_log(&vcs.repo_path, &scoped, cancel)?;
        let raw_spans = coalesce_highlight_entries(&entries, read_from_disk);

        let mut spans = Vec::with_capacity(raw_spans.len());
        let mut changed_paths = HashSet::new();
        let mut paths_by_rev: HashMap<String, Vec<String>> = HashMap::new();

        for (from_revs, to_target) in raw_spans {
            let changed_files = span_changed_files(
                vcs, &from_revs, &to_target, branch_changed_files, &spawns, cancel,
            )?;
            let post_renames =
                span_post_renames(vcs, &to_target, effective_to, read_from_disk, &spawns, cancel)?;

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
        let rev_results: Vec<Result<(String, HashMap<String, String>)>> =
            std::thread::scope(|scope| {
                let handles: Vec<_> = rev_list
                    .iter()
                    .map(|(rev, ps)| {
                        let spawns_ref = &spawns;
                        scope.spawn(move || {
                            batch_jj_file_contents(vcs, rev, ps, spawns_ref, cancel)
                                .map(|contents| (rev.clone(), contents))
                        })
                    })
                    .collect();
                handles.into_iter().map(joined).collect()
            });
        let rev_contents = rev_results.into_iter().collect::<Result<HashMap<_, _>>>()?;

        Ok(Self {
            label: revset.to_string(),
            spans,
            changed_paths,
            rev_contents,
            #[cfg(test)]
            spawn_count: spawns.load(Ordering::Relaxed),
        })
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::super::JjVcs;
    use super::super::parse::ChangedFile;
    use super::super::tests::{jj_available, setup_repo_with_remote};
    use super::*;
    use crate::diff::{DiffLine, FileDiff, LineSource};
    use crate::vcs::Vcs;

    fn jj(repo: &Path, args: &[&str]) {
        let output = Command::new("jj").args(args).current_dir(repo).output().unwrap();
        assert!(
            output.status.success(),
            "jj {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn jj_text(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("jj").args(args).current_dir(repo).output().unwrap();
        assert!(
            output.status.success(),
            "jj {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn addition(content: &str, line_number: usize) -> DiffLine {
        DiffLine::new(LineSource::Staged, content.to_string(), '+', Some(line_number))
    }

    fn empty_plan(label: &str, spans: Vec<JjHighlightSpan>, changed: &[&str]) -> JjHighlightPlan {
        JjHighlightPlan {
            label: label.to_string(),
            spans,
            changed_paths: changed.iter().map(|p| (*p).to_string()).collect(),
            rev_contents: HashMap::new(),
            spawn_count: 0,
        }
    }

    #[test]
    fn working_copy_fallback_is_only_for_the_working_copy() {
        let working = JjHighlightSpan {
            from_revs: Vec::new(),
            to_target: SpanTarget::WorkingCopy,
            changed_files: HashMap::new(),
            post_renames: HashMap::new(),
        };
        assert_eq!(
            working.paths_for_file("new.txt", Some("old.txt")),
            Some(("old.txt", "new.txt"))
        );
        let revision =
            JjHighlightSpan { to_target: SpanTarget::Revision("abc".to_string()), ..working };
        assert_eq!(revision.paths_for_file("new.txt", Some("old.txt")), None);
    }

    #[test]
    fn root_commit_id_is_forty_zeros_and_empty_is_not() {
        assert!(!is_root_commit_id(""), "an empty id is not the root commit");
        let root = "0".repeat(40);
        assert!(is_root_commit_id(&root), "jj's root commit id is forty zeros, got {root}");
        assert!(!is_root_commit_id("0001"));
    }

    #[test]
    fn renamed_file_is_highlighted_when_only_its_old_path_is_in_the_set() {
        let mut rev_contents = HashMap::new();
        rev_contents.insert(
            "REV".to_string(),
            HashMap::from([("old.txt".to_string(), "added-line\n".to_string())]),
        );
        let mut plan = empty_plan(
            "hl",
            vec![JjHighlightSpan {
                from_revs: Vec::new(),
                to_target: SpanTarget::Revision("REV".to_string()),
                changed_files: HashMap::from([("old.txt".to_string(), None)]),
                post_renames: HashMap::new(),
            }],
            &["old.txt"],
        );
        plan.rev_contents = rev_contents;
        let file = FileDiff::new(vec![addition("added-line", 1)]);
        let cancel = Arc::new(AtomicBool::new(false));
        let hl = plan.compute_for_file(
            Path::new("."),
            &file,
            "new.txt",
            Some("old.txt"),
            None,
            Some("added-line\n"),
            &cancel,
        );
        assert_eq!(hl.line_count(), 1, "the added line of a renamed file must be marked");
        assert!(hl.touched);
    }

    #[test]
    fn file_outside_the_highlight_set_keeps_the_label() {
        let plan = empty_plan("feature@origin..feature", Vec::new(), &[]);
        let file = FileDiff::new(Vec::new());
        let cancel = Arc::new(AtomicBool::new(false));
        let hl = plan.compute_for_file(
            Path::new("."),
            &file,
            "untouched.txt",
            None,
            None,
            None,
            &cancel,
        );
        assert_eq!(hl.label.as_deref(), Some("feature@origin..feature"));
        assert_eq!(hl.line_count(), 0);
        assert!(!hl.touched);
    }

    #[test]
    fn working_copy_rename_reads_the_old_path_from_disk() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("old.txt"), "keep\n").unwrap();
        let plan = empty_plan(
            "hl",
            vec![JjHighlightSpan {
                from_revs: Vec::new(),
                to_target: SpanTarget::WorkingCopy,
                changed_files: HashMap::from([("old.txt".to_string(), None)]),
                post_renames: HashMap::from([("new.txt".to_string(), "old.txt".to_string())]),
            }],
            &["new.txt"],
        );
        let file = FileDiff::new(vec![addition("keep", 1), addition("extra", 2)]);
        let cancel = Arc::new(AtomicBool::new(false));
        let hl = plan.compute_for_file(
            dir.path(),
            &file,
            "new.txt",
            None,
            None,
            Some("keep\nextra\n"),
            &cancel,
        );
        let marked: Vec<&str> =
            hl.highlighted_lines.iter().map(|&idx| file.lines[idx].content.as_str()).collect();
        assert_eq!(marked, vec!["keep"]);
    }

    #[test]
    fn batch_file_show_skips_empty_paths_and_the_root() {
        if !jj_available() {
            return;
        }
        let temp = tempfile::TempDir::new().unwrap();
        jj(temp.path(), &["git", "init"]);
        let vcs = JjVcs::new(temp.path().to_path_buf()).unwrap();
        let spawns = AtomicUsize::new(0);
        let cancel = Arc::new(AtomicBool::new(false));
        let empty = batch_jj_file_contents(&vcs, "not-a-root", &[], &spawns, &cancel);
        assert_eq!(spawns.load(Ordering::Relaxed), 0, "an empty path list must not call jj");
        assert!(empty.unwrap().is_empty());
        let root = "0".repeat(40);
        let rooted =
            batch_jj_file_contents(&vcs, &root, &["file.txt".to_string()], &spawns, &cancel);
        assert_eq!(spawns.load(Ordering::Relaxed), 0, "the root commit must not call jj");
        assert!(rooted.unwrap().is_empty());
    }

    #[test]
    fn working_copy_span_lists_only_files_git_sees_since_the_parent() {
        if !jj_available() {
            return;
        }
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        jj(repo, &["git", "init"]);
        assert!(repo.join(".git").is_dir(), "jj git init must leave a colocated .git directory");
        std::fs::write(repo.join("earlier.txt"), "earlier\n").unwrap();
        jj(repo, &["commit", "-m", "earlier"]);
        let parent = jj_text(repo, &["log", "-r", "@-", "--no-graph", "-T", "commit_id"]);
        assert!(!is_root_commit_id(&parent), "{parent}");
        // Unsnapshotted. A later jj command would record it and hide the git path.
        std::fs::write(repo.join("later.txt"), "later\n").unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let branch = vec![
            ChangedFile { path: "earlier.txt".to_string(), old_path: None },
            ChangedFile { path: "later.txt".to_string(), old_path: None },
        ];
        let spawns = AtomicUsize::new(0);
        let cancel = Arc::new(AtomicBool::new(false));
        let map = span_changed_files(
            &vcs,
            &[parent],
            &SpanTarget::WorkingCopy,
            &branch,
            &spawns,
            &cancel,
        )
        .unwrap();
        assert!(map.contains_key("later.txt"), "{map:?}");
        assert!(
            !map.contains_key("earlier.txt"),
            "the git path must not include the parent commit's file: {map:?}"
        );
    }

    #[test]
    fn unsnapshotted_rename_is_a_post_rename_on_a_revision_span() {
        if !jj_available() {
            return;
        }
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        jj(repo, &["git", "init"]);
        assert!(repo.join(".git").is_dir(), "jj git init must leave a colocated .git directory");
        std::fs::write(repo.join("a.txt"), "same line\nsecond\n").unwrap();
        jj(repo, &["commit", "-m", "add a"]);
        let commit = jj_text(repo, &["log", "-r", "@-", "--no-graph", "-T", "commit_id"]);
        std::fs::rename(repo.join("a.txt"), repo.join("renamed.txt")).unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let spawns = AtomicUsize::new(0);
        let cancel = Arc::new(AtomicBool::new(false));
        let map =
            span_post_renames(&vcs, &SpanTarget::Revision(commit), "@", true, &spawns, &cancel)
                .unwrap();
        assert_eq!(map.get("renamed.txt").map(String::as_str), Some("a.txt"), "{map:?}");
    }

    #[test]
    fn binary_file_under_head_carries_the_highlight_label() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        std::fs::write(repo.join("data.bin"), b"\0not-an-image").unwrap();
        jj(repo, &["commit", "-m", "binary"]);
        jj(repo, &["bookmark", "set", "feature", "-r", "@-"]);
        jj(repo, &["new", "main"]);
        let vcs = JjVcs::with_options(
            repo.to_path_buf(),
            crate::vcs::VcsOptions {
                base: None,
                head: Some("feature"),
                highlight: Some("feature"),
            },
        )
        .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let refresh = vcs.refresh(&cancel).unwrap();
        let bin = refresh
            .files
            .iter()
            .find(|f| f.lines.first().is_some_and(|l| l.file_path.as_deref() == Some("data.bin")))
            .expect("data.bin");
        assert!(bin.lines.iter().any(|l| l.content == "[binary file]"), "{:?}", bin.lines);
        assert_eq!(bin.highlight.label.as_deref(), Some("feature"));
    }

    #[test]
    fn first_commit_above_the_root_is_highlighted() {
        if !jj_available() {
            return;
        }
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        jj(repo, &["git", "init"]);
        std::fs::write(repo.join("first.txt"), "from-root\n").unwrap();
        jj(repo, &["commit", "-m", "first"]);
        let vcs = JjVcs::with_options(
            repo.to_path_buf(),
            crate::vcs::VcsOptions { base: None, head: Some("@-"), highlight: Some("@-") },
        )
        .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let refresh = vcs.refresh(&cancel).unwrap();
        let file = refresh
            .files
            .iter()
            .find(|f| f.lines.first().is_some_and(|l| l.file_path.as_deref() == Some("first.txt")))
            .expect("first.txt");
        let idx = file.lines.iter().position(|l| l.content == "from-root").expect("from-root");
        assert!(file.highlight.is_line_highlighted(idx), "{:?}", file.highlight);
        assert_eq!(file.highlight.label.as_deref(), Some("@-"));
    }
}
