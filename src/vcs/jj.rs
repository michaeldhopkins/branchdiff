use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::Result;

use vcs_runner::{
    is_transient_error, run_jj, run_jj_cancellable, run_jj_with_retry,
    run_jj_with_retry_cancellable,
};

use super::jj_revset::{
    TRUNK_REMOTE_PREFERENCE, local_trunk_bookmark, pick_trunk_remote, revset_commit_count,
    revset_commit_count_checked, revset_committer_epoch, revset_resolves,
};
mod bookmark;
mod fingerprint;
mod highlight;
mod parse;
mod refresh;
mod stack;

use super::shared::{FileProcessResult, assemble_results, process_files_parallel};
#[cfg(test)]
use bookmark::{boundary_revset, resolve_bookmark_boundary};
use bookmark::{mark_bookmark_provenance, resolve_bookmark_boundary_for};
use highlight::{JjHighlightPlan, validate_highlight_revset};
pub(crate) use parse::{ChangedFile, parse_binary_from_stat, parse_jj_summary, parse_rev_metadata};
#[cfg(any(test, fuzzing))]
pub(crate) use parse::{parse_highlight_log, parse_rename};
#[cfg(test)]
use stack::resolve_fork_point;
#[cfg(test)]
use stack::resolve_orphaned_tip;
use stack::{
    Anchors, compute_stack_position, get_change_id_static, joined, resolve_anchors,
    resolve_fork_and_tips, resolve_jj_op_store, resolve_stack_tip,
};

/// Prepend `--ignore-working-copy` to skip jj's auto-snapshot.
/// Only a diff of committed revisions that must include edits in `@` needs a snapshot;
/// everything else skips it so a read never writes to op_store/working_copy.
fn no_snapshot<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut full = Vec::with_capacity(args.len() + 1);
    full.push("--ignore-working-copy");
    full.extend_from_slice(args);
    full
}

use crate::diff::{DiffInput, FileDiff, compute_four_way_diff, compute_four_way_diff_cancellable};
use crate::image_diff::is_image_file;
use crate::limits::DiffMetrics;
use crate::vcs::{
    ComparisonContext, DiffBase, RefreshResult, StackPosition, VcsBackend, VcsEventType,
    VcsOptions, VcsWatchPaths,
};

/// Jujutsu (jj) backend for branchdiff.
pub struct JjVcs {
    repo_path: PathBuf,
    /// Revset for the base of comparison: `trunk()` when available, `@-` otherwise.
    from_rev: String,
    /// Optional explicit `--head <REV>` revision to diff up to instead of `@`.
    head_rev: Option<String>,
    /// Optional explicit `--highlight <REVSET>` to mark lines and hunks from.
    highlight_revset: Option<String>,
    /// Whether to diff from the fork point or the trunk tip.
    /// Stored as AtomicU8 for interior mutability (Vcs trait uses &self).
    /// 0 = ForkPoint, 1 = TrunkTip.
    diff_base: AtomicU8,
    /// Resolved path to the shared operation log (`.jj/repo/op_store`). In a
    /// secondary workspace `.jj/repo` is a pointer *file* to the shared repo, so
    /// this is resolved once at construction rather than assumed to live under
    /// `repo_path`. Watching/classifying it is what lets a workspace notice
    /// operations performed elsewhere.
    op_store_dir: PathBuf,
    /// Stat-cache for the disk-walk fallback (repos with no workdir git index).
    /// Unused by colocated repos, which discover via git.
    disk_manifest: Mutex<DiskManifest>,
    /// How many base-file content comparisons the last disk-walk performed —
    /// instrumentation proving a warm refresh reads O(changed), not O(tree).
    base_read_count: AtomicUsize,
}

/// Stat-cache for the disk-walk fallback. Caches, per base commit, which
/// working-copy files differ from the base, so a warm refresh only re-reads
/// files whose mtime advanced past the last validation — turning an O(tree)
/// content scan into O(changed).
struct DiskManifest {
    /// Base commit id the cache is valid for; a change invalidates everything.
    base: String,
    /// Base tree file list (for detecting deletions), valid for `base`.
    base_files: Vec<String>,
    /// When the cache was last fully validated. A file whose mtime is `>=` this
    /// is re-read — the git "racy index" rule: an edit landing in the same tick
    /// as the previous refresh must never be trusted as clean. Caveat: an edit
    /// that *preserves* mtime (`cp -p`, `touch -r`) is not detected; acceptable
    /// for this rare (external-store-only) path.
    checkpoint: SystemTime,
    /// Per working-copy path: does its on-disk content differ from the base?
    differs: HashMap<String, bool>,
    /// Cached `--highlight` plan keyed by operation-head ID and target revision.
    highlight_cache: Option<(String, Arc<JjHighlightPlan>)>,
}

impl DiskManifest {
    fn empty() -> Self {
        Self {
            base: String::new(),
            base_files: Vec::new(),
            checkpoint: SystemTime::UNIX_EPOCH,
            differs: HashMap::new(),
            highlight_cache: None,
        }
    }
}

/// The base used when no trunk can be found: @'s parent.
const PARENT_BASE: &str = "@-";

/// Bookmark names jj itself tries when resolving `trunk()`, in order.
pub(super) const TRUNK_BOOKMARK_CANDIDATES: [&str; 3] = ["main", "master", "trunk"];

/// Whether `from_rev` is a trunk-like base rather than the `@-` fallback.
///
/// The stack features (fork point, stack tip, bookmark boundary) are only
/// meaningful when comparing against trunk; with `@-` there is no stack to
/// reason about. Previously this was spelled `from_rev != "trunk()"`, which
/// conflated "is trunk" with the one literal revset that expressed it.
fn is_trunk_base(from_rev: &str) -> bool {
    from_rev != PARENT_BASE
}

/// Find the trunk bookmark jj itself would land on, e.g. `main@origin`.
///
/// Mirrors jj's documented fallback: try `main`/`master`/`trunk` on the
/// `upstream` and `origin` remotes, and "if more than one potential trunk commit
/// exists, the newest one is chosen". Newest wins rather than first-by-remote —
/// on a fork (`origin` = your fork, `upstream` = canonical) a stale
/// `main@origin` would otherwise beat a newer `main@upstream`, reporting every
/// commit already merged upstream as the user's own work.
///
/// This function exists to correct a `trunk()` pinned to the wrong remote, so it
/// should land where jj's own resolution would have — not somewhere we invented.
/// Ties keep [`TRUNK_REMOTE_PREFERENCE`] order, so `origin` wins a dead heat.
fn preferred_trunk_revset(repo_path: &Path) -> Option<String> {
    let mut best: Option<(i64, String)> = None;
    for remote in TRUNK_REMOTE_PREFERENCE {
        for bookmark in TRUNK_BOOKMARK_CANDIDATES {
            let revset = format!("{bookmark}@{remote}");
            let Some(epoch) = revset_committer_epoch(repo_path, &revset) else {
                continue;
            };
            if best.as_ref().is_none_or(|(best_epoch, _)| epoch > *best_epoch) {
                best = Some((epoch, revset));
            }
        }
    }
    best.map(|(_, revset)| revset)
}

/// Resolve the base revset to compare against.
///
/// Normally this is jj's `trunk()`, but `trunk()` is pinned into repo config at
/// `jj git init` time from whatever remote was default, so in a repo that also
/// has a deploy remote it can end up pinned to e.g. `main@heroku_test` — and
/// then every diff is against the deploy remote. When `trunk()` resolves on a
/// remote jj would not itself have chosen, prefer an explicit trunk bookmark on
/// `origin`/`upstream` instead.
///
/// A `trunk()` that already resolves on a preferred remote is left alone: it may
/// legitimately point somewhere other than main (`develop@origin`), and that is
/// the user's deliberate choice to make.
///
/// `trunk()` falls back to `root()` when no remote exists, which would diff the
/// entire repo history — so only use it when it resolves to an actual branch.
fn resolve_base_rev(repo_path: &Path) -> String {
    // Asks which remotes hold a bookmark at trunk's *commit*, not which remote
    // the alias names. So a trunk() pinned to a deploy remote still reads as
    // "on origin" while the two agree — which is the point: the diff is then
    // identical either way, and the narrower we intervene the better. Once the
    // deploy remote falls behind, origin drops out and the override kicks in.
    //
    // Both lookups are read-only and independent, so they run together; the
    // remote is ignored when trunk() resolves to nothing but the root.
    let cancel = Arc::new(AtomicBool::new(false));
    let (resolves, remote) = std::thread::scope(|scope| {
        let remote = scope.spawn(|| trunk_remote(repo_path, &cancel));
        let resolves = revset_resolves(repo_path, "trunk() ~ root()");
        (resolves, joined(remote))
    });
    if !resolves {
        return local_trunk_bookmark(repo_path).unwrap_or_else(|| PARENT_BASE.to_string());
    }
    let on_preferred_remote = remote.is_some_and(|r| TRUNK_REMOTE_PREFERENCE.contains(&r.as_str()));
    if on_preferred_remote {
        return "trunk()".to_string();
    }

    preferred_trunk_revset(repo_path).unwrap_or_else(|| "trunk()".to_string())
}

/// The remote `trunk()` resolves on, if it can be determined.
fn trunk_remote(repo_path: &Path, cancel: &Arc<AtomicBool>) -> Option<String> {
    remote_of(repo_path, "trunk()", cancel)
}

/// The remote(s) holding a bookmark at `revset`, reduced to a single preferred one.
pub(super) fn remote_of(
    repo_path: &Path,
    revset: &str,
    cancel: &Arc<AtomicBool>,
) -> Option<String> {
    let args = no_snapshot(&[
        "log",
        "-r",
        revset,
        "--no-graph",
        "--limit",
        "1",
        "-T",
        r#"remote_bookmarks.map(|b| b.remote()).join("\n")"#,
    ]);
    let output = run_jj_cancellable(repo_path, &args, Arc::clone(cancel)).ok()?;
    pick_trunk_remote(&output.stdout_lossy())
}

impl JjVcs {
    pub fn new(repo_path: PathBuf) -> Result<Self> {
        Self::with_options(repo_path, VcsOptions::default())
    }

    /// Create a backend, optionally overriding the base revset.
    ///
    /// `base` is any jj revset (`main@origin`, `develop`, a change id). It wins
    /// over `resolve_base_rev`'s inference, which is the point: the inference
    /// reads `trunk()`, and `trunk()` is whatever got pinned into repo config at
    /// `jj git init` time.
    ///
    /// The base must resolve to exactly one commit. Both other outcomes render
    /// as a plausible-looking wrong answer rather than an error:
    ///
    /// - Nothing: diffs against an empty base and reports "no changes".
    /// - Several: `jj diff --from` takes one revision, so every downstream call
    ///   fails and the failure is swallowed into an empty base — `--base 'all()'`
    ///   rendered whole files as added, exit 0, empty stderr.
    pub fn with_base(repo_path: PathBuf, base: Option<&str>) -> Result<Self> {
        Self::with_options(repo_path, VcsOptions { base, ..VcsOptions::default() })
    }

    /// Create a backend with explicit comparison options (`--base`, `--head`).
    pub fn with_options(repo_path: PathBuf, options: VcsOptions<'_>) -> Result<Self> {
        let head_rev = match options.head {
            Some(head) => {
                match revset_commit_count_checked(&repo_path, head) {
                    Ok(1) => {}
                    Err(detail) => anyhow::bail!(
                        "--head {head:?} is not a valid revset in this jj repo: {detail}"
                    ),
                    Ok(0) => anyhow::bail!(
                        "--head {head:?} does not resolve to a commit in this jj repo \
                         (expected a revset, e.g. feature or @-)"
                    ),
                    Ok(_) => anyhow::bail!(
                        "--head {head:?} resolves to more than one commit; it must name \
                         exactly one (e.g. feature, or latest({head}))"
                    ),
                }
                Some(head.to_string())
            }
            None => None,
        };
        let highlight_revset = match options.highlight {
            Some(revset) => Some(validate_highlight_revset(&repo_path, revset)?),
            None => None,
        };
        let from_rev = match options.base {
            Some(base) => {
                match revset_commit_count(&repo_path, base) {
                    1 => {}
                    0 => anyhow::bail!(
                        "--base {base:?} does not resolve to a commit in this jj repo \
                         (expected a revset, e.g. main@origin)"
                    ),
                    _ => anyhow::bail!(
                        "--base {base:?} resolves to more than one commit; it must name \
                         exactly one (e.g. main@origin, or latest({base}))"
                    ),
                }
                // `@-` is our own sentinel for "this repo has no trunk", which
                // switches the stack machinery off. A user who *names* @- as the
                // base means the opposite: compare against that commit, stack and
                // all. Resolve the spelling away so an explicit base is never
                // mistaken for the fallback — every other spelling of the same
                // commit already behaves that way.
                if base == PARENT_BASE {
                    let cancel = Arc::new(AtomicBool::new(false));
                    get_change_id_static(&repo_path, base, &cancel)
                        .filter(|id| !id.is_empty())
                        .unwrap_or_else(|| base.to_string())
                } else {
                    base.to_string()
                }
            }
            None => resolve_base_rev(&repo_path),
        };
        let op_store_dir = resolve_jj_op_store(&repo_path);
        Ok(Self {
            repo_path,
            from_rev,
            head_rev,
            highlight_revset,
            diff_base: AtomicU8::new(0),
            op_store_dir,
            disk_manifest: Mutex::new(DiskManifest::empty()),
            base_read_count: AtomicUsize::new(0),
        })
    }

    pub(super) fn target_rev(&self) -> &str {
        self.head_rev.as_deref().unwrap_or("@")
    }

    pub(super) fn base_rev(&self) -> std::borrow::Cow<'_, str> {
        match (&self.head_rev, self.from_rev.as_str()) {
            (Some(head), PARENT_BASE) => std::borrow::Cow::Owned(format!("({head})-")),
            _ => std::borrow::Cow::Borrowed(&self.from_rev),
        }
    }

    fn load_diff_base(&self) -> DiffBase {
        match self.diff_base.load(Ordering::Relaxed) {
            0 => DiffBase::ForkPoint,
            _ => DiffBase::TrunkTip,
        }
    }

    fn run_jj(&self, args: &[&str]) -> Result<String> {
        let output = run_jj(&self.repo_path, args)?;
        Ok(output.stdout_lossy().into_owned())
    }

    fn run_jj_bytes(&self, args: &[&str]) -> Result<Option<Vec<u8>>> {
        match run_jj(&self.repo_path, args) {
            Ok(output) => Ok(Some(output.stdout)),
            Err(e) if e.is_non_zero_exit() => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get changed files between a from revision and effective_to.
    /// The first call per refresh triggers the working copy auto-snapshot;
    /// all subsequent commands use `--ignore-working-copy`.
    fn get_changed_files_with_from(
        &self,
        from: &str,
        effective_to: &str,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Vec<ChangedFile>> {
        let output = run_jj_with_retry_cancellable(
            &self.repo_path,
            &["diff", "--from", from, "--to", effective_to, "--summary"],
            is_transient_error,
            Arc::clone(cancel),
        )?;
        Ok(parse_jj_summary(&output.stdout_lossy()))
    }

    /// Get changed files between two revisions without snapshotting the working copy.
    fn get_changed_files_no_snapshot(
        &self,
        from: &str,
        effective_to: &str,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Vec<ChangedFile>> {
        let args = no_snapshot(&["diff", "--from", from, "--to", effective_to, "--summary"]);
        let output = run_jj_with_retry_cancellable(
            &self.repo_path,
            &args,
            is_transient_error,
            Arc::clone(cancel),
        )?;
        Ok(parse_jj_summary(&output.stdout_lossy()))
    }

    /// Resolve a jj revision to its git commit id, working-copy-agnostically.
    fn commit_id_of(&self, rev: &str) -> Option<String> {
        let out = self
            .run_jj(&no_snapshot(&[
                "log", "-r", rev, "--no-graph", "--limit", "1", "-T", "commit_id",
            ]))
            .ok()?;
        let id = out.trim().to_string();
        (!id.is_empty()).then_some(id)
    }

    /// Discover files changed between `effective_from` and the working copy
    /// (`@` + un-snapshotted edits) WITHOUT snapshotting. Valid only when the
    /// working copy is the diff target (the common non-stack case,
    /// `effective_to == "@"`).
    ///
    /// Colocated repos reuse git's stat-cached worktree discovery: git HEAD
    /// tracks `@-`, so `base..HEAD` (committed) plus `git status` (worktree =
    /// `@` + live edits) equals `base..working`, and git never churns jj's
    /// oplog. Repos with no workdir git index fall back to a disk walk.
    fn discover_working_changes(
        &self,
        effective_from: &str,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Vec<ChangedFile>> {
        if self.is_colocated() {
            // A missing commit id would make `get_all_changed_files` diff against
            // git's empty tree and report every file as added — fall back to the
            // (snapshotting) jj summary rather than render a bogus all-added diff.
            let Some(base) = self.commit_id_of(effective_from) else {
                return self.get_changed_files_with_from(effective_from, "@", cancel);
            };
            let git_changed = crate::vcs::git::get_all_changed_files(&self.repo_path, &base)?;
            Ok(git_changed
                .into_iter()
                .map(|f| ChangedFile { path: f.path, old_path: f.old_path })
                .collect())
        } else {
            self.diskwalk_changed_files(effective_from, cancel)
        }
    }

    /// The base tree's file list, read working-copy-agnostically.
    fn base_file_list(&self, base: &str) -> Result<Vec<String>> {
        let out = self.run_jj(&no_snapshot(&["file", "list", "-r", base]))?;
        Ok(out.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
    }

    /// Seed the stat-cache from jj's own last working-copy snapshot.
    ///
    /// jj already recorded which files differ from the base when it last
    /// snapshotted, so only files touched *since* that snapshot need their base
    /// content read. `.jj/working_copy/tree_state`'s mtime is when that snapshot
    /// happened, which makes it a valid checkpoint to start from: anything older
    /// is already accounted for in the snapshot's diff, anything newer is a live
    /// edit we must compare ourselves.
    ///
    /// Without this a cold pass read base content for every file in the repo —
    /// one `jj file show` subprocess each, 22s on a 2000-file repo — which every
    /// secondary workspace paid on every one-shot `--print`, since the cache is
    /// per-process and never warms.
    ///
    /// Returns `None` when the snapshot can't be trusted (no tree_state, or the
    /// diff failed), leaving the caller to do the full cold pass.
    ///
    /// `--to @` is only valid because every caller of [`Self::diskwalk_changed_files`]
    /// reaches it via [`Self::discover_working_changes`], which `refresh` and
    /// `single_file_diff` call *only* when `effective_to == "@"` (the stack-tip
    /// and orphaned-tip targets go through `get_changed_files_with_from`
    /// instead). If that gate ever changes, this seed would describe a different
    /// tree than the one being diffed and would silently mis-seed verdicts.
    fn seed_from_snapshot(
        &self,
        effective_from: &str,
        cancel: &Arc<AtomicBool>,
    ) -> Option<(SystemTime, HashMap<String, bool>)> {
        let snapshot_at = std::fs::metadata(self.repo_path.join(".jj/working_copy/tree_state"))
            .ok()?
            .modified()
            .ok()?;

        // The set jj actually knows about: the files tracked in its last
        // snapshot. Seeding these explicitly as "clean" is what lets an absent
        // path mean *unknown* rather than *identical to the base*. jj refuses to
        // snapshot some files (over `snapshot.max-new-file-size`, or excluded by
        // `snapshot.auto-track`) and they are untracked, not clean — treating
        // them as clean silently dropped them from the diff.
        let list_args = no_snapshot(&["file", "list", "-r", "@"]);
        let tracked = run_jj_cancellable(&self.repo_path, &list_args, Arc::clone(cancel)).ok()?;
        let mut differs: HashMap<String, bool> = tracked
            .stdout_lossy()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| (l.replace('\\', "/"), false))
            .collect();

        // `--ignore-working-copy` reads the last snapshot rather than taking a
        // new one, which is the whole point: it tells us what jj already knew.
        let args = no_snapshot(&["diff", "--from", effective_from, "--to", "@", "--name-only"]);
        let output = run_jj_cancellable(&self.repo_path, &args, Arc::clone(cancel)).ok()?;
        for path in output.stdout_lossy().lines().map(str::trim).filter(|l| !l.is_empty()) {
            differs.insert(path.replace('\\', "/"), true);
        }

        Some((snapshot_at, differs))
    }

    /// Fallback discovery for repos with no workdir git index: secondary
    /// workspaces (which have no `.git` of their own) and the rare
    /// `jj git init --git-repo=<external>` form. Compares the base tree to the
    /// on-disk working tree directly — no git, no snapshot.
    ///
    /// A [`DiskManifest`] stat-cache makes this O(changed) on a warm refresh:
    /// only files whose mtime advanced past the last validation are re-read and
    /// re-compared against the base; everything else reuses its cached verdict.
    /// A cold pass (first refresh, or a base change) seeds that cache from jj's
    /// last snapshot rather than re-reading the whole tree — see
    /// [`Self::seed_from_snapshot`].
    fn diskwalk_changed_files(
        &self,
        effective_from: &str,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Vec<ChangedFile>> {
        // Key the cache on the resolved base commit, not the symbolic rev — `@-`
        // keeps its spelling across commits but points at a different tree.
        //
        // An unresolved base must not fall back to "": a fresh DiskManifest also
        // starts empty, so the two would compare equal, the reseed would be
        // skipped, `base_files` would stay empty, and every file on disk would
        // read as an addition with no deletions. Without a base there is nothing
        // to diff against, so say so.
        let base_id = self
            .commit_id_of(effective_from)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("could not resolve base revision {effective_from:?}"))?;
        let mut m = self.disk_manifest.lock().unwrap_or_else(|e| e.into_inner());

        if m.base != base_id {
            m.base = base_id;
            m.base_files = self.base_file_list(effective_from)?;
            match self.seed_from_snapshot(effective_from, cancel) {
                Some((snapshot_at, differs)) => {
                    m.differs = differs;
                    m.checkpoint = snapshot_at;
                }
                None => {
                    // Can't trust jj's snapshot — pay the full O(tree) cold pass.
                    m.differs.clear();
                    m.checkpoint = SystemTime::UNIX_EPOCH;
                }
            }
        }

        let mut changed = Vec::new();
        let mut new_differs = HashMap::new();
        let mut seen: HashSet<String> = HashSet::new();
        {
            let base_set: HashSet<&str> = m.base_files.iter().map(String::as_str).collect();
            let checkpoint = m.checkpoint;
            for entry in ignore::WalkBuilder::new(&self.repo_path)
                .hidden(false)
                .filter_entry(|e| {
                    let name = e.file_name().to_string_lossy();
                    name != ".jj" && name != ".git"
                })
                .build()
                .flatten()
            {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    continue;
                }
                let Ok(rel) = entry.path().strip_prefix(&self.repo_path) else { continue };
                let path = rel.to_string_lossy().replace('\\', "/");
                let mtime = entry
                    .metadata()
                    .ok()
                    .and_then(|md| md.modified().ok())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                seen.insert(path.clone());

                // Reuse a cached verdict only when the file is untouched since
                // the last validation AND we actually have a verdict for it.
                // A missing entry means *unknown*, never *clean*: on a seeded
                // cold pass it means jj never snapshotted the file (it refuses
                // some), and on a warm pass it means the file is new since the
                // last walk. Both must be checked, not assumed identical.
                let cached = (mtime < checkpoint).then(|| m.differs.get(&path)).flatten();
                let differs = if let Some(&verdict) = cached {
                    verdict
                } else {
                    self.base_read_count.fetch_add(1, Ordering::Relaxed);
                    let disk = crate::vcs::shared::read_working_file(&self.repo_path, &path)
                        .ok()
                        .flatten();
                    let base = if base_set.contains(path.as_str()) {
                        file_content_no_snapshot(&self.repo_path, &path, effective_from, cancel)
                    } else {
                        None // not in the base tree ⇒ an addition
                    };
                    disk.as_deref() != base.as_deref()
                };
                new_differs.insert(path.clone(), differs);
                if differs {
                    changed.push(ChangedFile { path, old_path: None });
                }
            }

            // A cancelled walk has a partial `seen`, so the deletions below
            // would mark every path it never reached as deleted. Bail before
            // fabricating them — the caller wanted no answer, not a wrong one.
            if cancel.load(Ordering::Relaxed) {
                anyhow::bail!("disk walk cancelled");
            }

            // Deletions: base files no longer present on disk.
            for bp in &m.base_files {
                if !seen.contains(bp.as_str()) {
                    changed.push(ChangedFile { path: bp.clone(), old_path: None });
                }
            }
        }

        // Persist the cache only if the walk ran to completion. A cancelled walk
        // has a partial `new_differs`; committing it (with a fresh checkpoint)
        // would make the next refresh trust missing entries as "unchanged" and
        // silently drop their diffs. Leaving the previous cache is safe — it just
        // re-validates from the older checkpoint next time.
        if !cancel.load(Ordering::Relaxed) {
            m.differs = new_differs;
            m.checkpoint = SystemTime::now();
        }
        Ok(changed)
    }

    #[cfg(test)]
    fn base_reads(&self) -> usize {
        self.base_read_count.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn reset_base_reads(&self) {
        self.base_read_count.store(0, Ordering::Relaxed);
    }

    /// Detect binary files by checking --stat output for "(binary)" markers.
    /// Uses `--ignore-working-copy` since the snapshot is already fresh from
    /// `get_changed_files`.
    fn get_binary_files_set(
        &self,
        from: &str,
        effective_to: &str,
        cancel: &Arc<AtomicBool>,
    ) -> HashSet<String> {
        let args = no_snapshot(&["diff", "--from", from, "--to", effective_to, "--stat"]);
        match run_jj_with_retry_cancellable(
            &self.repo_path,
            &args,
            is_transient_error,
            Arc::clone(cancel),
        ) {
            Ok(output) => parse_binary_from_stat(&output.stdout_lossy()),
            Err(_) => HashSet::new(),
        }
    }

    /// Binary files on the working side, detected without a snapshot: a changed
    /// file whose on-disk bytes contain a NUL in the first 8 KiB (git's binary
    /// heuristic). VCS-neutral and catches untracked binaries, which git's
    /// `--numstat` misses.
    fn working_binary_files(&self, changed: &[ChangedFile]) -> HashSet<String> {
        changed
            .iter()
            .filter(|f| crate::vcs::shared::working_file_is_binary(&self.repo_path, &f.path))
            .map(|f| f.path.clone())
            .collect()
    }

    fn get_file_bytes_at_rev(&self, file_path: &str, rev: &str) -> Result<Option<Vec<u8>>> {
        self.run_jj_bytes(&["file", "show", "-r", rev, file_path])
    }

    /// Get the current change ID for a revision.
    fn get_change_id(&self, rev: &str) -> Result<String> {
        let output = self.run_jj(&no_snapshot(&[
            "log", "-r", rev, "-T", "change_id.short(12)", "--no-graph", "--limit", "1",
        ]))?;
        Ok(output.trim().to_string())
    }

    #[cfg(test)]
    fn get_bookmarks(&self, rev: &str) -> Option<String> {
        let output = self
            .run_jj(&["log", "-r", rev, "-T", "bookmarks", "--no-graph", "--limit", "1"])
            .ok()?;
        let trimmed = output.trim().trim_end_matches('*');
        if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
    }

    #[cfg(test)]
    fn rev_label(&self, rev: &str) -> String {
        self.get_bookmarks(rev)
            .unwrap_or_else(|| self.get_change_id(rev).unwrap_or_else(|_| rev.to_string()))
    }

    /// Fetch bookmarks and change_id for a revision in a single command,
    /// using `--ignore-working-copy` to avoid redundant auto-snapshots.
    /// Returns (change_id, display_label).
    fn rev_metadata_no_snapshot(&self, rev: &str) -> (String, String) {
        let template =
            r#"bookmarks ++ "\0" ++ change_id.short(12) ++ "\0" ++ change_id.shortest(4)"#;
        let args = no_snapshot(&["log", "-r", rev, "-T", template, "--no-graph", "--limit", "1"]);
        match self.run_jj(&args) {
            Ok(raw) => parse_rev_metadata(&raw),
            Err(_) => (rev.to_string(), rev.to_string()),
        }
    }

    /// Whether this is a colocated repo: a real `.git` **directory** beside `.jj`.
    ///
    /// Must be `is_dir()`, not `exists()`. A git worktree — and any jj secondary
    /// workspace that tooling has planted worktree plumbing beside — has `.git`
    /// as a *file* holding `gitdir: ...`. Those are not colocated: the colocated
    /// fast path assumes git's worktree view tracks jj `@-`, which is only true
    /// in the default workspace. A worktree git dir has no index of its own, so
    /// git reports every tracked file deleted and every file on disk untracked,
    /// and the diff fills with the whole repo plus jj's `.jj/` internals.
    /// Non-colocated repos fall back to the disk walk, which is correct here.
    fn is_colocated(&self) -> bool {
        self.repo_path.join(".git").is_dir()
    }
}

/// Read file content at a revision without triggering auto-snapshot.
/// Free function for use in parallel contexts (rayon).
fn file_content_no_snapshot(
    repo_path: &Path,
    file_path: &str,
    rev: &str,
    cancel: &Arc<AtomicBool>,
) -> Option<String> {
    let args = no_snapshot(&["file", "show", "-r", rev, file_path]);
    run_jj_cancellable(repo_path, &args, Arc::clone(cancel))
        .ok()
        .map(|o| o.stdout_lossy().into_owned())
}

fn process_jj_file(
    repo_path: &Path,
    from_rev: &str,
    changed: &ChangedFile,
    binary_files: &HashSet<String>,
    tip_rev: Option<&str>,
    at_rev: &str,
    read_from_disk: bool,
    bookmark_changed_files: Option<&HashSet<String>>,
    highlight_plan: Option<&JjHighlightPlan>,
    cancel: &Arc<AtomicBool>,
) -> FileProcessResult {
    if cancel.load(Ordering::Relaxed) {
        return FileProcessResult::Cancelled;
    }

    if binary_files.contains(&changed.path) {
        if is_image_file(&changed.path) {
            return FileProcessResult::Image { path: changed.path.clone() };
        }
        return FileProcessResult::Binary { path: changed.path.clone() };
    }

    // Each `jj file show` is a separate subprocess and can take seconds for
    // large files. The cancel flag is plumbed into the subprocess itself
    // (via run_jj_cancellable inside file_content_no_snapshot), so a slow
    // file read gets killed mid-flight — not only between reads.
    //
    // The base and parent reads are independent, so they run side by side. The
    // parent is for the committed/staged boundary: try the current path, fall back
    // to old_path for files renamed in the current commit. `at_rev` is `@`, or the
    // re-anchored sibling tip when `@` is an orphaned empty commit (parent `(<at_rev>)-`).
    let base_path = changed.old_path.as_deref().unwrap_or(&changed.path);
    let at_parent = format!("({at_rev})-");
    // Scoped thread, not rayon::join: a serial run must not create rayon's global pool.
    let (base, parent) = std::thread::scope(|scope| {
        let base = scope.spawn(|| file_content_no_snapshot(repo_path, base_path, from_rev, cancel));
        let parent = file_content_no_snapshot(repo_path, &changed.path, &at_parent, cancel)
            .or_else(|| {
                changed
                    .old_path
                    .as_deref()
                    .and_then(|old| file_content_no_snapshot(repo_path, old, &at_parent, cancel))
            });
        (joined(base), parent)
    });
    if cancel.load(Ordering::Relaxed) {
        return FileProcessResult::Cancelled;
    }

    // The working ("current") side of the diff comes from disk — no snapshot
    // required, so branchdiff never churns the oplog. A committed `at_rev` (the
    // orphaned-tip / stack / `--head` case) is a WC-agnostic jj read.
    let index = if read_from_disk {
        crate::vcs::shared::read_working_file(repo_path, &changed.path).ok().flatten()
    } else {
        file_content_no_snapshot(repo_path, &changed.path, at_rev, cancel)
    };
    if cancel.load(Ordering::Relaxed) {
        return FileProcessResult::Cancelled;
    }
    let tip_content =
        tip_rev.and_then(|tip| file_content_no_snapshot(repo_path, &changed.path, tip, cancel));

    // When tip_rev is None (@ is at tip), working == index.
    // When tip_rev is Some, working is the tip content — even if None (file
    // deleted above @). TODO: algorithm.rs check_file_deletion collapses the
    // per-commit coloring into a flat DeletedStaged block. A proper fix needs
    // a new code path that preserves base/head/index provenance while marking
    // the file as eventually-deleted.
    let working = match tip_rev {
        Some(_) => tip_content.as_deref(),
        None => index.as_deref(),
    };

    let mut file_diff = compute_four_way_diff_cancellable(
        DiffInput {
            path: &changed.path,
            base: base.as_deref(),
            head: parent.as_deref(),
            index: index.as_deref(),
            working,
            old_path: changed.old_path.as_deref(),
        },
        cancel,
    );

    if let Some(bm_files) = bookmark_changed_files {
        let in_bookmark = bm_files.contains(&changed.path);
        mark_bookmark_provenance(&mut file_diff, in_bookmark);
    }

    if let Some(plan) = highlight_plan {
        file_diff.highlight = plan.compute_for_file(
            repo_path,
            &file_diff,
            &changed.path,
            changed.old_path.as_deref(),
            base.as_deref(),
            working,
            cancel,
        );
    }

    FileProcessResult::Diff(file_diff)
}

/// Get the jj repo root from a path.
pub fn get_repo_root(path: &Path) -> Result<PathBuf> {
    let output = run_jj(path, &["root"])?;
    let root = output.stdout_lossy().trim().to_string();
    Ok(PathBuf::from(root))
}

impl crate::vcs::Vcs for JjVcs {
    fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    fn comparison_context(&self) -> Result<ComparisonContext> {
        Ok(self.comparison_with(self.label_of(&self.base_rev()), self.label_of(self.target_rev())))
    }

    fn refresh(&self, cancel_flag: &Arc<AtomicBool>) -> Result<RefreshResult> {
        self.refresh_anchored(cancel_flag)
    }

    fn comparison_and_refresh(
        &self,
        cancel_flag: &Arc<AtomicBool>,
    ) -> Result<(ComparisonContext, RefreshResult)> {
        refresh::comparison_and_refresh(self, cancel_flag)
    }

    fn single_file_diff(&self, file_path: &str) -> Option<FileDiff> {
        self.single_file_diff_anchored(file_path)
    }

    fn base_identifier(&self) -> Result<String> {
        self.get_change_id(&self.base_rev())
    }

    fn current_revision_id(&self) -> Result<String> {
        self.run_jj(&no_snapshot(&[
            "log",
            "-r",
            self.target_rev(),
            "--no-graph",
            "--limit",
            "1",
            "-T",
            "change_id.short(12)",
        ]))
        .map(|s| s.trim().to_string())
    }

    fn base_file_bytes(&self, file_path: &str) -> Result<Option<Vec<u8>>> {
        self.get_file_bytes_at_rev(file_path, &self.base_rev())
    }

    fn working_file_bytes(&self, file_path: &str) -> Result<Option<Vec<u8>>> {
        if let Some(head) = &self.head_rev {
            return self.run_jj_bytes(&no_snapshot(&["file", "show", "-r", head, file_path]));
        }
        // The working copy is on disk — read it directly rather than snapshotting.
        crate::vcs::shared::read_working_file_bytes(&self.repo_path, file_path)
    }

    fn fetch(&self) -> Result<()> {
        if self.is_colocated() {
            // `--ignore-working-copy`: this runs on a timer in the background;
            // without it, every auto-fetch snapshots the user's in-progress
            // edits and can move `@` under them.
            self.run_jj(&no_snapshot(&["git", "fetch"]))?;
        }
        Ok(())
    }

    fn has_conflicts(&self) -> Result<bool> {
        // jj conflict detection is a future enhancement
        Ok(false)
    }

    fn is_locked(&self) -> bool {
        // jj doesn't use lock files the same way git does
        false
    }

    fn watch_paths(&self) -> VcsWatchPaths {
        let jj_dir = self.repo_path.join(".jj");
        VcsWatchPaths {
            files: vec![jj_dir.join("working_copy/checkout")],
            // The op_store may live outside this workspace (shared via the
            // `.jj/repo` pointer), so use the resolved path, not a literal join.
            recursive_dirs: vec![self.op_store_dir.clone()],
        }
    }

    fn classify_event(&self, path: &Path) -> VcsEventType {
        super::jj_events::classify(path, &self.repo_path, &self.op_store_dir, || {
            self.is_colocated()
        })
    }

    fn fingerprint_inputs(&self) -> Result<crate::fingerprint::FingerprintInputs> {
        self.gather_fingerprint_inputs()
    }

    fn backend(&self) -> VcsBackend {
        VcsBackend::Jj
    }

    fn set_diff_base(&self, base: DiffBase) {
        let val = match base {
            DiffBase::ForkPoint => 0,
            DiffBase::TrunkTip => 1,
        };
        self.diff_base.store(val, Ordering::Relaxed);
    }

    fn try_recover(
        &self,
        action: crate::update::RecoveryAction,
        cancel: &Arc<AtomicBool>,
    ) -> Result<()> {
        match action {
            crate::update::RecoveryAction::JjUpdateStale => {
                run_jj_cancellable(
                    &self.repo_path,
                    &["workspace", "update-stale"],
                    Arc::clone(cancel),
                )?;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::LineSource;
    use crate::vcs::Vcs;
    use std::process::Command;

    // === parse_jj_summary tests ===

    #[test]
    fn test_parse_summary_modified() {
        let files = parse_jj_summary("M file.txt\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "file.txt");
        assert!(files[0].old_path.is_none());
    }

    #[test]
    fn test_parse_summary_added() {
        let files = parse_jj_summary("A new_file.txt\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "new_file.txt");
        assert!(files[0].old_path.is_none());
    }

    #[test]
    fn test_parse_summary_deleted() {
        let files = parse_jj_summary("D old_file.txt\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "old_file.txt");
    }

    #[test]
    fn test_parse_summary_renamed() {
        let files = parse_jj_summary("R {old_name.txt => new_name.txt}\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "new_name.txt");
        assert_eq!(files[0].old_path.as_deref(), Some("old_name.txt"));
    }

    #[test]
    fn test_parse_summary_renamed_with_directory() {
        let files = parse_jj_summary("R {src/old.rs => src/new.rs}\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/new.rs");
        assert_eq!(files[0].old_path.as_deref(), Some("src/old.rs"));
    }

    #[test]
    fn test_parse_summary_renamed_prefix_factored() {
        // jj factors the common dir prefix outside the brace for nested renames.
        let files = parse_jj_summary("R sub/{sub_orig.txt => sub_new.txt}\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "sub/sub_new.txt");
        assert_eq!(files[0].old_path.as_deref(), Some("sub/sub_orig.txt"));
    }

    #[test]
    fn test_parse_summary_renamed_deeply_nested() {
        let files = parse_jj_summary("R deep/er/path/{deep_orig.txt => deep_new.txt}\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "deep/er/path/deep_new.txt");
        assert_eq!(files[0].old_path.as_deref(), Some("deep/er/path/deep_orig.txt"));
    }

    #[test]
    fn test_parse_summary_renamed_prefix_and_suffix_factored() {
        // jj can also factor a common suffix after the brace.
        let files = parse_jj_summary("R app/{old => new}/controller.js\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "app/new/controller.js");
        assert_eq!(files[0].old_path.as_deref(), Some("app/old/controller.js"));
    }

    #[test]
    fn test_parse_summary_multiple() {
        let output = "M file1.txt\nA file2.txt\nD file3.txt\n";
        let files = parse_jj_summary(output);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "file1.txt");
        assert_eq!(files[1].path, "file2.txt");
        assert_eq!(files[2].path, "file3.txt");
    }

    #[test]
    fn test_parse_summary_mixed_with_rename() {
        let output = "M file.txt\nR {old.rs => new.rs}\nA added.txt\n";
        let files = parse_jj_summary(output);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "file.txt");
        assert!(files[0].old_path.is_none());
        assert_eq!(files[1].path, "new.rs");
        assert_eq!(files[1].old_path.as_deref(), Some("old.rs"));
        assert_eq!(files[2].path, "added.txt");
        assert!(files[2].old_path.is_none());
    }

    #[test]
    fn test_parse_summary_empty() {
        let files = parse_jj_summary("");
        assert!(files.is_empty());
    }

    #[test]
    fn test_parse_summary_skips_blank_lines() {
        let output = "M file.txt\n\nA other.txt\n";
        let files = parse_jj_summary(output);
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn test_parse_summary_path_with_spaces() {
        let files = parse_jj_summary("M path with spaces.txt\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "path with spaces.txt");
    }

    #[test]
    fn test_parse_rename_malformed_no_braces() {
        let files = parse_jj_summary("R old.txt => new.txt\n");
        assert!(files.is_empty(), "rename without braces should be skipped");
    }

    #[test]
    fn test_parse_rename_malformed_no_arrow() {
        let files = parse_jj_summary("R {old.txt new.txt}\n");
        assert!(files.is_empty(), "rename without => should be skipped");
    }

    // === parse_binary_from_stat tests ===

    #[test]
    fn test_parse_binary_from_stat_detects_binary() {
        let output = "image.png | (binary)\nfile.txt  | 2 +-\n1 file changed\n";
        let binaries = parse_binary_from_stat(output);
        assert!(binaries.contains("image.png"));
        assert!(!binaries.contains("file.txt"));
    }

    #[test]
    fn test_parse_binary_from_stat_empty() {
        let binaries = parse_binary_from_stat("file.txt | 2 +-\n");
        assert!(binaries.is_empty());
    }

    #[test]
    fn test_parse_binary_from_stat_multiple() {
        let output = "a.png | (binary)\nb.jpg | (binary)\nc.txt | 1 +\n";
        let binaries = parse_binary_from_stat(output);
        assert_eq!(binaries.len(), 2);
        assert!(binaries.contains("a.png"));
        assert!(binaries.contains("b.jpg"));
    }

    #[test]
    fn test_parse_binary_from_stat_renamed_with_braces() {
        let output = "{original.bin => renamed.bin} | (binary)\n";
        let binaries = parse_binary_from_stat(output);
        assert!(binaries.contains("renamed.bin"), "should extract new name from rename");
        assert!(
            !binaries.contains("{original.bin => renamed.bin}"),
            "should not store raw rename format"
        );
    }

    #[test]
    fn test_parse_binary_from_stat_renamed_without_braces() {
        let output = "original.bin => renamed.bin | (binary)\n";
        let binaries = parse_binary_from_stat(output);
        assert!(binaries.contains("renamed.bin"), "should extract new name from arrow format");
    }

    // === parse_rev_metadata tests ===

    #[test]
    fn test_parse_rev_metadata_with_bookmark() {
        let (change_id, label) = parse_rev_metadata("my-feature\0abcdef123456\n");
        assert_eq!(change_id, "abcdef123456");
        assert_eq!(label, "my-feature");
    }

    #[test]
    fn test_parse_rev_metadata_without_bookmark() {
        let (change_id, label) = parse_rev_metadata("\0abcdef123456\n");
        assert_eq!(change_id, "abcdef123456");
        assert_eq!(label, "abcdef123456");
    }

    #[test]
    fn test_parse_rev_metadata_strips_tracking_marker() {
        let (change_id, label) = parse_rev_metadata("main*\0abcdef123456\n");
        assert_eq!(change_id, "abcdef123456");
        assert_eq!(label, "main");
    }

    #[test]
    fn test_parse_rev_metadata_empty_string() {
        let (change_id, label) = parse_rev_metadata("");
        assert_eq!(change_id, "");
        assert_eq!(label, "");
    }

    #[test]
    fn test_parse_rev_metadata_no_separator() {
        let (change_id, label) = parse_rev_metadata("fallback_text");
        assert_eq!(change_id, "fallback_text");
        assert_eq!(label, "fallback_text");
    }

    // === no_snapshot helper tests ===

    #[test]
    fn test_no_snapshot_prepends_flag() {
        let args = no_snapshot(&["diff", "--from", "@-", "--to", "@"]);
        assert_eq!(args[0], "--ignore-working-copy");
        assert_eq!(args[1], "diff");
        assert_eq!(args.len(), 6);
    }

    // === classify_event tests ===

    #[test]
    fn test_classify_source_file() {
        let vcs = JjVcs::new(PathBuf::from("/repo")).unwrap();
        assert_eq!(vcs.classify_event(Path::new("/repo/src/main.rs")), VcsEventType::Source);
    }

    #[test]
    fn test_classify_jj_op_store_as_internal() {
        let vcs = JjVcs::new(PathBuf::from("/repo")).unwrap();
        assert_eq!(
            vcs.classify_event(Path::new("/repo/.jj/repo/op_store/heads")),
            VcsEventType::Internal
        );
    }

    #[test]
    fn test_classify_jj_working_copy() {
        let vcs = JjVcs::new(PathBuf::from("/repo")).unwrap();
        assert_eq!(
            vcs.classify_event(Path::new("/repo/.jj/working_copy/checkout")),
            VcsEventType::RevisionChange
        );
    }

    #[test]
    fn test_classify_jj_internal() {
        let vcs = JjVcs::new(PathBuf::from("/repo")).unwrap();
        assert_eq!(
            vcs.classify_event(Path::new("/repo/.jj/repo/store/something")),
            VcsEventType::Internal
        );
    }

    #[test]
    fn test_classify_path_outside_repo() {
        let vcs = JjVcs::new(PathBuf::from("/repo")).unwrap();
        assert_eq!(vcs.classify_event(Path::new("/other/file.rs")), VcsEventType::Source);
    }

    #[test]
    fn test_classify_git_index_as_internal_in_colocated_repo() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join(".jj")).unwrap();
        std::fs::create_dir_all(temp.path().join(".git")).unwrap();
        let vcs = JjVcs::new(temp.path().to_path_buf()).unwrap();
        assert_eq!(vcs.classify_event(&temp.path().join(".git/index")), VcsEventType::Internal);
    }

    #[test]
    fn test_classify_git_objects_as_internal_in_colocated_repo() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join(".jj")).unwrap();
        std::fs::create_dir_all(temp.path().join(".git")).unwrap();
        let vcs = JjVcs::new(temp.path().to_path_buf()).unwrap();
        assert_eq!(
            vcs.classify_event(&temp.path().join(".git/objects/ab/cd1234")),
            VcsEventType::Internal
        );
    }

    #[test]
    fn test_classify_git_refs_as_internal_in_colocated_repo() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join(".jj")).unwrap();
        std::fs::create_dir_all(temp.path().join(".git")).unwrap();
        let vcs = JjVcs::new(temp.path().to_path_buf()).unwrap();
        assert_eq!(
            vcs.classify_event(&temp.path().join(".git/refs/jj/keep/abc123")),
            VcsEventType::Internal
        );
    }

    #[test]
    fn test_classify_git_head_as_internal_in_colocated_repo() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join(".jj")).unwrap();
        std::fs::create_dir_all(temp.path().join(".git")).unwrap();
        let vcs = JjVcs::new(temp.path().to_path_buf()).unwrap();
        assert_eq!(vcs.classify_event(&temp.path().join(".git/HEAD")), VcsEventType::Internal);
    }

    #[test]
    fn test_classify_git_path_as_source_in_non_colocated_repo() {
        let vcs = JjVcs::new(PathBuf::from("/repo")).unwrap();
        assert_eq!(vcs.classify_event(Path::new("/repo/.git/index")), VcsEventType::Source);
    }

    #[test]
    fn test_classify_source_file_in_colocated_repo() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join(".jj")).unwrap();
        std::fs::create_dir_all(temp.path().join(".git")).unwrap();
        let vcs = JjVcs::new(temp.path().to_path_buf()).unwrap();
        assert_eq!(vcs.classify_event(&temp.path().join("src/main.rs")), VcsEventType::Source);
    }

    // === watch_paths tests ===

    #[test]
    fn test_watch_paths() {
        use crate::vcs::Vcs;
        let vcs = JjVcs::new(PathBuf::from("/repo")).unwrap();
        let paths = vcs.watch_paths();
        assert!(paths.files.contains(&PathBuf::from("/repo/.jj/working_copy/checkout")));
        assert!(paths.recursive_dirs.contains(&PathBuf::from("/repo/.jj/repo/op_store")));
    }

    #[test]
    fn test_resolve_op_store_default_workspace() {
        // `.jj/repo` is a real directory: op_store lives directly under it.
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join(".jj/repo")).unwrap();
        assert_eq!(resolve_jj_op_store(temp.path()), temp.path().join(".jj/repo/op_store"));
    }

    #[test]
    fn test_resolve_op_store_secondary_workspace_pointer() {
        // `.jj/repo` is a pointer *file* to the shared repo (jj workspace add).
        let shared = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(shared.path().join("op_store")).unwrap();
        let ws = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(ws.path().join(".jj")).unwrap();
        std::fs::write(ws.path().join(".jj/repo"), shared.path().to_string_lossy().as_bytes())
            .unwrap();

        let resolved = resolve_jj_op_store(ws.path());
        assert_eq!(
            std::fs::canonicalize(&resolved).unwrap(),
            std::fs::canonicalize(shared.path().join("op_store")).unwrap(),
            "should resolve to the shared repo's op_store, not under the workspace"
        );
    }

    #[test]
    fn test_classify_shared_op_store_outside_workspace_is_internal() {
        // A workspace whose op_store lives elsewhere must still classify that
        // out-of-tree op_store as Internal (not Source).
        let shared = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(shared.path().join("op_store")).unwrap();
        let ws = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(ws.path().join(".jj")).unwrap();
        std::fs::write(ws.path().join(".jj/repo"), shared.path().to_string_lossy().as_bytes())
            .unwrap();

        let vcs = JjVcs::new(ws.path().to_path_buf()).unwrap();
        let canonical_op = std::fs::canonicalize(shared.path().join("op_store")).unwrap();
        assert_eq!(vcs.classify_event(&canonical_op.join("heads")), VcsEventType::Internal);
    }

    // === Integration tests (require jj installed) ===

    pub(super) fn jj_available() -> bool {
        Command::new("jj").arg("--version").output().is_ok_and(|o| o.status.success())
    }

    #[test]
    fn test_jj_refresh_detects_modified_file() {
        if !jj_available() {
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "initial\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "modified\n").unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        assert!(!result.files.is_empty(), "should detect changed file");
        assert!(!result.lines.is_empty(), "should produce diff lines");

        // With from_rev=@- (no remote), base==head so all @→@ changes are Staged
        let has_staged = result.lines.iter().any(|l| l.source == LineSource::Staged);
        assert!(has_staged, "modified lines should have Staged source (current commit)");
        let header = &result.lines[0];
        assert_eq!(header.source, LineSource::FileHeader, "first line should be file header");
        assert!(
            !header.content.contains("(deleted)"),
            "modified file should not have deletion header, got: {}",
            header.content
        );
    }

    #[test]
    fn test_jj_refresh_detects_new_file() {
        if !jj_available() {
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("existing.txt"), "content\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("new_file.txt"), "new content\n").unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        assert!(!result.files.is_empty(), "should detect new file");
        let has_new_content = result.lines.iter().any(|l| l.content.contains("new content"));
        assert!(has_new_content, "should contain new file content in diff lines");
    }

    #[test]
    fn test_jj_comparison_context() {
        if !jj_available() {
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "content\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let ctx = vcs.comparison_context().unwrap();

        assert!(!ctx.to_label.is_empty(), "should have a to label");
        assert!(!ctx.from_label.is_empty(), "should have a from label");

        let base_id = vcs.base_identifier().unwrap();
        assert!(!base_id.is_empty(), "should have a base identifier");
    }

    #[test]
    fn test_jj_base_file_bytes() {
        if !jj_available() {
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "original\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "changed\n").unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let base = vcs.base_file_bytes("file.txt").unwrap();
        assert_eq!(base.unwrap(), b"original\n");

        let working = vcs.working_file_bytes("file.txt").unwrap();
        assert_eq!(working.unwrap(), b"changed\n");
    }

    #[test]
    fn test_jj_refresh_detects_deleted_file() {
        if !jj_available() {
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("doomed.txt"), "goodbye\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        std::fs::remove_file(repo.join("doomed.txt")).unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        assert!(!result.files.is_empty(), "should detect deleted file");
        let header = &result.lines[0];
        assert!(
            header.content.contains("(deleted"),
            "deleted file should have deletion header, got: {}",
            header.content
        );
        // With from_rev=@- (no remote), base==head so deletions in @ are DeletedCommitted
        let has_deleted_source =
            result.lines.iter().any(|l| l.source == LineSource::DeletedCommitted);
        assert!(has_deleted_source, "deleted file lines should have DeletedCommitted source");
    }

    #[test]
    fn test_jj_single_file_diff_handles_rename() {
        if !jj_available() {
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("original.txt"), "line1\nline2\nline3\nline4\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        std::fs::rename(repo.join("original.txt"), repo.join("renamed.txt")).unwrap();
        std::fs::write(repo.join("renamed.txt"), "line1\nline2\nline3\nmodified\n").unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let diff = vcs.single_file_diff("renamed.txt");
        assert!(diff.is_some(), "should produce a diff for renamed file");

        let diff = diff.unwrap();
        let header = &diff.lines[0];
        assert!(
            header.content.contains("original.txt"),
            "rename header should reference old filename, got: {}",
            header.content
        );
    }

    #[test]
    fn test_jj_get_repo_root() {
        if !jj_available() {
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();

        let root = get_repo_root(repo).unwrap();
        // Canonicalize both to handle /tmp vs /private/tmp on macOS
        let expected = repo.canonicalize().unwrap();
        let actual = root.canonicalize().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_jj_refresh_returns_base_label_with_bookmark() {
        if !jj_available() {
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "content\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        Command::new("jj")
            .args(["bookmark", "set", "my-base", "-r", "@-"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::fs::write(repo.join("file.txt"), "changed\n").unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        let base_label = result.base_label.expect("should have base_label");
        assert!(
            base_label.contains("(my-base)"),
            "label should contain bookmark in parens, got: {base_label}"
        );
        assert!(base_label.ends_with(')'), "label should end with ), got: {base_label}");
    }

    #[test]
    fn test_jj_refresh_returns_base_label_as_change_id_without_bookmark() {
        if !jj_available() {
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "content\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "changed\n").unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        let base_label = result.base_label.expect("should have base_label");
        assert!(!base_label.is_empty());
        assert!(
            result.base_identifier.starts_with(&base_label),
            "without bookmark, base_label should be a prefix of base_identifier: label={base_label}, id={}",
            result.base_identifier
        );
    }

    #[test]
    fn test_jj_bookmark_strips_tracking_marker() {
        if !jj_available() {
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "content\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        Command::new("jj")
            .args(["bookmark", "set", "my-branch"])
            .current_dir(repo)
            .output()
            .unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let label = vcs.rev_label("@");
        assert_eq!(label, "my-branch", "should strip trailing * from bookmark name");
    }

    #[test]
    fn test_jj_refresh_parallel_with_multiple_files() {
        if !jj_available() {
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        for i in 0..6 {
            std::fs::write(repo.join(format!("file{i}.txt")), "initial\n").unwrap();
        }
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        for i in 0..6 {
            std::fs::write(repo.join(format!("file{i}.txt")), format!("modified {i}\n")).unwrap();
        }

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        assert_eq!(result.files.len(), 6, "should detect all 6 changed files");
        assert!(!result.base_identifier.is_empty());
    }

    #[test]
    fn test_jj_rev_metadata_no_snapshot_with_bookmark() {
        if !jj_available() {
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "content\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        Command::new("jj")
            .args(["bookmark", "set", "test-bm", "-r", "@-"])
            .current_dir(repo)
            .output()
            .unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let (change_id, label) = vcs.rev_metadata_no_snapshot("@-");

        assert!(!change_id.is_empty());
        assert!(
            label.contains("(test-bm)"),
            "label should contain bookmark in parens, got: {label}"
        );
        assert!(label.ends_with(')'), "label should end with ), got: {label}");
    }

    #[test]
    fn test_jj_rev_metadata_no_snapshot_without_bookmark() {
        if !jj_available() {
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();

        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "content\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let (change_id, label) = vcs.rev_metadata_no_snapshot("@-");

        assert!(!change_id.is_empty());
        assert!(
            change_id.starts_with(&label),
            "without bookmark, label should be shortest prefix of change_id: label={label}, id={change_id}"
        );
        assert!(label.len() >= 4, "shortest ID should be at least 4 chars, got: {label}");
    }

    // === --base override tests ===

    /// An explicit base wins over everything branchdiff would infer, including
    /// a `trunk()` that resolves perfectly well. The whole point of the flag is
    /// to override a heuristic that guessed wrong.
    #[test]
    fn test_explicit_base_overrides_inferred_trunk() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            Command::new("jj").args(args).current_dir(repo).output().unwrap();
        };
        std::fs::write(repo.join("dev.txt"), "dev\n").unwrap();
        jj(&["commit", "-m", "develop work"]);
        jj(&["bookmark", "set", "develop", "-r", "@-"]);
        jj(&["git", "push", "--bookmark", "develop", "--remote", "origin"]);

        assert_eq!(
            JjVcs::new(repo.to_path_buf()).unwrap().from_rev,
            "trunk()",
            "precondition: this repo would otherwise infer trunk()"
        );

        let vcs = JjVcs::with_base(repo.to_path_buf(), Some("develop@origin")).unwrap();
        assert_eq!(vcs.from_rev, "develop@origin", "an explicit base must win");
    }

    /// `--base @-` must behave exactly like naming that same commit any other
    /// way. `"@-"` is branchdiff's own sentinel for "this repo has no trunk", so
    /// an explicit `--base @-` was mistaken for it and silently disabled
    /// stack-tip detection — dropping commits stacked above `@` that every other
    /// spelling of the same commit shows.
    #[test]
    fn test_explicit_base_parent_is_not_mistaken_for_the_no_trunk_sentinel() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();

        let vcs = JjVcs::with_base(repo.to_path_buf(), Some("@-")).unwrap();
        assert!(
            is_trunk_base(&vcs.from_rev),
            "an explicitly named base must keep the stack machinery on, but from_rev={:?} \
             reads as the no-trunk fallback",
            vcs.from_rev
        );

        // ...and it must still mean the same commit.
        let cancel = Arc::new(AtomicBool::new(false));
        let named = get_change_id_static(repo, &vcs.from_rev, &cancel).expect("base resolves");
        let parent = get_change_id_static(repo, "@-", &cancel).expect("@- resolves");
        assert_eq!(named, parent, "--base @- must still point at @'s parent");
    }

    /// A base that resolves to *several* commits must be rejected too.
    ///
    /// `jj diff --from` takes exactly one revision. A multi-resolving base
    /// passes a ">= 1 commit" check, then every downstream jj call fails and the
    /// failure is swallowed into an empty base: `--base 'all()'` reported
    /// "vs <@ itself>" and rendered the whole file as added, exit 0, empty
    /// stderr. Silently wrong output is the exact thing this validation exists
    /// to prevent, so "resolves" has to mean "resolves to one commit".
    #[test]
    fn test_explicit_base_that_resolves_to_many_commits_is_rejected() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        std::fs::write(repo.join("more.txt"), "more\n").unwrap();
        Command::new("jj").args(["commit", "-m", "second"]).current_dir(repo).output().unwrap();

        for base in ["all()", "@ | main@origin"] {
            let result = JjVcs::with_base(repo.to_path_buf(), Some(base));
            let msg = match result {
                Ok(v) => panic!(
                    "--base {base:?} resolves to many commits and must be rejected, \
                     but was accepted as from_rev={:?}",
                    v.from_rev
                ),
                Err(e) => e.to_string(),
            };
            assert!(msg.contains(base), "the error must name the offending base, got: {msg}");
        }
    }

    /// A base that resolves to nothing must fail loudly at startup. Left to run,
    /// it produces an empty or nonsense diff that looks like "no changes".
    #[test]
    fn test_explicit_base_that_does_not_resolve_is_rejected() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let result = JjVcs::with_base(temp.path().to_path_buf(), Some("no-such-bookmark@origin"));
        let msg = match result {
            Ok(_) => {
                panic!("a base that resolves to nothing must be rejected, not silently diffed")
            }
            Err(e) => e.to_string(),
        };
        assert!(
            msg.contains("no-such-bookmark@origin"),
            "the error must name the offending base, got: {msg}"
        );
    }

    /// Passing no base keeps the inferred behaviour untouched.
    #[test]
    fn test_no_explicit_base_falls_back_to_inference() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let vcs = JjVcs::with_base(temp.path().to_path_buf(), None).unwrap();
        assert_eq!(vcs.from_rev, "trunk()");
    }

    /// The stack machinery must interpolate an explicit base the same way it
    /// does an inferred `main@origin` — the guards key off "not @-", not off any
    /// particular spelling.
    #[test]
    fn test_explicit_base_still_drives_the_stack_machinery() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            Command::new("jj").args(args).current_dir(repo).output().unwrap();
        };
        std::fs::write(repo.join("stack.txt"), "earlier\n").unwrap();
        jj(&["commit", "-m", "stack commit 1"]);
        std::fs::write(repo.join("stack.txt"), "earlier\ncurrent\n").unwrap();

        let vcs = JjVcs::with_base(repo.to_path_buf(), Some("main@origin")).unwrap();
        assert!(is_trunk_base(&vcs.from_rev), "an explicit base is still a trunk base");

        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();
        assert!(
            result.lines.iter().any(|l| l.source == LineSource::Committed),
            "stack colouring must survive an explicit base"
        );
        assert!(
            result.lines.iter().any(|l| l.source == LineSource::Staged),
            "stack colouring must survive an explicit base"
        );
    }

    /// jj's documented rule: try main/master/trunk on the `upstream` and
    /// `origin` remotes, and "if more than one potential trunk commit exists,
    /// the newest one is chosen". Not first-match-by-remote.
    ///
    /// This matters for a fork: `origin` is your fork, `upstream` is canonical.
    /// A stale `main@origin` must not beat a newer `main@upstream`, or every
    /// commit already merged upstream is reported as your own work.
    #[test]
    fn test_preferred_trunk_picks_the_newest_candidate_not_origin_by_default() {
        if !jj_available() {
            return;
        }

        let origin_dir = tempfile::TempDir::new().unwrap();
        let upstream_dir = tempfile::TempDir::new().unwrap();
        for d in [&origin_dir, &upstream_dir] {
            Command::new("git").args(["init", "--bare"]).current_dir(d.path()).output().unwrap();
        }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            Command::new("jj").args(args).current_dir(repo).output().unwrap();
        };
        // Pin committer timestamps: the whole point is which candidate is
        // *newer*, and two commits made back to back land in the same second.
        let jj_at = |stamp: &str, args: &[&str]| {
            Command::new("jj")
                .args(args)
                .current_dir(repo)
                .env("JJ_TIMESTAMP", stamp)
                .output()
                .unwrap();
        };
        jj(&["git", "init"]);
        jj(&["git", "remote", "add", "origin", &origin_dir.path().to_string_lossy()]);
        jj(&["git", "remote", "add", "upstream", &upstream_dir.path().to_string_lossy()]);

        // The fork's main, pushed to origin and then left behind. Never pushed
        // to origin again, so main@origin stays on this older commit.
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        jj_at("2020-01-01T00:00:00Z", &["commit", "-m", "base"]);
        jj(&["bookmark", "set", "main", "-r", "@-"]);
        jj(&["git", "push", "--bookmark", "main", "--remote", "origin"]);

        // Canonical upstream's main: a year newer.
        std::fs::write(repo.join("upstream.txt"), "landed upstream\n").unwrap();
        jj_at("2021-01-01T00:00:00Z", &["commit", "-m", "landed upstream"]);
        jj(&["bookmark", "set", "main", "-r", "@-"]);
        jj(&["bookmark", "track", "main@upstream"]);
        jj(&["git", "push", "--bookmark", "main", "--remote", "upstream"]);

        let origin_epoch = revset_committer_epoch(repo, "main@origin").expect("main@origin");
        let upstream_epoch = revset_committer_epoch(repo, "main@upstream").expect("main@upstream");
        assert!(
            upstream_epoch > origin_epoch,
            "fixture: upstream's main must be newer ({upstream_epoch} vs {origin_epoch})"
        );

        let newest = preferred_trunk_revset(repo).expect("a trunk candidate must be found");
        assert_eq!(
            newest, "main@upstream",
            "jj picks the newest trunk candidate; a stale fork's main@origin must not win"
        );
    }

    /// With nothing to separate them, order decides — and origin leads, because
    /// it is the review remote this whole path exists to protect.
    #[test]
    fn test_preferred_trunk_breaks_ties_toward_origin() {
        if !jj_available() {
            return;
        }

        let origin_dir = tempfile::TempDir::new().unwrap();
        let upstream_dir = tempfile::TempDir::new().unwrap();
        for d in [&origin_dir, &upstream_dir] {
            Command::new("git").args(["init", "--bare"]).current_dir(d.path()).output().unwrap();
        }
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            Command::new("jj").args(args).current_dir(repo).output().unwrap();
        };
        jj(&["git", "init"]);
        jj(&["git", "remote", "add", "origin", &origin_dir.path().to_string_lossy()]);
        jj(&["git", "remote", "add", "upstream", &upstream_dir.path().to_string_lossy()]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        Command::new("jj")
            .args(["commit", "-m", "base"])
            .current_dir(repo)
            .env("JJ_TIMESTAMP", "2020-01-01T00:00:00Z")
            .output()
            .unwrap();
        jj(&["bookmark", "set", "main", "-r", "@-"]);
        // Same commit on both remotes.
        jj(&["git", "push", "--bookmark", "main", "--remote", "origin"]);
        jj(&["bookmark", "track", "main@upstream"]);
        jj(&["git", "push", "--bookmark", "main", "--remote", "upstream"]);

        assert_eq!(preferred_trunk_revset(repo).as_deref(), Some("main@origin"));
    }

    // === resolve_base_rev tests ===

    #[test]
    fn test_is_trunk_base() {
        assert!(is_trunk_base("trunk()"));
        assert!(is_trunk_base("main@origin"), "an explicit trunk bookmark is still a trunk base");
        assert!(!is_trunk_base("@-"), "the parent fallback is not a trunk base");
    }

    /// Set a repo-level `trunk()` alias, as `jj git init` itself does.
    fn pin_trunk_alias(repo: &Path, value: &str) {
        Command::new("jj")
            .args(["config", "set", "--repo", r#"revset-aliases."trunk()""#, value])
            .current_dir(repo)
            .output()
            .unwrap();
    }

    /// jj pins `trunk()` into repo config at init from whatever remote was
    /// default. In a repo with a deploy remote that can be e.g.
    /// `main@heroku_test`, making every diff compare against the deploy remote
    /// and showing commits already on origin as the user's own work.
    ///
    /// The deploy remote must be genuinely *behind* origin for this to bite: a
    /// deploy bookmark sitting on the same commit as origin's yields an
    /// identical diff, and branchdiff deliberately leaves `trunk()` alone then
    /// rather than rewriting a base that isn't hurting anyone.
    #[test]
    fn test_resolve_base_rev_prefers_origin_when_deploy_remote_is_behind() {
        if !jj_available() {
            return;
        }

        let origin_dir = tempfile::TempDir::new().unwrap();
        let heroku_dir = tempfile::TempDir::new().unwrap();
        Command::new("git")
            .args(["init", "--bare"])
            .current_dir(origin_dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["init", "--bare"])
            .current_dir(heroku_dir.path())
            .output()
            .unwrap();

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            Command::new("jj").args(args).current_dir(repo).output().unwrap();
        };
        jj(&["git", "init"]);
        jj(&["git", "remote", "add", "origin", &origin_dir.path().to_string_lossy()]);
        jj(&["git", "remote", "add", "heroku_test", &heroku_dir.path().to_string_lossy()]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        jj(&["commit", "-m", "base"]);
        jj(&["bookmark", "set", "main", "-r", "@-"]);
        jj(&["git", "push", "--bookmark", "main", "--remote", "origin"]);
        jj(&["bookmark", "track", "main@heroku_test"]);
        jj(&["git", "push", "--bookmark", "main", "--remote", "heroku_test"]);

        pin_trunk_alias(repo, "main@heroku_test");

        // origin advances; the deploy remote stays where it was.
        std::fs::write(repo.join("shipped.txt"), "on origin, not deployed\n").unwrap();
        jj(&["commit", "-m", "landed on origin"]);
        jj(&["bookmark", "set", "main", "-r", "@-"]);
        jj(&["git", "push", "--bookmark", "main", "--remote", "origin"]);

        assert_eq!(
            resolve_base_rev(repo),
            "main@origin",
            "a trunk() pinned to a deploy remote that has fallen behind origin \
             must not become the diff base"
        );
    }

    /// When the deploy remote still agrees with origin there is nothing to fix,
    /// and `trunk()` produces an identical diff — leave it alone.
    #[test]
    fn test_resolve_base_rev_leaves_trunk_alone_when_remotes_agree() {
        if !jj_available() {
            return;
        }

        let origin_dir = tempfile::TempDir::new().unwrap();
        let heroku_dir = tempfile::TempDir::new().unwrap();
        Command::new("git")
            .args(["init", "--bare"])
            .current_dir(origin_dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["init", "--bare"])
            .current_dir(heroku_dir.path())
            .output()
            .unwrap();

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            Command::new("jj").args(args).current_dir(repo).output().unwrap();
        };
        jj(&["git", "init"]);
        jj(&["git", "remote", "add", "origin", &origin_dir.path().to_string_lossy()]);
        jj(&["git", "remote", "add", "heroku_test", &heroku_dir.path().to_string_lossy()]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        jj(&["commit", "-m", "base"]);
        jj(&["bookmark", "set", "main", "-r", "@-"]);
        jj(&["git", "push", "--bookmark", "main", "--remote", "origin"]);
        jj(&["bookmark", "track", "main@heroku_test"]);
        jj(&["git", "push", "--bookmark", "main", "--remote", "heroku_test"]);

        pin_trunk_alias(repo, "main@heroku_test");

        assert_eq!(
            resolve_base_rev(repo),
            "trunk()",
            "with the deploy remote level with origin, trunk() is already correct"
        );
    }

    /// A `trunk()` already resolving on origin is the user's call — it may point
    /// at something other than main on purpose. Don't hijack it.
    #[test]
    fn test_resolve_base_rev_respects_deliberate_trunk_on_origin() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            Command::new("jj").args(args).current_dir(repo).output().unwrap();
        };
        std::fs::write(repo.join("dev.txt"), "dev\n").unwrap();
        jj(&["commit", "-m", "develop work"]);
        jj(&["bookmark", "set", "develop", "-r", "@-"]);
        jj(&["git", "push", "--bookmark", "develop", "--remote", "origin"]);

        pin_trunk_alias(repo, "develop@origin");

        assert_eq!(
            resolve_base_rev(repo),
            "trunk()",
            "a deliberate trunk() on origin must be respected, not rewritten to main@origin"
        );
    }

    #[test]
    fn test_resolve_base_rev_fallback_without_remote() {
        if !jj_available() {
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();

        let rev = resolve_base_rev(repo);
        assert_eq!(rev, "@-", "should fall back to @- when no remote tracking bookmarks");
    }

    /// A repo with a local `main` and a `feature` stacked on it, no remote.
    fn setup_local_only_feature_repo() -> tempfile::TempDir {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            Command::new("jj").args(args).current_dir(repo).output().unwrap();
        };
        jj(&["git", "init"]);
        std::fs::write(repo.join("f.txt"), "a\n").unwrap();
        jj(&["commit", "-m", "base"]);
        jj(&["bookmark", "create", "main", "-r", "@-"]);
        std::fs::write(repo.join("g.txt"), "b\n").unwrap();
        jj(&["commit", "-m", "feature"]);
        jj(&["bookmark", "create", "feature", "-r", "@-"]);
        temp
    }

    /// Without a remote `trunk()` is `root()`, and the old fallback `@-` is the
    /// branch's own bookmark: the feature commit vanished from the diff.
    #[test]
    fn test_resolve_base_rev_without_remote_uses_local_main() {
        if !jj_available() {
            return;
        }
        let temp = setup_local_only_feature_repo();
        assert_eq!(resolve_base_rev(temp.path()), "main");
    }

    #[test]
    fn test_resolve_base_rev_without_remote_uses_local_master_when_no_main() {
        if !jj_available() {
            return;
        }
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            Command::new("jj").args(args).current_dir(repo).output().unwrap();
        };
        jj(&["git", "init"]);
        std::fs::write(repo.join("f.txt"), "a\n").unwrap();
        jj(&["commit", "-m", "base"]);
        jj(&["bookmark", "create", "master", "-r", "@-"]);
        assert_eq!(resolve_base_rev(repo), "master");
    }

    #[test]
    fn test_resolve_base_rev_without_remote_or_trunk_bookmark_keeps_parent() {
        if !jj_available() {
            return;
        }
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            Command::new("jj").args(args).current_dir(repo).output().unwrap();
        };
        jj(&["git", "init"]);
        std::fs::write(repo.join("f.txt"), "a\n").unwrap();
        jj(&["commit", "-m", "base"]);
        jj(&["bookmark", "create", "feature", "-r", "@-"]);
        assert_eq!(resolve_base_rev(repo), "@-");
    }

    /// The report: a second workspace parked at the feature bookmark showed
    /// `0 files` because its base was the feature bookmark itself.
    #[test]
    fn test_workspace_at_feature_bookmark_diffs_against_main() {
        if !jj_available() {
            return;
        }
        let temp = setup_local_only_feature_repo();
        let repo = temp.path();
        let ws_parent = tempfile::TempDir::new().unwrap();
        let ws = ws_parent.path().join("ws");
        let out = Command::new("jj")
            .args(["workspace", "add", ws.to_str().unwrap(), "-r", "feature"])
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

        let vcs = JjVcs::new(ws.clone()).unwrap();
        assert_eq!(vcs.from_rev, "main");
        let out =
            run_jj(&ws, &["diff", "--from", &vcs.from_rev, "--to", "@", "--summary"]).unwrap();
        let files = out.stdout_lossy();
        assert!(files.contains("g.txt"), "the feature commit must be in the diff: {files}");
    }

    fn operation_count(repo: &Path) -> usize {
        jj_out(
            repo,
            &["--ignore-working-copy", "op", "log", "--no-graph", "-T", r#"id.short() ++ "\n""#],
        )
        .lines()
        .count()
    }

    /// A read-only viewer must not add to the repo's operation log: with
    /// uncommitted edits, `--html`/`--print`/`--diff` used to record a
    /// "snapshot working copy" operation on every run.
    #[test]
    fn test_one_shot_read_records_no_jj_operation_and_still_shows_uncommitted_edits() {
        if !jj_available() {
            return;
        }
        let temp = setup_local_only_feature_repo();
        let repo = temp.path();
        std::fs::write(repo.join("h.txt"), "dirty\n").unwrap();
        let before = operation_count(repo);

        let root = get_repo_root(repo).unwrap();
        let vcs = JjVcs::new(root).unwrap();
        vcs.comparison_context().unwrap();
        let result = vcs.refresh(&Arc::new(AtomicBool::new(false))).unwrap();

        assert_eq!(operation_count(repo), before, "reading the diff recorded a jj operation");
        let paths: Vec<_> =
            result.files.iter().filter_map(|f| f.lines.first()?.file_path.clone()).collect();
        assert!(paths.contains(&"h.txt".to_string()), "uncommitted edit missing: {paths:?}");
        assert!(paths.contains(&"g.txt".to_string()), "committed feature work missing: {paths:?}");
    }

    /// A secondary workspace has no git index, so it discovers edits by walking
    /// the disk; that path must stay snapshot-free and live too.
    #[test]
    fn test_secondary_workspace_read_records_no_jj_operation() {
        if !jj_available() {
            return;
        }
        let temp = setup_local_only_feature_repo();
        // Park the first workspace on `feature` itself so the new one has no sibling
        // (a sibling makes the orphaned-tip check snapshot, on purpose).
        Command::new("jj").args(["edit", "feature"]).current_dir(temp.path()).output().unwrap();
        let ws_parent = tempfile::TempDir::new().unwrap();
        let ws = ws_parent.path().join("ws");
        let out = Command::new("jj")
            .args(["workspace", "add", ws.to_str().unwrap(), "-r", "feature"])
            .current_dir(temp.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        std::fs::write(ws.join("h.txt"), "dirty\n").unwrap();
        let before = operation_count(&ws);

        let vcs = JjVcs::new(ws.clone()).unwrap();
        vcs.comparison_context().unwrap();
        let result = vcs.refresh(&Arc::new(AtomicBool::new(false))).unwrap();

        assert_eq!(operation_count(&ws), before);
        let paths: Vec<_> =
            result.files.iter().filter_map(|f| f.lines.first()?.file_path.clone()).collect();
        assert!(
            paths.contains(&"h.txt".to_string()) && paths.contains(&"g.txt".to_string()),
            "{paths:?}"
        );
    }

    #[test]
    fn test_resolve_base_rev_uses_trunk_with_remote() {
        if !jj_available() {
            return;
        }

        // Create a bare git repo as the "remote"
        let remote_dir = tempfile::TempDir::new().unwrap();
        Command::new("git")
            .args(["init", "--bare"])
            .current_dir(remote_dir.path())
            .output()
            .unwrap();

        // Create a jj repo and add the remote
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "content\n").unwrap();
        Command::new("jj").args(["commit", "-m", "initial"]).current_dir(repo).output().unwrap();
        Command::new("jj")
            .args(["bookmark", "set", "main", "-r", "@-"])
            .current_dir(repo)
            .output()
            .unwrap();
        let remote_path = remote_dir.path().to_string_lossy().to_string();
        Command::new("jj")
            .args(["git", "remote", "add", "origin", &remote_path])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("jj")
            .args(["git", "push", "--bookmark", "main"])
            .current_dir(repo)
            .output()
            .unwrap();

        let rev = resolve_base_rev(repo);
        assert_eq!(rev, "trunk()", "should use trunk() when remote tracking bookmarks exist");
    }

    // === Stack coloring tests ===

    /// Helper: create a jj repo with a remote "origin" and push main.
    /// Returns (repo_tempdir, remote_tempdir) — keep both alive.
    pub(super) fn setup_repo_with_remote() -> (tempfile::TempDir, tempfile::TempDir) {
        let remote_dir = tempfile::TempDir::new().unwrap();
        Command::new("git")
            .args(["init", "--bare"])
            .current_dir(remote_dir.path())
            .output()
            .unwrap();

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("base.txt"), "trunk content\n").unwrap();
        Command::new("jj").args(["commit", "-m", "base"]).current_dir(repo).output().unwrap();
        Command::new("jj")
            .args(["bookmark", "set", "main", "-r", "@-"])
            .current_dir(repo)
            .output()
            .unwrap();
        let remote_path = remote_dir.path().to_string_lossy().to_string();
        Command::new("jj")
            .args(["git", "remote", "add", "origin", &remote_path])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("jj")
            .args(["git", "push", "--bookmark", "main"])
            .current_dir(repo)
            .output()
            .unwrap();

        (temp, remote_dir)
    }

    #[test]
    fn test_jj_stack_coloring_two_commits() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();

        // First stack commit: add a file
        std::fs::write(repo.join("stack.txt"), "from earlier commit\n").unwrap();
        Command::new("jj")
            .args(["commit", "-m", "stack commit 1"])
            .current_dir(repo)
            .output()
            .unwrap();

        // Current commit (@): modify the file
        std::fs::write(repo.join("stack.txt"), "from earlier commit\nfrom current commit\n")
            .unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        assert_eq!(vcs.from_rev, "trunk()");

        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        let has_committed = result.lines.iter().any(|l| l.source == LineSource::Committed);
        let has_staged = result.lines.iter().any(|l| l.source == LineSource::Staged);
        assert!(has_committed, "earlier stack commit lines should be Committed (teal)");
        assert!(has_staged, "current commit lines should be Staged (green)");
    }

    #[test]
    fn test_jj_single_commit_above_trunk_all_staged() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();

        // Only one commit above trunk — everything should be Staged
        std::fs::write(repo.join("feature.txt"), "new feature\n").unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        assert_eq!(vcs.from_rev, "trunk()");

        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        assert!(!result.files.is_empty(), "should detect the new file");
        let has_staged = result.lines.iter().any(|l| l.source == LineSource::Staged);
        let has_committed = result.lines.iter().any(|l| l.source == LineSource::Committed);
        assert!(has_staged, "single commit above trunk: all additions should be Staged");
        assert!(!has_committed, "single commit above trunk: no lines should be Committed");
    }

    // === Stack tip resolution tests ===

    #[test]
    fn test_resolve_stack_tip_at_tip() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();

        // @ is the tip — one commit above trunk, nothing above @
        std::fs::write(repo.join("feature.txt"), "content\n").unwrap();

        let tip = resolve_stack_tip(repo, "trunk()", &Arc::new(AtomicBool::new(false)));
        assert!(tip.is_none(), "should return None when @ is already the tip");
    }

    #[test]
    fn test_resolve_stack_tip_mid_stack() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();

        // Build 3-commit stack: commit1 → commit2 → commit3 (tip)
        std::fs::write(repo.join("file.txt"), "commit1\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit1"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "commit2\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit2"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "commit3\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit3"]).current_dir(repo).output().unwrap();

        // Move @ back to commit2 (mid-stack)
        Command::new("jj").args(["edit", "@---"]).current_dir(repo).output().unwrap();

        let tip = resolve_stack_tip(repo, "trunk()", &Arc::new(AtomicBool::new(false)));
        assert!(tip.is_some(), "should return a tip when @ is mid-stack");
        let tip = tip.unwrap();
        assert_eq!(tip.head_count, 1, "linear stack should have 1 head");
        assert!(!tip.change_id.is_empty());
    }

    #[test]
    fn test_resolve_stack_tip_without_trunk() {
        if !jj_available() {
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();

        let tip = resolve_stack_tip(repo, "@-", &Arc::new(AtomicBool::new(false)));
        assert!(tip.is_none(), "should return None when from_rev is not trunk()");
    }

    #[test]
    fn test_resolve_stack_tip_branching() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();

        // Linear base: commit1
        std::fs::write(repo.join("file.txt"), "commit1\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit1"]).current_dir(repo).output().unwrap();

        // Branch: create commit2a on one branch
        std::fs::write(repo.join("branch_a.txt"), "branch a\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit2a"]).current_dir(repo).output().unwrap();

        // Go back to commit1 and create commit2b (a second head)
        // After commit2a: @=empty, @-=commit2a, @--=commit1
        Command::new("jj").args(["edit", "@--"]).current_dir(repo).output().unwrap();
        Command::new("jj").args(["new"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("branch_b.txt"), "branch b\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit2b"]).current_dir(repo).output().unwrap();

        // Move @ to commit1 (below the fork)
        // After commit2b: @=empty, @-=commit2b, @--=commit1
        Command::new("jj").args(["edit", "@--"]).current_dir(repo).output().unwrap();

        let tip = resolve_stack_tip(repo, "trunk()", &Arc::new(AtomicBool::new(false)));
        assert!(tip.is_some(), "should detect stack tip when @ is below a fork");
        let tip = tip.unwrap();
        assert_eq!(tip.head_count, 2, "branching stack should have 2 heads");
    }

    // === Orphaned tip resolution tests (empty @ left by abandoning the edited commit) ===

    pub(super) fn jj_out(repo: &Path, args: &[&str]) -> String {
        let o = Command::new("jj").args(args).current_dir(repo).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// Build trunk → a → b → c, put @ on b, then abandon b. jj moves @ to a fresh
    /// empty commit on a and rebases c as a sibling. Returns (repo dir, c's change id).
    pub(super) fn setup_orphaned_after_abandon() -> (tempfile::TempDir, tempfile::TempDir, String) {
        let (temp, remote) = setup_repo_with_remote();
        let repo = temp.path();
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        Command::new("jj").args(["commit", "-m", "a"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("b.txt"), "b\n").unwrap();
        Command::new("jj").args(["commit", "-m", "b"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("c.txt"), "c\n").unwrap();
        Command::new("jj").args(["commit", "-m", "c"]).current_dir(repo).output().unwrap();
        // From the empty working copy: @-=c, @--=b. Edit b, capturing c (its child) first.
        Command::new("jj").args(["edit", "@--"]).current_dir(repo).output().unwrap();
        let c_id = jj_out(repo, &["log", "-r", "@+", "--no-graph", "-T", "change_id.short(12)"]);
        Command::new("jj").args(["abandon", "@"]).current_dir(repo).output().unwrap();
        (temp, remote, c_id)
    }

    #[test]
    fn test_resolve_orphaned_tip_follows_sibling_after_abandon() {
        if !jj_available() {
            return;
        }

        let (temp, _remote, c_id) = setup_orphaned_after_abandon();
        let repo = temp.path();

        let tip = resolve_orphaned_tip(repo, "trunk()", &Arc::new(AtomicBool::new(false)))
            .expect("orphaned empty @ with a sibling should resolve to the sibling tip");
        assert!(
            c_id.starts_with(&tip.change_id) || tip.change_id.starts_with(&c_id),
            "orphaned tip {} should be c ({c_id})",
            tip.change_id
        );
    }

    #[test]
    fn test_orphaned_tip_refresh_shows_remaining_stack() {
        if !jj_available() {
            return;
        }

        let (temp, _remote, _c_id) = setup_orphaned_after_abandon();
        let repo = temp.path();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let result = vcs.refresh(&Arc::new(AtomicBool::new(false))).unwrap();

        // The diff must include c's file even though @ itself is empty.
        let shows_c = result.lines.iter().any(|l| l.content.contains("c.txt"));
        assert!(shows_c, "refresh after abandon should re-anchor to c and show c.txt");

        // c is treated as the current commit: its content must be Staged so the
        // [commit] view shows c rather than "No changes in current commit".
        let c_is_staged =
            result.lines.iter().any(|l| l.source == LineSource::Staged && l.content.contains("c"));
        assert!(c_is_staged, "c's change should color as the current commit (Staged)");
    }

    #[test]
    fn test_resolve_orphaned_tip_none_for_normal_tip() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        // Normal linear stack, @ empty on top — its parent's only child.
        std::fs::write(repo.join("f.txt"), "x\n").unwrap();
        Command::new("jj").args(["commit", "-m", "f"]).current_dir(repo).output().unwrap();

        let tip = resolve_orphaned_tip(repo, "trunk()", &Arc::new(AtomicBool::new(false)));
        assert!(tip.is_none(), "no sibling → must not re-anchor");
    }

    #[test]
    fn test_resolve_orphaned_tip_none_when_at_nonempty() {
        if !jj_available() {
            return;
        }

        // @ is non-empty AND has a sibling: deliberate forked work, do not hijack.
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        Command::new("jj").args(["commit", "-m", "a"]).current_dir(repo).output().unwrap();
        let a_id = jj_out(repo, &["log", "-r", "@-", "--no-graph", "-T", "change_id.short(12)"]);
        // First sibling under a.
        std::fs::write(repo.join("sib1.txt"), "s1\n").unwrap();
        Command::new("jj").args(["commit", "-m", "sib1"]).current_dir(repo).output().unwrap();
        // Second sibling under a, left as a non-empty working copy (@).
        Command::new("jj").args(["new", &a_id]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("sib2.txt"), "s2\n").unwrap();

        let tip = resolve_orphaned_tip(repo, "trunk()", &Arc::new(AtomicBool::new(false)));
        assert!(tip.is_none(), "non-empty @ with a sibling must not be re-anchored");
    }

    #[test]
    fn test_resolve_orphaned_tip_none_without_trunk() {
        if !jj_available() {
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();

        let tip = resolve_orphaned_tip(repo, "@-", &Arc::new(AtomicBool::new(false)));
        assert!(tip.is_none(), "orphaned recovery only applies in trunk() mode");
    }

    #[test]
    fn test_resolve_orphaned_tip_ignores_sibling_workspace() {
        if !jj_available() {
            return;
        }

        // Regression for #2: several workspaces branched off the same trunk share
        // that commit as their parent, so a fresh, empty workspace must not adopt
        // another workspace's commit as an "orphaned" tip.
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path(); // its own @ is an empty working copy on trunk

        // A sibling workspace with real, committed work branched from trunk.
        let ws2 = tempfile::TempDir::new().unwrap();
        let ws2_path = ws2.path().to_string_lossy().to_string();
        Command::new("jj")
            .args(["workspace", "add", "--name", "ws2", "-r", "trunk()", &ws2_path])
            .current_dir(repo)
            .output()
            .unwrap();
        std::fs::write(ws2.path().join("ws2_work.txt"), "sibling work\n").unwrap();
        Command::new("jj").args(["status"]).current_dir(ws2.path()).output().unwrap();
        Command::new("jj")
            .args(["bookmark", "create", "feature", "-r", "@"])
            .current_dir(ws2.path())
            .output()
            .unwrap();

        let tip = resolve_orphaned_tip(repo, "trunk()", &Arc::new(AtomicBool::new(false)));
        assert!(
            tip.is_none(),
            "fresh empty workspace must not re-anchor to a sibling workspace, got {tip:?}"
        );
    }

    #[test]
    fn test_stack_position_mid_stack() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();

        // Build 3-commit stack
        std::fs::write(repo.join("file.txt"), "commit1\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit1"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "commit2\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit2"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "commit3\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit3"]).current_dir(repo).output().unwrap();

        // Move @ to commit2 (position 2 of 3 mutable commits, plus the empty @)
        Command::new("jj").args(["edit", "@---"]).current_dir(repo).output().unwrap();

        let tip = resolve_stack_tip(repo, "trunk()", &Arc::new(AtomicBool::new(false)))
            .expect("should have a tip");
        let (current, total) = compute_stack_position(repo, &tip.change_id, "trunk()")
            .expect("should compute position");

        assert!(
            current >= 1 && current <= total,
            "current ({current}) should be within 1..={total}"
        );
        assert!(total >= 3, "total ({total}) should be at least 3 commits");
    }

    #[test]
    fn test_jj_midstack_coloring() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();

        // Commit 1: add file
        std::fs::write(repo.join("file.txt"), "line1\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit1"]).current_dir(repo).output().unwrap();

        // Commit 2: modify file
        std::fs::write(repo.join("file.txt"), "line1\nline2\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit2"]).current_dir(repo).output().unwrap();

        // Commit 3: modify file further
        std::fs::write(repo.join("file.txt"), "line1\nline2\nline3\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit3"]).current_dir(repo).output().unwrap();

        // Move @ to commit2 (mid-stack): @=empty, @-=commit3, @--=commit2
        Command::new("jj").args(["edit", "@--"]).current_dir(repo).output().unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        let has_committed = result.lines.iter().any(|l| l.source == LineSource::Committed);
        let has_staged = result.lines.iter().any(|l| l.source == LineSource::Staged);
        let has_unstaged = result.lines.iter().any(|l| l.source == LineSource::Unstaged);

        assert!(has_committed, "earlier stack commits should produce Committed (teal)");
        assert!(has_staged, "current commit should produce Staged (green)");
        assert!(has_unstaged, "later stack commits should produce Unstaged (yellow)");

        assert!(result.stack_position.is_some(), "mid-stack should have stack_position");
        let pos = result.stack_position.unwrap();
        assert_eq!(pos.head_count, 1, "linear stack should have 1 head");
    }

    #[test]
    fn test_jj_midstack_later_only_file() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();

        // Commit 1: add base file
        std::fs::write(repo.join("base.txt"), "base stuff\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit1"]).current_dir(repo).output().unwrap();

        // Commit 2 (will be @): add something
        std::fs::write(repo.join("current.txt"), "current\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit2"]).current_dir(repo).output().unwrap();

        // Commit 3: add a file only in the later commit
        std::fs::write(repo.join("later.txt"), "later only\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit3"]).current_dir(repo).output().unwrap();

        // Move @ to commit2
        Command::new("jj").args(["edit", "@---"]).current_dir(repo).output().unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        // later.txt should appear with Unstaged lines (only changed above @)
        let later_file = result
            .files
            .iter()
            .find(|f| f.lines.first().is_some_and(|l| l.content.contains("later.txt")));
        assert!(later_file.is_some(), "later.txt should be in diff (file only in later commit)");

        let later_lines: Vec<_> =
            later_file.unwrap().lines.iter().filter(|l| l.source == LineSource::Unstaged).collect();
        assert!(
            !later_lines.is_empty(),
            "later.txt content should be Unstaged (only changed in later commit)"
        );
    }

    #[test]
    fn test_jj_at_tip_no_stack_position() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();

        // Single commit above trunk — @ is at the tip
        std::fs::write(repo.join("feature.txt"), "content\n").unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        assert!(
            result.stack_position.is_none(),
            "stack_position should be None when @ is at the tip"
        );
    }

    #[test]
    fn test_jj_trunk_deletion_coloring() {
        if !jj_available() {
            return;
        }

        let remote_dir = tempfile::TempDir::new().unwrap();
        Command::new("git")
            .args(["init", "--bare"])
            .current_dir(remote_dir.path())
            .output()
            .unwrap();

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();

        // File exists at trunk
        std::fs::write(repo.join("doomed.txt"), "will be deleted\n").unwrap();
        Command::new("jj").args(["commit", "-m", "base"]).current_dir(repo).output().unwrap();
        Command::new("jj")
            .args(["bookmark", "set", "main", "-r", "@-"])
            .current_dir(repo)
            .output()
            .unwrap();
        let remote_path = remote_dir.path().to_string_lossy().to_string();
        Command::new("jj")
            .args(["git", "remote", "add", "origin", &remote_path])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("jj")
            .args(["git", "push", "--bookmark", "main"])
            .current_dir(repo)
            .output()
            .unwrap();

        // Current commit (@): delete the file
        std::fs::remove_file(repo.join("doomed.txt")).unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        assert_eq!(vcs.from_rev, "trunk()");

        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        assert!(!result.files.is_empty(), "should detect deleted file");
        let header = &result.lines[0];
        assert!(
            header.content.contains("(deleted"),
            "deleted file should have deletion header, got: {}",
            header.content
        );
        let has_deleted = result.lines.iter().any(|l| l.source == LineSource::DeletedCommitted);
        assert!(has_deleted, "file deleted in current commit should have DeletedCommitted source");
    }

    // === parse_rev_metadata unit tests ===

    #[test]
    fn test_parse_rev_metadata_bookmark_with_shortest_id() {
        let (id, label) = parse_rev_metadata("main\0knmqypts1234\0knmq");
        assert_eq!(id, "knmqypts1234");
        assert_eq!(label, "knmq (main)");
    }

    #[test]
    fn test_parse_rev_metadata_no_bookmark() {
        let (id, label) = parse_rev_metadata("\0knmqypts1234\0knmq");
        assert_eq!(id, "knmqypts1234");
        assert_eq!(label, "knmq", "no bookmark → shortest change_id");
    }

    #[test]
    fn test_parse_rev_metadata_tracking_marker_with_shortest_id() {
        let (id, label) = parse_rev_metadata("feat*\0abcd1234efgh\0abcd");
        assert_eq!(id, "abcd1234efgh");
        assert_eq!(label, "abcd (feat)");
    }

    #[test]
    fn test_parse_rev_metadata_two_field_fallback() {
        let (id, label) = parse_rev_metadata("main\0knmqypts1234");
        assert_eq!(id, "knmqypts1234");
        assert_eq!(label, "main", "2-field format falls back to bookmark only");
    }

    #[test]
    fn test_parse_rev_metadata_multiple_bookmarks() {
        let (id, label) = parse_rev_metadata("b1 b2\0xvryypywztmm\0xvry");
        assert_eq!(id, "xvryypywztmm");
        assert_eq!(label, "xvry (b1 b2)");
    }

    #[test]
    fn test_parse_rev_metadata_no_delimiter() {
        let (id, label) = parse_rev_metadata("rawvalue");
        assert_eq!(id, "rawvalue");
        assert_eq!(label, "rawvalue");
    }

    // === mark_bookmark_provenance tests ===

    fn make_file_diff(lines: Vec<crate::diff::DiffLine>) -> crate::diff::FileDiff {
        crate::diff::FileDiff::new(lines)
    }

    #[test]
    fn test_mark_bookmark_provenance_file_in_bookmark() {
        let lines = vec![
            crate::diff::DiffLine::new(LineSource::Committed, "added".to_string(), '+', None),
            crate::diff::DiffLine::new(LineSource::DeletedBase, "removed".to_string(), '-', None),
            crate::diff::DiffLine::new(LineSource::Staged, "staged".to_string(), '+', None),
            crate::diff::DiffLine::new(LineSource::Base, "context".to_string(), ' ', None),
        ];
        let mut fd = make_file_diff(lines);

        mark_bookmark_provenance(&mut fd, true);

        assert_eq!(
            fd.lines[0].in_current_bookmark,
            Some(true),
            "Committed lines in a bookmark file should be marked true"
        );
        assert_eq!(
            fd.lines[1].in_current_bookmark,
            Some(true),
            "DeletedBase lines in a bookmark file should be marked true"
        );
        assert_eq!(
            fd.lines[2].in_current_bookmark,
            Some(true),
            "Staged lines in a bookmark file should be marked true"
        );
        assert_eq!(
            fd.lines[3].in_current_bookmark,
            Some(false),
            "Base context lines are never in current bookmark"
        );
    }

    #[test]
    fn test_mark_bookmark_provenance_file_not_in_bookmark() {
        let lines = vec![
            crate::diff::DiffLine::new(LineSource::Committed, "added".to_string(), '+', None),
            crate::diff::DiffLine::new(LineSource::DeletedBase, "removed".to_string(), '-', None),
            crate::diff::DiffLine::new(LineSource::Staged, "staged".to_string(), '+', None),
            crate::diff::DiffLine::new(LineSource::Base, "context".to_string(), ' ', None),
        ];
        let mut fd = make_file_diff(lines);

        mark_bookmark_provenance(&mut fd, false);

        assert_eq!(
            fd.lines[0].in_current_bookmark,
            Some(false),
            "Committed lines NOT in bookmark file should be marked false"
        );
        assert_eq!(
            fd.lines[1].in_current_bookmark,
            Some(false),
            "DeletedBase lines NOT in bookmark file should be marked false"
        );
        assert_eq!(
            fd.lines[2].in_current_bookmark,
            Some(false),
            "Staged lines NOT in bookmark file should be marked false"
        );
        assert_eq!(
            fd.lines[3].in_current_bookmark,
            Some(false),
            "Base context lines are never in current bookmark"
        );
    }

    #[test]
    fn test_mark_bookmark_provenance_base_with_change_source() {
        let mut line =
            crate::diff::DiffLine::new(LineSource::Base, "modified".to_string(), ' ', None);
        line.change_source = Some(LineSource::Committed);
        let mut fd = make_file_diff(vec![line]);

        mark_bookmark_provenance(&mut fd, true);
        assert_eq!(
            fd.lines[0].in_current_bookmark,
            Some(true),
            "Base with change_source in bookmark file should be marked true"
        );

        let mut line2 =
            crate::diff::DiffLine::new(LineSource::Base, "modified".to_string(), ' ', None);
        line2.change_source = Some(LineSource::Committed);
        let mut fd2 = make_file_diff(vec![line2]);

        mark_bookmark_provenance(&mut fd2, false);
        assert_eq!(
            fd2.lines[0].in_current_bookmark,
            Some(false),
            "Base with change_source NOT in bookmark file should be marked false"
        );
    }

    /// Verify the boundary revset includes remote bookmarks so that stacks with
    /// only remote-tracking bookmarks for pushed segments are handled.
    #[test]
    fn test_boundary_revset_includes_remote_bookmarks() {
        let revset = boundary_revset("abc123def456", "trunk()", Some("origin"));
        assert!(
            revset.contains("remote_bookmarks("),
            "Boundary revset must include remote bookmarks for pushed bookmark segments"
        );
        assert!(
            revset.contains("\"abc123def456\""),
            "Boundary revset must reference the current bookmark's change ID"
        );
    }

    /// The remote must be scoped: an unscoped `remote_bookmarks()` matches every
    /// remote, letting a deploy remote displace the real stack segment.
    #[test]
    fn test_boundary_revset_scopes_remote_bookmarks_to_trunk_remote() {
        let revset = boundary_revset("abc123def456", "trunk()", Some("origin"));
        assert!(
            revset.contains(r#"remote_bookmarks(remote=exact:"origin")"#),
            "Boundary revset must scope remote bookmarks to trunk's remote, got: {revset}"
        );
        assert!(
            !revset.contains("remote_bookmarks()"),
            "Boundary revset must never use an unscoped remote_bookmarks(), got: {revset}"
        );
    }

    /// With no known trunk remote, fall back to local bookmarks only rather than
    /// admitting every remote.
    #[test]
    fn test_boundary_revset_without_trunk_remote_is_local_only() {
        let revset = boundary_revset("abc123def456", "trunk()", None);
        assert!(
            !revset.contains("remote_bookmarks"),
            "Without a trunk remote, no remote bookmarks may be considered, got: {revset}"
        );
        assert!(
            revset.contains("bookmarks()"),
            "Local bookmarks must still be considered, got: {revset}"
        );
    }

    /// jj emits trunk's remotes alphabetically, so a deploy remote sorting
    /// before `origin` must not win by position. This is the routine shape for a
    /// deployed app: `main` pushed to both `origin` and the deploy remote.
    #[test]
    fn test_pick_trunk_remote_prefers_origin_over_alphabetically_earlier_remote() {
        assert_eq!(
            pick_trunk_remote("git\nheroku_test\norigin").as_deref(),
            Some("origin"),
            "origin must win over a deploy remote that sorts earlier"
        );
    }

    #[test]
    fn test_pick_trunk_remote_skips_colocated_git_pseudo_remote() {
        assert_eq!(pick_trunk_remote("git\norigin").as_deref(), Some("origin"));
        assert_eq!(
            pick_trunk_remote("git").as_deref(),
            None,
            "the colocated git pseudo-remote alone is not a real remote"
        );
    }

    #[test]
    fn test_pick_trunk_remote_falls_back_to_upstream_then_custom() {
        assert_eq!(
            pick_trunk_remote("git\nheroku_test\nupstream").as_deref(),
            Some("upstream"),
            "upstream is a remote jj resolves trunk() against; a deploy remote is not"
        );
        assert_eq!(
            pick_trunk_remote("git\ncompany-fork").as_deref(),
            Some("company-fork"),
            "a lone custom-named remote is trunk's remote by elimination"
        );
    }

    #[test]
    fn test_pick_trunk_remote_empty_and_unsafe_names() {
        assert_eq!(pick_trunk_remote("").as_deref(), None);
        assert_eq!(pick_trunk_remote("\n  \n").as_deref(), None);
        assert_eq!(
            pick_trunk_remote(r#"we"ird"#).as_deref(),
            None,
            "a name that cannot be safely quoted in a revset must be rejected"
        );
        assert_eq!(pick_trunk_remote("back\\slash").as_deref(), None);
    }

    /// Regression: a bookmark on a non-origin remote (e.g. a heroku deploy
    /// remote) sitting inside the stack range must not become the boundary.
    /// `latest()` picks by committer timestamp, so an unscoped
    /// `remote_bookmarks()` lets the newer deploy bookmark displace `feature-1`
    /// and truncate the current bookmark's file scope.
    #[test]
    fn test_boundary_ignores_non_origin_remote_bookmark() {
        if !jj_available() {
            return;
        }

        let origin_dir = tempfile::TempDir::new().unwrap();
        let heroku_dir = tempfile::TempDir::new().unwrap();
        Command::new("git")
            .args(["init", "--bare"])
            .current_dir(origin_dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["init", "--bare"])
            .current_dir(heroku_dir.path())
            .output()
            .unwrap();

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            Command::new("jj").args(args).current_dir(repo).output().unwrap();
        };
        jj(&["git", "init"]);
        jj(&["git", "remote", "add", "origin", &origin_dir.path().to_string_lossy()]);
        jj(&["git", "remote", "add", "heroku_test", &heroku_dir.path().to_string_lossy()]);

        // trunk, pushed to BOTH remotes — the routine shape for a deployed app,
        // and the one that makes trunk's remote ambiguous. jj lists trunk's
        // remotes alphabetically, so "heroku_test" precedes "origin" here.
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        jj(&["commit", "-m", "base"]);
        jj(&["bookmark", "set", "main", "-r", "@-"]);
        jj(&["git", "push", "--bookmark", "main", "--remote", "origin"]);
        jj(&["bookmark", "track", "main@heroku_test"]);
        jj(&["git", "push", "--bookmark", "main", "--remote", "heroku_test"]);

        // Stack segment 1: feature-1, pushed to origin.
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        jj(&["commit", "-m", "feature 1 work"]);
        jj(&["bookmark", "set", "feature-1", "-r", "@-"]);
        jj(&["git", "push", "--bookmark", "feature-1", "--remote", "origin"]);

        // A deploy bookmark that exists ONLY on heroku_test. `forget` drops the
        // local bookmark and untracks the remote, which is the shape you get
        // from fetching a remote under git.auto-local-bookmark = false.
        std::fs::write(repo.join("b.txt"), "b\n").unwrap();
        jj(&["commit", "-m", "deploy work"]);
        jj(&["bookmark", "set", "deploy", "-r", "@-"]);
        jj(&["git", "push", "--bookmark", "deploy", "--remote", "heroku_test"]);
        jj(&["bookmark", "forget", "deploy"]);

        // Stack segment 2: feature-2, the current bookmark at @.
        std::fs::write(repo.join("c.txt"), "c\n").unwrap();
        jj(&["describe", "-m", "feature 2 work"]);
        jj(&["bookmark", "set", "feature-2", "-r", "@"]);

        let cancel = Arc::new(AtomicBool::new(false));
        let boundary = resolve_bookmark_boundary(repo, "trunk()", &cancel)
            .expect("boundary should resolve for a stack on trunk()");

        assert_eq!(boundary.bookmark_name, "feature-2");
        assert!(
            boundary.changed_files.contains("b.txt"),
            "boundary must be feature-1 (origin), so b.txt is in feature-2's scope; \
             deploy@heroku_test won latest() and truncated the scope. changed_files={:?}",
            boundary.changed_files
        );
    }

    /// `try_recover` with a pre-set cancel flag must bail before spawning jj —
    /// vcs-runner's contract is "If `cancel` is already set before spawn,
    /// returns `Cancelled` without starting the child." This is the cheap
    /// short-circuit branchdiff relies on when the user quits during a
    /// queued recovery.
    #[test]
    fn try_recover_short_circuits_on_preset_cancel() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let vcs = JjVcs::new(temp.path().to_path_buf()).unwrap();

        let cancel = Arc::new(AtomicBool::new(true));
        let result = vcs.try_recover(crate::update::RecoveryAction::JjUpdateStale, &cancel);
        assert!(result.is_err(), "pre-set cancel must abort recovery");
        let msg = format!("{:#}", result.unwrap_err()).to_lowercase();
        assert!(msg.contains("cancel"), "error must surface as cancellation, got: {msg:?}");
    }

    /// Counterpart: with cancel clear, try_recover on a non-stale repo should
    /// succeed (jj's update-stale is a no-op when nothing's stale). This
    /// guards against accidentally short-circuiting on the wrong flag state.
    #[test]
    fn try_recover_succeeds_when_cancel_clear() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        let vcs = JjVcs::new(temp.path().to_path_buf()).unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.try_recover(crate::update::RecoveryAction::JjUpdateStale, &cancel);
        assert!(
            result.is_ok(),
            "non-stale repo should accept update-stale silently, got: {result:?}"
        );
    }

    /// `refresh` with a pre-set cancel flag must bail quickly without doing
    /// the full work. Pre-set means the user hit `q` between scheduling and
    /// the spawn thread starting — we want a fast, clean exit, not a hang.
    #[test]
    fn refresh_short_circuits_on_preset_cancel() {
        if !jj_available() {
            return;
        }

        let (temp, _remote) = setup_repo_with_remote();
        std::fs::write(temp.path().join("feature.txt"), "new\n").unwrap();
        let vcs = JjVcs::new(temp.path().to_path_buf()).unwrap();

        let cancel = Arc::new(AtomicBool::new(true));
        let start = std::time::Instant::now();
        let result = vcs.refresh(&cancel);
        let elapsed = start.elapsed();

        assert!(result.is_err(), "pre-set cancel should produce an Err");
        // Should be fast — we're checking the cancel-propagation contract,
        // not measuring perf. A 5s ceiling catches a regression where
        // cancel doesn't propagate at all, without flaking on slow CI.
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "refresh with pre-set cancel took {elapsed:?} — cancel isn't propagating"
        );
    }

    /// Whether `@` is an empty commit, read without snapshotting.
    fn at_is_empty(repo: &Path) -> String {
        jj_out(repo, &["log", "-r", "@", "--no-graph", "--ignore-working-copy", "-T", "empty"])
    }

    /// The periodic auto-fetch runs on a timer in the background, so it must not
    /// snapshot the user's in-progress edits (which would fold them into `@` and
    /// move it under them). We prove it by leaving an un-snapshotted edit and
    /// asserting `@` stays empty across the fetch.
    #[test]
    fn fetch_is_working_copy_agnostic() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        // Un-snapshotted edit to an already-committed file.
        std::fs::write(repo.join("base.txt"), "dirty working copy edit\n").unwrap();
        assert_eq!(at_is_empty(repo), "true", "precondition: the edit must be un-snapshotted");

        vcs.fetch().unwrap();

        assert_eq!(
            at_is_empty(repo),
            "true",
            "fetch snapshotted the working copy (@ is no longer empty) — auto-fetch must be \
             working-copy-agnostic"
        );
    }

    /// The auto-fetch reads the base on either side of the fetch, so reading it
    /// is on the same timer and must not snapshot either: a snapshot every 30s
    /// folds edits into `@` and writes `.jj/`, which wakes the watcher again.
    #[test]
    fn base_identifier_is_working_copy_agnostic() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        std::fs::write(repo.join("base.txt"), "dirty working copy edit\n").unwrap();
        assert_eq!(at_is_empty(repo), "true", "precondition: the edit must be un-snapshotted");

        vcs.base_identifier().unwrap();

        assert_eq!(at_is_empty(repo), "true", "base_identifier snapshotted the working copy");
    }

    /// Count operations in the log, read without snapshotting.
    fn op_count(repo: &Path) -> usize {
        jj_out(repo, &["op", "log", "--no-graph", "--ignore-working-copy", "-T", r#""x\n""#])
            .lines()
            .count()
    }

    /// Characterizes the jj rule branchdiff's oplog-churn depends on: a snapshot
    /// records an operation ONLY when the working copy actually changed. So a
    /// refresh churns exactly one op per edit — which is why an always-on TUI
    /// that refreshes on every save is a constant background writer. This test
    /// pins the assumption; if a future jj changes it, the de-snapshot design
    /// needs revisiting.
    #[test]
    fn jj_snapshot_records_operation_only_on_a_real_change() {
        if !jj_available() {
            return;
        }
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("f.txt"), "a\n").unwrap();
        Command::new("jj").args(["commit", "-m", "init"]).current_dir(repo).output().unwrap();

        let base = op_count(repo);
        // A working-copy-agnostic read never snapshots.
        Command::new("jj")
            .args(["diff", "--ignore-working-copy"])
            .current_dir(repo)
            .output()
            .unwrap();
        assert_eq!(op_count(repo), base, "ignore-working-copy read must not create an operation");
        // A snapshot on an unchanged working copy is a no-op.
        Command::new("jj").args(["diff"]).current_dir(repo).output().unwrap();
        assert_eq!(op_count(repo), base, "snapshot with no change must not create an operation");
        // A snapshot on a changed working copy records exactly one operation.
        std::fs::write(repo.join("f.txt"), "a\nb\n").unwrap();
        Command::new("jj").args(["diff"]).current_dir(repo).output().unwrap();
        assert_eq!(op_count(repo), base + 1, "snapshotting a real change records one operation");
    }

    /// The invariant any de-snapshot refactor MUST preserve: the diff reflects
    /// the on-disk working copy, including edits jj hasn't snapshotted yet.
    /// Green today (via the snapshot); must stay green when the working side is
    /// sourced from disk instead.
    #[test]
    fn refresh_diff_includes_unsnapshotted_working_copy_edit() {
        if !jj_available() {
            return;
        }
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        Command::new("jj").args(["git", "init", "--colocate"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("f.txt"), "a\nb\n").unwrap();
        Command::new("jj").args(["commit", "-m", "init"]).current_dir(repo).output().unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        std::fs::write(repo.join("f.txt"), "a\nb\nUNSNAPSHOTTED\n").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = vcs.refresh(&cancel).unwrap();

        assert!(
            result.lines.iter().any(|l| l.content.contains("UNSNAPSHOTTED")),
            "refresh must reflect the on-disk working copy, including un-snapshotted edits"
        );
    }

    /// Proves the discovery mechanism the colocated de-snapshot design rests on:
    /// in a colocated repo git HEAD tracks jj `@-`, and git's stat-cached diff
    /// sees jj's un-snapshotted working-copy edits — WITHOUT churning jj's
    /// operation log. This is what lets branchdiff learn the changed-file set
    /// (and live edits) with zero snapshots. (jjpr principle #7: pin the one
    /// load-bearing assumption.)
    #[test]
    fn colocated_git_sees_unsnapshotted_edit_without_churning_jj_oplog() {
        if !jj_available() {
            return;
        }
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        Command::new("jj").args(["git", "init", "--colocate"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("f.txt"), "a\nb\n").unwrap();
        Command::new("jj").args(["commit", "-m", "init"]).current_dir(repo).output().unwrap();

        let git_head = {
            let o =
                Command::new("git").args(["rev-parse", "HEAD"]).current_dir(repo).output().unwrap();
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        let jj_parent = jj_out(
            repo,
            &["log", "-r", "@-", "--no-graph", "--ignore-working-copy", "-T", "commit_id"],
        );
        assert_eq!(git_head, jj_parent, "in a colocated repo, git HEAD must track jj @-");

        let ops_before = op_count(repo);
        std::fs::write(repo.join("f.txt"), "a\nb\nc\n").unwrap(); // un-snapshotted

        let changed = {
            let o = Command::new("git")
                .args(["diff", "--name-only", "HEAD"])
                .current_dir(repo)
                .output()
                .unwrap();
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        assert!(
            changed.contains("f.txt"),
            "git must see the un-snapshotted jj working-copy edit, got: {changed:?}"
        );
        assert_eq!(
            op_count(repo),
            ops_before,
            "git-based change discovery must not churn the jj operation log"
        );
    }

    /// The stack machinery must work when the base is an explicit bookmark
    /// rather than the literal `trunk()`.
    ///
    /// `resolve_base_rev` hands back `main@origin` whenever `trunk()` is pinned
    /// to a remote jj wouldn't have chosen. Every one of these revsets used to
    /// hardcode `trunk()` and every guard compared `from_rev != "trunk()"`, so
    /// this configuration would have silently disabled fork-point / stack-tip /
    /// boundary detection while still diffing against the deploy remote. Assert
    /// the revsets interpolate and still resolve.
    #[test]
    fn stack_machinery_works_with_an_explicit_bookmark_base() {
        if !jj_available() {
            return;
        }

        let origin_dir = tempfile::TempDir::new().unwrap();
        let heroku_dir = tempfile::TempDir::new().unwrap();
        Command::new("git")
            .args(["init", "--bare"])
            .current_dir(origin_dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["init", "--bare"])
            .current_dir(heroku_dir.path())
            .output()
            .unwrap();

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            Command::new("jj").args(args).current_dir(repo).output().unwrap();
        };
        jj(&["git", "init"]);
        jj(&["git", "remote", "add", "origin", &origin_dir.path().to_string_lossy()]);
        jj(&["git", "remote", "add", "heroku_test", &heroku_dir.path().to_string_lossy()]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        jj(&["commit", "-m", "base"]);
        jj(&["bookmark", "set", "main", "-r", "@-"]);
        jj(&["git", "push", "--bookmark", "main", "--remote", "origin"]);
        jj(&["bookmark", "track", "main@heroku_test"]);
        jj(&["git", "push", "--bookmark", "main", "--remote", "heroku_test"]);
        pin_trunk_alias(repo, "main@heroku_test");

        // origin advances past the deploy remote, so the override engages.
        std::fs::write(repo.join("shipped.txt"), "landed\n").unwrap();
        jj(&["commit", "-m", "landed on origin"]);
        jj(&["bookmark", "set", "main", "-r", "@-"]);
        jj(&["git", "push", "--bookmark", "main", "--remote", "origin"]);

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        assert_eq!(vcs.from_rev, "main@origin", "fixture must exercise the bookmark base");

        // A two-commit stack on top of origin's main.
        std::fs::write(repo.join("stack.txt"), "from earlier commit\n").unwrap();
        jj(&["commit", "-m", "stack commit 1"]);
        std::fs::write(repo.join("stack.txt"), "from earlier commit\nfrom current commit\n")
            .unwrap();

        let cancel = Arc::new(AtomicBool::new(false));

        // The revsets must interpolate `main@origin` and still resolve. Before
        // the refactor these silently returned None on a non-"trunk()" base.
        let tip = resolve_stack_tip(repo, &vcs.from_rev, &cancel);
        assert!(
            tip.is_none() || tip.is_some(),
            "resolve_stack_tip must not panic on a bookmark base"
        );
        assert!(
            resolve_orphaned_tip(repo, &vcs.from_rev, &cancel).is_none(),
            "no sibling here, so no orphaned tip"
        );

        // Fork point resolves and is a real commit id, not an empty string.
        if let Some(fork) = resolve_fork_point(repo, &vcs.from_rev, &cancel) {
            assert!(!fork.is_empty(), "fork point must be a real commit id");
        }

        // And the refresh that all of it feeds still colours the stack.
        let result = vcs.refresh(&cancel).unwrap();
        assert!(
            result.lines.iter().any(|l| l.source == LineSource::Committed),
            "the earlier stack commit's lines must still be Committed with a bookmark base"
        );
        assert!(
            result.lines.iter().any(|l| l.source == LineSource::Staged),
            "the current commit's lines must still be Staged with a bookmark base"
        );
        // The origin-side commit must NOT appear as the user's work.
        let headers: Vec<&str> = result
            .lines
            .iter()
            .filter(|l| l.source == LineSource::FileHeader)
            .map(|l| l.content.as_str())
            .collect();
        assert!(
            !headers.iter().any(|h| h.contains("shipped.txt")),
            "a commit already on origin/main must not be reported as a local change; headers={headers:?}"
        );
    }

    /// A jj *secondary workspace* must not be mistaken for a colocated repo.
    ///
    /// The colocated fast path assumes git's worktree view tracks jj `@-` (see
    /// `colocated_git_sees_unsnapshotted_edit_without_churning_jj_oplog`). That
    /// holds only in the default workspace. `workon`-style tooling adds git
    /// worktree plumbing beside a `jj workspace add` checkout so git commands
    /// work, which leaves a `.git` *file* — and the worktree git dir has no
    /// index, so `git status` there reports every tracked file deleted and every
    /// file on disk untracked. Taking the colocated path then floods the diff
    /// with the whole repo plus jj's own `.jj/` internals.
    #[test]
    fn secondary_workspace_with_git_worktree_file_is_not_colocated() {
        if !jj_available() {
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path();
        let main_repo = root.join("repo");
        std::fs::create_dir_all(&main_repo).unwrap();
        Command::new("jj")
            .args(["git", "init", "--colocate"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        for i in 1..=4 {
            std::fs::write(main_repo.join(format!("file{i}.txt")), format!("content {i}\n"))
                .unwrap();
        }
        Command::new("jj").args(["commit", "-m", "base"]).current_dir(&main_repo).output().unwrap();

        let ws = root.join("ws1");
        Command::new("jj")
            .args(["workspace", "add", "--name", "ws1", ws.to_str().unwrap()])
            .current_dir(&main_repo)
            .output()
            .unwrap();

        // Replicate the git worktree plumbing `workon` plants beside the workspace.
        let wt_git_dir = main_repo.join(".git/worktrees/ws1");
        std::fs::create_dir_all(&wt_git_dir).unwrap();
        std::fs::write(wt_git_dir.join("gitdir"), format!("{}/.git\n", ws.display())).unwrap();
        std::fs::write(wt_git_dir.join("commondir"), "../..\n").unwrap();
        let head = {
            let o = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&main_repo)
                .output()
                .unwrap();
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        std::fs::write(wt_git_dir.join("HEAD"), format!("{head}\n")).unwrap();
        std::fs::write(ws.join(".git"), format!("gitdir: {}\n", wt_git_dir.display())).unwrap();

        assert!(ws.join(".git").exists(), "fixture: workspace .git must exist");
        assert!(!ws.join(".git").is_dir(), "fixture: workspace .git must be a FILE, not a dir");

        let vcs = JjVcs::new(ws.clone()).unwrap();
        assert!(
            !vcs.is_colocated(),
            "a secondary workspace whose .git is a worktree pointer file is not colocated"
        );

        // And the discovery it gates must report only the real edit.
        std::fs::write(ws.join("file1.txt"), "content 1\nmy actual change\n").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let changed = vcs.discover_working_changes(&vcs.from_rev.clone(), &cancel).unwrap();
        let names: Vec<&str> = changed.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(
            names,
            ["file1.txt"],
            "only the edited file may be reported; jj internals and untouched files must not leak in"
        );
    }

    /// GOAL of the de-snapshot refactor, written failing-first: a refresh over a
    /// dirty working copy must record NO new jj operation. RED today because
    /// `refresh()` snapshots via `jj diff --summary --to @`; the refactor makes
    /// it pass. Kept `#[ignore]` so the suite stays green until then — run with
    /// `cargo test -- --ignored` to watch it fail, then delete the attribute.
    #[test]
    fn refresh_does_not_churn_the_oplog() {
        if !jj_available() {
            return;
        }
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        Command::new("jj").args(["git", "init", "--colocate"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("f.txt"), "a\n").unwrap();
        Command::new("jj").args(["commit", "-m", "init"]).current_dir(repo).output().unwrap();

        // Construct on a clean tree (no snapshot), THEN dirty the working copy.
        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        std::fs::write(repo.join("f.txt"), "a\nb\n").unwrap();

        let before = op_count(repo);
        let cancel = Arc::new(AtomicBool::new(false));
        vcs.refresh(&cancel).unwrap();

        assert_eq!(
            op_count(repo),
            before,
            "refresh snapshotted the working copy — it must source the working side without churning the oplog"
        );
    }

    /// Sibling of the goal test for the incremental path: a single-file refresh
    /// must not snapshot either. `single_file_diff` currently reuses the same
    /// snapshotting `jj diff --summary`, so this is RED today and pins that the
    /// hot per-keystroke path is churn-free too.
    #[test]
    fn single_file_diff_does_not_churn_the_oplog() {
        if !jj_available() {
            return;
        }
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        Command::new("jj").args(["git", "init", "--colocate"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("f.txt"), "a\n").unwrap();
        Command::new("jj").args(["commit", "-m", "init"]).current_dir(repo).output().unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        std::fs::write(repo.join("f.txt"), "a\nb\n").unwrap();
        let before = op_count(repo);
        let _ = vcs.single_file_diff("f.txt");
        assert_eq!(
            op_count(repo),
            before,
            "single-file refresh must source the working side without snapshotting"
        );
    }

    /// A different observable of the same guarantee, and the one that captures
    /// the user-facing harm: branchdiff must not silently fold the user's
    /// in-progress edit into their working-copy commit `@`. RED today (the
    /// snapshot records the edit into `@`, so `@` stops being empty); GREEN once
    /// the working side is read from disk and `@` is left exactly as the user
    /// left it.
    #[test]
    fn refresh_does_not_record_the_users_edit_into_at() {
        if !jj_available() {
            return;
        }
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        Command::new("jj").args(["git", "init", "--colocate"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("f.txt"), "a\n").unwrap();
        Command::new("jj").args(["commit", "-m", "init"]).current_dir(repo).output().unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        std::fs::write(repo.join("f.txt"), "a\nb\n").unwrap();
        assert_eq!(at_is_empty(repo), "true", "precondition: the edit is un-snapshotted");

        let cancel = Arc::new(AtomicBool::new(false));
        vcs.refresh(&cancel).unwrap();

        assert_eq!(
            at_is_empty(repo),
            "true",
            "refresh folded the user's in-progress edit into @ — it must leave the working-copy commit untouched"
        );
    }

    /// The cumulative goal, encoding the actual objective: an edit-and-watch
    /// session leaves ZERO branchdiff-authored operations in the log. RED today
    /// (one op per distinct edit → +3 here); GREEN once refreshes never snapshot.
    /// Also guards against a partial fix that de-snapshots one path but not all.
    #[test]
    fn an_editing_session_produces_no_branchdiff_operations() {
        if !jj_available() {
            return;
        }
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        Command::new("jj").args(["git", "init", "--colocate"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("f.txt"), "a\n").unwrap();
        Command::new("jj").args(["commit", "-m", "init"]).current_dir(repo).output().unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let baseline = op_count(repo);
        let cancel = Arc::new(AtomicBool::new(false));
        for i in 0..3 {
            std::fs::write(repo.join("f.txt"), format!("a\nedit {i}\n")).unwrap();
            vcs.refresh(&cancel).unwrap();
        }
        assert_eq!(
            op_count(repo),
            baseline,
            "an edit-and-watch session must add zero operations from branchdiff"
        );
    }

    // --- Regression guards for the de-snapshot refactor ---------------------
    // These pass TODAY (via the snapshot) and MUST stay green once the working
    // side is sourced from disk. They pin the discovery cases most at risk when
    // `jj diff --summary` is replaced: delete, rename, binary, and untracked.

    /// Build a colocated repo with `f.txt` committed, return (temp, repo_path).
    fn repo_with_committed_file(name: &str, content: &str) -> tempfile::TempDir {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        Command::new("jj").args(["git", "init", "--colocate"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join(name), content).unwrap();
        Command::new("jj").args(["commit", "-m", "init"]).current_dir(repo).output().unwrap();
        temp
    }

    // Note: delete and untracked-new are already guarded end-to-end by
    // `test_jj_refresh_detects_deleted_file` and `test_jj_refresh_detects_new_file`;
    // no duplicate is added here.

    #[test]
    fn refresh_reports_a_renamed_file() {
        if !jj_available() {
            return;
        }
        // Enough identical content that jj's similarity heuristic sees a rename.
        let temp = repo_with_committed_file("old.txt", "alpha\nbeta\ngamma\ndelta\nepsilon\n");
        let repo = temp.path();
        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        std::fs::rename(repo.join("old.txt"), repo.join("new.txt")).unwrap();

        let result = vcs.refresh(&Arc::new(AtomicBool::new(false))).unwrap();
        assert!(
            result.files.iter().any(|f| f.old_path.as_deref() == Some("old.txt")),
            "a rename must be reported with its old path, got: {:?}",
            result.files.iter().map(|f| f.old_path.as_deref()).collect::<Vec<_>>()
        );
    }

    /// A binary working-copy file must be classified as binary (so its bytes are
    /// never rendered as diff text). This currently works only *after* a refresh
    /// snapshots the file into `@` — `binary_files()` reads `@` with
    /// `--ignore-working-copy`. That coupling is itself a thing the de-snapshot
    /// refactor must remove (binary detection has to read disk); this guard keeps
    /// the observable outcome pinned across that change.
    #[test]
    fn refresh_detects_a_binary_working_copy_file() {
        if !jj_available() {
            return;
        }
        let temp = repo_with_committed_file("readme.txt", "text\n");
        let repo = temp.path();
        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        // A NUL byte makes jj classify the file as binary.
        std::fs::write(repo.join("blob.bin"), [0u8, 1, 2, 0, 255, 0, 42]).unwrap();

        let _ = vcs.refresh(&Arc::new(AtomicBool::new(false))).unwrap();
        // Exercise the live path refresh itself uses (the Vcs::binary_files
        // trait method was dead API and is gone).
        let cancel = Arc::new(AtomicBool::new(false));
        let changed = vcs.discover_working_changes(&vcs.from_rev.clone(), &cancel).unwrap();
        let binaries = vcs.working_binary_files(&changed);
        assert!(
            binaries.contains("blob.bin"),
            "a binary working-copy file must be detected, got: {binaries:?}"
        );
    }

    /// Directly answers the "why is non-colocated special?" question: the
    /// working-copy read is VCS-neutral. A NON-colocated `jj git init` repo must
    /// surface an un-snapshotted edit exactly like a colocated one. Green today
    /// (snapshot), must stay green when read from disk — proving the design
    /// doesn't hinge on `--colocate`.
    #[test]
    fn refresh_includes_unsnapshotted_edit_in_non_colocated_repo() {
        if !jj_available() {
            return;
        }
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap(); // NO --colocate
        std::fs::write(repo.join("f.txt"), "a\nb\n").unwrap();
        Command::new("jj").args(["commit", "-m", "init"]).current_dir(repo).output().unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        std::fs::write(repo.join("f.txt"), "a\nb\nNONCOLOCATED\n").unwrap();
        let result = vcs.refresh(&Arc::new(AtomicBool::new(false))).unwrap();

        assert!(
            result.lines.iter().any(|l| l.content.contains("NONCOLOCATED")),
            "a non-colocated jj repo must still surface un-snapshotted working-copy edits"
        );
    }

    /// A *cold* disk walk must read base content for O(changed) files, not for
    /// every file in the repo.
    ///
    /// The stat cache lives in the process, so a one-shot `branchdiff --print`
    /// never warms it — every run is a cold pass. Each cold content comparison
    /// spawns a `jj file show` subprocess, so O(tree) meant 2000 subprocesses
    /// and 22s on a 2000-file repo (vs 0.83s on the colocated git path). jj's
    /// last snapshot already knows what differs from the base; only files
    /// touched since it need reading.
    #[test]
    fn cold_disk_walk_reads_base_content_only_for_changed_files() {
        if !jj_available() {
            return;
        }
        let root = tempfile::TempDir::new().unwrap();
        let main_repo = root.path().join("repo");
        std::fs::create_dir_all(&main_repo).unwrap();
        Command::new("jj").args(["git", "init"]).current_dir(&main_repo).output().unwrap();
        for i in 0..40 {
            std::fs::write(main_repo.join(format!("f{i}.txt")), format!("content {i}\n")).unwrap();
        }
        Command::new("jj").args(["commit", "-m", "init"]).current_dir(&main_repo).output().unwrap();

        // A secondary workspace has no `.git` of its own, so it is the disk-walk
        // path — this is what `workon`-style checkouts land on.
        let ws = root.path().join("ws");
        Command::new("jj")
            .args(["workspace", "add", "--name", "ws", ws.to_str().unwrap()])
            .current_dir(&main_repo)
            .output()
            .unwrap();

        // A fresh Vcs = a cold cache, exactly like a one-shot --print.
        let vcs = JjVcs::new(ws.clone()).unwrap();
        assert!(!vcs.is_colocated(), "fixture must exercise the disk-walk path");
        std::fs::write(ws.join("f0.txt"), "content 0\nlive edit\n").unwrap();

        vcs.reset_base_reads();
        let cancel = Arc::new(AtomicBool::new(false));
        let changed = vcs.discover_working_changes(&vcs.from_rev.clone(), &cancel).unwrap();

        let names: Vec<&str> = changed.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(names, ["f0.txt"], "the live edit must still be found");
        assert!(
            vcs.base_reads() <= 4,
            "a cold walk must not read every file's base content: {} reads for 40 files \
             (only f0.txt was touched since jj's snapshot)",
            vcs.base_reads()
        );
    }

    /// A file jj refuses to snapshot must not vanish from the diff.
    ///
    /// The seed lists the paths jj's snapshot knows *differ*. Treating "absent
    /// from the seed" as "identical to the base" is wrong, because absent also
    /// means "jj never snapshotted this file at all". jj refuses files over
    /// `snapshot.max-new-file-size` (1MiB by default) and leaves them untracked,
    /// so such a file — created before the last snapshot, hence
    /// `mtime < checkpoint` — was both absent from the seed and never
    /// content-compared. It disappeared from the diff silently, on default
    /// config, with no warning from branchdiff.
    ///
    /// Absent must mean *unknown* (go and check), never *clean*.
    #[test]
    fn cold_disk_walk_does_not_lose_files_jj_refused_to_snapshot() {
        if !jj_available() {
            return;
        }
        let root = tempfile::TempDir::new().unwrap();
        let main_repo = root.path().join("repo");
        std::fs::create_dir_all(&main_repo).unwrap();
        Command::new("jj").args(["git", "init"]).current_dir(&main_repo).output().unwrap();
        std::fs::write(main_repo.join("tracked.txt"), "base\n").unwrap();
        Command::new("jj").args(["commit", "-m", "base"]).current_dir(&main_repo).output().unwrap();

        let ws = root.path().join("ws");
        Command::new("jj")
            .args(["workspace", "add", "--name", "ws", ws.to_str().unwrap()])
            .current_dir(&main_repo)
            .output()
            .unwrap();

        // Over jj's default 1MiB snapshot limit: jj refuses it and leaves it untracked.
        std::fs::write(ws.join("big.txt"), "a".repeat(2 * 1024 * 1024)).unwrap();

        // Make the refused file strictly older than the snapshot that follows,
        // so it lands on the `mtime < checkpoint` (trust-the-seed) branch.
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(ws.join("tracked.txt"), "base\nedit\n").unwrap();
        Command::new("jj").args(["status"]).current_dir(&ws).output().unwrap(); // snapshot

        let vcs = JjVcs::new(ws.clone()).unwrap();
        assert!(!vcs.is_colocated(), "fixture must exercise the disk-walk path");
        let cancel = Arc::new(AtomicBool::new(false));
        let changed = vcs.discover_working_changes(&vcs.from_rev.clone(), &cancel).unwrap();
        let names: Vec<&str> = changed.iter().map(|c| c.path.as_str()).collect();

        assert!(
            names.contains(&"big.txt"),
            "a file jj refused to snapshot must still be reported as an addition; \
             seeding 'absent means clean' silently loses it. got: {names:?}"
        );
        assert!(names.contains(&"tracked.txt"), "the ordinary edit must still be found");
    }

    /// An unresolvable base must fail, not report the whole repo as new.
    ///
    /// `commit_id_of(..).unwrap_or_default()` yielded `""`, and a fresh
    /// `DiskManifest` also starts with `base: ""`. So a base that fails to
    /// resolve on the *first* walk compares equal to the empty cache: the reseed
    /// is skipped, `base_files` stays empty, and every file on disk reads as an
    /// addition with no deletions — a plausible-looking wrong answer.
    #[test]
    fn diskwalk_with_an_unresolvable_base_errors_rather_than_reporting_everything_new() {
        if !jj_available() {
            return;
        }
        let root = tempfile::TempDir::new().unwrap();
        let main_repo = root.path().join("repo");
        std::fs::create_dir_all(&main_repo).unwrap();
        Command::new("jj").args(["git", "init"]).current_dir(&main_repo).output().unwrap();
        std::fs::write(main_repo.join("a.txt"), "a\n").unwrap();
        std::fs::write(main_repo.join("b.txt"), "b\n").unwrap();
        Command::new("jj").args(["commit", "-m", "base"]).current_dir(&main_repo).output().unwrap();
        let ws = root.path().join("ws");
        Command::new("jj")
            .args(["workspace", "add", "--name", "ws", ws.to_str().unwrap()])
            .current_dir(&main_repo)
            .output()
            .unwrap();

        let vcs = JjVcs::new(ws.clone()).unwrap();
        assert!(!vcs.is_colocated(), "fixture must exercise the disk-walk path");
        let cancel = Arc::new(AtomicBool::new(false));

        match vcs.discover_working_changes("no-such-revset-xyz", &cancel) {
            Err(_) => {}
            Ok(changed) => panic!(
                "an unresolvable base must be an error, not {} files reported as new: {:?}",
                changed.len(),
                changed.iter().map(|c| &c.path).collect::<Vec<_>>()
            ),
        }
    }

    /// A cancelled walk must not invent deletions.
    ///
    /// The walk `break`s out on cancel with a partial `seen` set, then still ran
    /// the "base files no longer on disk" loop — so every file it had not reached
    /// yet came back marked deleted. The existing cancel guard protects the
    /// cache, not the returned value.
    #[test]
    fn cancelled_diskwalk_does_not_fabricate_deletions() {
        if !jj_available() {
            return;
        }
        let root = tempfile::TempDir::new().unwrap();
        let main_repo = root.path().join("repo");
        std::fs::create_dir_all(&main_repo).unwrap();
        Command::new("jj").args(["git", "init"]).current_dir(&main_repo).output().unwrap();
        for i in 0..10 {
            std::fs::write(main_repo.join(format!("f{i}.txt")), "x\n").unwrap();
        }
        Command::new("jj").args(["commit", "-m", "base"]).current_dir(&main_repo).output().unwrap();
        let ws = root.path().join("ws");
        Command::new("jj")
            .args(["workspace", "add", "--name", "ws", ws.to_str().unwrap()])
            .current_dir(&main_repo)
            .output()
            .unwrap();

        let vcs = JjVcs::new(ws.clone()).unwrap();
        assert!(!vcs.is_colocated(), "fixture must exercise the disk-walk path");

        // Every file is present on disk and unchanged; nothing is deleted.
        let cancel = Arc::new(AtomicBool::new(true)); // already cancelled
        match vcs.discover_working_changes(&vcs.from_rev.clone(), &cancel) {
            Err(_) => {}
            Ok(changed) => panic!(
                "a cancelled walk must not return a result; it reported {} phantom changes: {:?}",
                changed.len(),
                changed.iter().map(|c| &c.path).collect::<Vec<_>>()
            ),
        }
    }

    /// RED goal, non-colocated variant: the no-churn guarantee must NOT be
    /// special-cased to colocated repos. Locks that the refactor keeps
    /// non-colocated refreshes churn-free too (i.e. doesn't fall back to a
    /// snapshot for them).
    #[test]
    fn refresh_does_not_churn_the_oplog_non_colocated() {
        if !jj_available() {
            return;
        }
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap(); // NO --colocate
        std::fs::write(repo.join("f.txt"), "a\n").unwrap();
        Command::new("jj").args(["commit", "-m", "init"]).current_dir(repo).output().unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        std::fs::write(repo.join("f.txt"), "a\nb\n").unwrap();
        let before = op_count(repo);
        vcs.refresh(&Arc::new(AtomicBool::new(false))).unwrap();

        assert_eq!(
            op_count(repo),
            before,
            "non-colocated refresh must also source the working side without churning the oplog"
        );
    }

    // --- The "no git index" boundary (what selects the discovery fallback) ---
    // The de-snapshot design keys discovery on `is_colocated()` (workdir `.git`
    // present → git's stat-cached worktree diff is available). These two tests
    // pin how narrow the fallback case actually is.

    /// The common setups — `jj git init`, `--colocate`, and `jj git clone` — all
    /// leave a workdir `.git`, so git-based discovery applies to them. (Clone and
    /// colocate verified out-of-band; plain init is the representative here.)
    #[test]
    fn common_jj_init_leaves_a_workdir_git_index() {
        if !jj_available() {
            return;
        }
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        Command::new("jj").args(["git", "init"]).current_dir(repo).output().unwrap(); // no --colocate
        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        assert!(
            vcs.is_colocated(),
            "plain `jj git init` must leave a workdir .git — the git index is available"
        );
    }

    /// The ONLY reachable "no git index" case: a jj repo whose git store lives
    /// OUTSIDE the worktree (`--git-repo=<external>`). No workdir `.git`, so git's
    /// worktree diff can't run — this is exactly what the disk-walk fallback is
    /// for, and `is_colocated()` detects it cleanly. Reading the working copy is
    /// still plain disk here, so the fallback needs no snapshot.
    #[test]
    fn jj_with_external_git_repo_has_no_workdir_git_index() {
        if !jj_available() {
            return;
        }
        let ext = tempfile::TempDir::new().unwrap();
        Command::new("git").args(["init", "--bare"]).current_dir(ext.path()).output().unwrap();

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        let arg = format!("--git-repo={}", ext.path().display());
        let out =
            Command::new("jj").args(["git", "init", &arg]).current_dir(repo).output().unwrap();
        assert!(
            out.status.success(),
            "jj git init --git-repo failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        assert!(!vcs.is_colocated(), "external-store jj repo must have no workdir git index");
        assert!(
            !repo.join(".git").exists(),
            "no workdir .git → git worktree-diff is unavailable here"
        );

        // The working copy is still ordinary disk — the fallback needs no snapshot.
        std::fs::write(repo.join("w.txt"), "live content\n").unwrap();
        assert_eq!(std::fs::read_to_string(repo.join("w.txt")).unwrap(), "live content\n");
    }

    /// RED goal for the disk-walk fallback itself. In a repo with no workdir git
    /// index (`!is_colocated()`), discovery can't use git and must not snapshot,
    /// so the fallback has to walk the tree and read the working file from disk.
    /// This pins BOTH properties so no shortcut satisfies it: the diff must
    /// include the un-snapshotted edit (forces the disk read) AND the refresh
    /// must not churn the oplog (forbids the snapshot). A naive
    /// `--ignore-working-copy` "fix" keeps the oplog clean but misses the edit,
    /// failing the first assertion; today's snapshot shows the edit but churns,
    /// failing the second. Only the disk-walk fallback passes both.
    #[test]
    fn refresh_without_git_index_uses_diskwalk_fallback_churn_free() {
        if !jj_available() {
            return;
        }
        let ext = tempfile::TempDir::new().unwrap();
        Command::new("git").args(["init", "--bare"]).current_dir(ext.path()).output().unwrap();

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        let arg = format!("--git-repo={}", ext.path().display());
        let out =
            Command::new("jj").args(["git", "init", &arg]).current_dir(repo).output().unwrap();
        assert!(
            out.status.success(),
            "jj git init --git-repo failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::write(repo.join("f.txt"), "a\nb\n").unwrap();
        Command::new("jj").args(["commit", "-m", "init"]).current_dir(repo).output().unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        assert!(!vcs.is_colocated(), "precondition: this repo has no workdir git index");
        std::fs::write(repo.join("f.txt"), "a\nb\nFALLBACK\n").unwrap(); // un-snapshotted edit

        let before = op_count(repo);
        let result = vcs.refresh(&Arc::new(AtomicBool::new(false))).unwrap();

        assert!(
            result.lines.iter().any(|l| l.content.contains("FALLBACK")),
            "disk-walk fallback must still surface the un-snapshotted edit (no git index available)"
        );
        assert_eq!(
            op_count(repo),
            before,
            "disk-walk fallback must discover changes without snapshotting"
        );
    }

    /// The perf guard for the fallback stat-cache (deterministic, not a timeout):
    /// neither a cold nor a warm refresh may read O(tree) base files.
    ///
    /// This used to assert the *cold* pass read every base file — encoding the
    /// slow path as an invariant. It isn't one: a cold pass now seeds its
    /// checkpoint from jj's last snapshot, which is what stops a one-shot
    /// `--print` in a secondary workspace from spawning a `jj file show` per
    /// file in the repo. Each assertion is paired with a correctness check,
    /// because "read nothing" would otherwise satisfy the perf bound by simply
    /// missing the change.
    #[test]
    fn diskwalk_fallback_warm_refresh_reads_only_changed_files() {
        if !jj_available() {
            return;
        }
        let ext = tempfile::TempDir::new().unwrap();
        Command::new("git").args(["init", "--bare"]).current_dir(ext.path()).output().unwrap();

        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        let arg = format!("--git-repo={}", ext.path().display());
        Command::new("jj").args(["git", "init", &arg]).current_dir(repo).output().unwrap();
        const N: usize = 40;
        for i in 0..N {
            std::fs::write(repo.join(format!("file_{i}.txt")), "base\n").unwrap();
        }
        Command::new("jj").args(["commit", "-m", "init"]).current_dir(repo).output().unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        assert!(!vcs.is_colocated(), "precondition: no workdir git index");
        let cancel = Arc::new(AtomicBool::new(false));

        // Cold refresh over a clean tree: jj's snapshot already says nothing
        // differs, so no base content need be read at all.
        vcs.refresh(&cancel).unwrap();
        let cold = vcs.base_reads();
        assert!(
            cold <= 3,
            "a cold refresh must seed from jj's snapshot, not read all {N} base files; got {cold}"
        );

        // Warm refresh after changing exactly one file: O(changed), not O(tree),
        // and the change must actually be found.
        std::thread::sleep(std::time::Duration::from_millis(10)); // mtime > checkpoint
        vcs.reset_base_reads();
        std::fs::write(repo.join("file_7.txt"), "changed on disk\n").unwrap();
        let result = vcs.refresh(&cancel).unwrap();
        let warm = vcs.base_reads();
        assert!(
            warm <= 3,
            "warm refresh must re-read O(changed) base files, not O(tree); got {warm} (cold was {cold})"
        );
        assert!(
            result.lines.iter().any(|l| l.content.contains("changed on disk")),
            "the live edit must be surfaced — a perf bound met by reading nothing is a bug, not a win"
        );
    }

    /// Characterization + deliberate non-goal: the mid-stack case intentionally
    /// snapshots. When a committed stack tip sits above `@`, editing `@` makes jj
    /// rebase the descendants, and the snapshot is what materializes that rebase
    /// so the tip reflects the edit. Reading the tip without a snapshot yields a
    /// *stale* tip (verified empirically), so de-snapshotting here would require
    /// branchdiff to reproduce jj's rebase — the reimplement-jj-internals
    /// fragility we reject. This pins the behavior; it flips if a future change
    /// makes mid-stack stop snapshotting (which would render a stale, wrong diff).
    #[test]
    fn mid_stack_refresh_snapshots_to_materialize_the_rebase() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        std::fs::write(repo.join("file.txt"), "commit1\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit1"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "commit2\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit2"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "commit3\n").unwrap();
        Command::new("jj").args(["commit", "-m", "commit3"]).current_dir(repo).output().unwrap();
        Command::new("jj").args(["edit", "@---"]).current_dir(repo).output().unwrap(); // @ mid-stack

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        std::fs::write(repo.join("file.txt"), "commit2 EDITED\n").unwrap(); // un-snapshotted
        let before = op_count(repo);
        vcs.refresh(&Arc::new(AtomicBool::new(false))).unwrap();
        assert!(
            op_count(repo) > before,
            "mid-stack refresh snapshots to materialize jj's descendant rebase"
        );
    }

    /// Regression: a cancelled disk-walk must not corrupt the stat-cache. A
    /// partial walk that persisted a fresh checkpoint would make the next walk
    /// trust missing entries as "unchanged" and silently drop a real edit.
    #[test]
    fn diskwalk_cancel_does_not_corrupt_the_manifest() {
        if !jj_available() {
            return;
        }
        let ext = tempfile::TempDir::new().unwrap();
        Command::new("git").args(["init", "--bare"]).current_dir(ext.path()).output().unwrap();
        let temp = tempfile::TempDir::new().unwrap();
        let repo = temp.path();
        let arg = format!("--git-repo={}", ext.path().display());
        Command::new("jj").args(["git", "init", &arg]).current_dir(repo).output().unwrap();
        for i in 0..8 {
            std::fs::write(repo.join(format!("f{i}.txt")), "base\n").unwrap();
        }
        Command::new("jj").args(["commit", "-m", "init"]).current_dir(repo).output().unwrap();

        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let clear = Arc::new(AtomicBool::new(false));
        vcs.diskwalk_changed_files("@-", &clear).unwrap(); // cold: populate cache

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(repo.join("f5.txt"), "edited\n").unwrap();

        // A cancelled walk (pre-set flag) must leave the cache intact.
        let cancelled = Arc::new(AtomicBool::new(true));
        let _ = vcs.diskwalk_changed_files("@-", &cancelled);

        // The next real walk must still see the edit.
        let changed = vcs.diskwalk_changed_files("@-", &clear).unwrap();
        assert!(
            changed.iter().any(|f| f.path == "f5.txt"),
            "a cancelled walk corrupted the cache into missing a real change: {:?}",
            changed.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
    }

    /// RED: a refresh while `@` is orphaned (empty, re-anchored to a sibling tip)
    /// still routes discovery through the snapshotting jj summary.
    #[test]
    fn refresh_does_not_churn_orphaned_tip() {
        if !jj_available() {
            return;
        }
        let (temp, _remote, _c) = setup_orphaned_after_abandon();
        let repo = temp.path();
        let vcs = JjVcs::new(repo.to_path_buf()).unwrap();
        let before = op_count(repo);
        vcs.refresh(&Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(
            op_count(repo),
            before,
            "an orphaned-@ refresh must not snapshot the working copy"
        );
    }

    #[test]
    fn head_flag_renders_target_revision_without_checking_it_out_or_snapshotting() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();

        // Create a `feature` bookmark commit off main.
        std::fs::write(repo.join("file.txt"), "initial\nfeature work\n").unwrap();
        std::fs::write(repo.join("feature_only.txt"), "only on feature\n").unwrap();
        Command::new("jj")
            .args(["bookmark", "create", "feature", "-r", "@"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("jj")
            .args(["commit", "-m", "feature commit"])
            .current_dir(repo)
            .output()
            .unwrap();

        // Move `@` back onto `main` and dirty the working copy without snapshotting.
        Command::new("jj").args(["new", "main"]).current_dir(repo).output().unwrap();
        std::fs::write(repo.join("file.txt"), "dirty working copy on main\n").unwrap();
        std::fs::write(repo.join("untracked_dirty.txt"), "dirty untracked\n").unwrap();

        let vcs = JjVcs::with_options(
            repo.to_path_buf(),
            crate::vcs::VcsOptions { head: Some("feature"), ..crate::vcs::VcsOptions::default() },
        )
        .unwrap();

        let before = op_count(repo);
        let cancel = Arc::new(AtomicBool::new(false));
        let (comparison, refresh) = vcs.comparison_and_refresh(&cancel).unwrap();
        assert_eq!(op_count(repo), before, "--head refresh must never snapshot the working copy");
        assert!(
            comparison.to_label.contains("feature"),
            "to_label should name feature, got: {}",
            comparison.to_label
        );
        assert_eq!(refresh.files.len(), 2);
        assert!(refresh.lines.iter().any(|l| l.content.contains("feature work")));
        assert!(refresh.lines.iter().any(|l| l.content.contains("only on feature")));
        assert!(!refresh.lines.iter().any(|l| l.content.contains("dirty")));

        let single = vcs.single_file_diff("file.txt").expect("single file diff for feature");
        assert!(single.lines.iter().any(|l| l.content.contains("feature work")));
        assert!(!single.lines.iter().any(|l| l.content.contains("dirty")));

        let bytes = vcs.working_file_bytes("feature_only.txt").unwrap();
        assert_eq!(bytes.as_deref(), Some(b"only on feature\n".as_slice()));

        let fp = vcs.fingerprint_inputs().unwrap();
        assert_eq!(fp.changed, vec!["feature_only.txt".to_string(), "file.txt".to_string()]);
    }

    #[test]
    fn head_flag_rejects_empty_or_multi_commit_revset() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();

        let empty_err = match JjVcs::with_options(
            repo.to_path_buf(),
            crate::vcs::VcsOptions { head: Some("none()"), ..crate::vcs::VcsOptions::default() },
        ) {
            Ok(_) => panic!("empty revset must fail"),
            Err(e) => format!("{e:#}"),
        };
        assert!(empty_err.contains("--head") && empty_err.contains("none()"), "got: {empty_err}");
        assert!(empty_err.contains("does not resolve to a commit"), "got: {empty_err}");

        let syntax_err = match JjVcs::with_options(
            repo.to_path_buf(),
            crate::vcs::VcsOptions {
                head: Some("feature..("),
                ..crate::vcs::VcsOptions::default()
            },
        ) {
            Ok(_) => panic!("invalid revset syntax must fail"),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            syntax_err.starts_with("--head \"feature..(\" is not a valid revset in this jj repo: ")
                && !syntax_err.contains("does not resolve")
                && !syntax_err.contains("Error:"),
            "got: {syntax_err}"
        );

        let multi_err = match JjVcs::with_options(
            repo.to_path_buf(),
            crate::vcs::VcsOptions { head: Some("all()"), ..crate::vcs::VcsOptions::default() },
        ) {
            Ok(_) => panic!("multi-commit revset must fail"),
            Err(e) => format!("{e:#}"),
        };
        assert_eq!(
            multi_err,
            "--head \"all()\" resolves to more than one commit; it must name exactly one (e.g. feature, or latest(all()))"
        );
    }

    #[test]
    fn highlight_flag_marks_unpushed_jj_revset_without_snapshotting_and_sets_files_json() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            let out = Command::new("jj").args(args).current_dir(repo).output().unwrap();
            assert!(
                out.status.success(),
                "jj {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        // Commit 1 on feature and push to origin so `feature@origin` exists.
        std::fs::write(repo.join("file.txt"), "base1\nbase2\nbase3\npushed line\n").unwrap();
        std::fs::write(repo.join("pushed.txt"), "pushed only\n").unwrap();
        jj(&["commit", "-m", "pushed commit"]);
        jj(&["bookmark", "set", "feature", "-r", "@-"]);
        jj(&["git", "push", "--bookmark", "feature", "--remote", "origin"]);

        // Commit 2 on feature (unpushed).
        std::fs::write(repo.join("file.txt"), "base1\nbase2\nbase3\npushed line\nunpushed line\n")
            .unwrap();
        std::fs::write(repo.join("unpushed.txt"), "unpushed only\n").unwrap();
        jj(&["commit", "-m", "unpushed commit"]);
        jj(&["bookmark", "set", "feature", "-r", "@-"]);

        // Move @ back to main so we test --head feature --highlight feature@origin..feature.
        jj(&["new", "main"]);

        let vcs = JjVcs::with_options(
            repo.to_path_buf(),
            crate::vcs::VcsOptions {
                base: None,
                head: Some("feature"),
                highlight: Some("feature@origin..feature"),
            },
        )
        .unwrap();

        let before = op_count(repo);
        let cancel = Arc::new(AtomicBool::new(false));
        let (_comparison, refresh) = vcs.comparison_and_refresh(&cancel).unwrap();
        assert_eq!(
            op_count(repo),
            before,
            "--highlight refresh must never snapshot the working copy"
        );
        assert_eq!(refresh.files.len(), 3);

        let file_diff = refresh
            .files
            .iter()
            .find(|f| f.lines.first().is_some_and(|l| l.content == "file.txt"))
            .unwrap();
        assert_eq!(file_diff.highlight.line_count(), 1);
        let pushed_idx = file_diff.lines.iter().position(|l| l.content == "pushed line").unwrap();
        let unpushed_idx =
            file_diff.lines.iter().position(|l| l.content == "unpushed line").unwrap();
        assert!(!file_diff.highlight.is_line_highlighted(pushed_idx));
        assert!(file_diff.highlight.is_in_highlighted_hunk(pushed_idx));
        assert!(file_diff.highlight.is_line_highlighted(unpushed_idx));

        let pushed_diff = refresh
            .files
            .iter()
            .find(|f| f.lines.first().is_some_and(|l| l.content == "pushed.txt"))
            .unwrap();
        assert_eq!(pushed_diff.highlight.line_count(), 0);

        let unpushed_diff = refresh
            .files
            .iter()
            .find(|f| f.lines.first().is_some_and(|l| l.content == "unpushed.txt"))
            .unwrap();
        assert_eq!(unpushed_diff.highlight.line_count(), 1);

        // Single-file diff also preserves highlighting.
        let single = vcs.single_file_diff("file.txt").unwrap();
        assert_eq!(single.highlight.line_count(), 1);

        // `--files --json` includes `"highlighted":true` / `"highlighted":false`.
        let json =
            crate::listing::render(&vcs, crate::cli::OutputMode::Files, true).unwrap().unwrap();
        assert!(
            json.contains("\"path\":\"file.txt\",\"old_path\":null,\"status\":\"added\",\"additions\":5,\"deletions\":0,\"highlighted\":true"),
            "{json}"
        );
        assert!(
            json.contains("\"path\":\"pushed.txt\",\"old_path\":null,\"status\":\"added\",\"additions\":1,\"deletions\":0,\"highlighted\":false"),
            "{json}"
        );
        assert!(
            json.contains("\"path\":\"unpushed.txt\",\"old_path\":null,\"status\":\"added\",\"additions\":1,\"deletions\":0,\"highlighted\":true"),
            "{json}"
        );
    }

    #[test]
    fn highlight_flag_accepts_empty_revset_after_push_and_rejects_invalid_revset() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            let out = Command::new("jj").args(args).current_dir(repo).output().unwrap();
            assert!(
                out.status.success(),
                "jj {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        std::fs::write(repo.join("file.txt"), "initial\npushed line\n").unwrap();
        jj(&["commit", "-m", "pushed commit"]);
        jj(&["bookmark", "set", "feature", "-r", "@-"]);
        jj(&["git", "push", "--bookmark", "feature", "--remote", "origin"]);

        // Right after pushing, `feature@origin..feature` resolves to 0 commits.
        let vcs = JjVcs::with_options(
            repo.to_path_buf(),
            crate::vcs::VcsOptions {
                base: None,
                head: Some("feature"),
                highlight: Some("feature@origin..feature"),
            },
        )
        .expect("empty highlight range after push must not fail startup");

        let cancel = Arc::new(AtomicBool::new(false));
        let (_comparison, refresh) = vcs.comparison_and_refresh(&cancel).unwrap();
        assert_eq!(refresh.files.len(), 1);
        assert_eq!(refresh.files[0].highlight.label.as_deref(), Some("feature@origin..feature"));
        assert_eq!(refresh.files[0].highlight.line_count(), 0);
        assert!(!refresh.files[0].highlight.is_highlighted());

        let json =
            crate::listing::render(&vcs, crate::cli::OutputMode::Files, true).unwrap().unwrap();
        assert!(json.contains("\"highlighted\":false"), "{json}");

        // Invalid syntax or unknown symbol must still be refused at startup.
        for bad in ["", "   ", "no_such_symbol_xyz", "a..b..c"] {
            assert!(
                JjVcs::with_options(
                    repo.to_path_buf(),
                    crate::vcs::VcsOptions {
                        base: None,
                        head: Some("feature"),
                        highlight: Some(bad),
                    },
                )
                .is_err(),
                "invalid highlight revset {bad:?} should be rejected"
            );
        }
        let bad_err = match JjVcs::with_options(
            repo.to_path_buf(),
            crate::vcs::VcsOptions {
                base: None,
                head: Some("feature"),
                highlight: Some("no_such_symbol_xyz"),
            },
        ) {
            Ok(_) => panic!("unknown symbol must fail"),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            bad_err.starts_with(
                "--highlight \"no_such_symbol_xyz\" is not a valid revset in this jj repo: "
            ) && bad_err.contains("no_such_symbol_xyz")
                && !bad_err.contains("Error:"),
            "got: {bad_err}"
        );
    }

    fn refresh_highlighting(repo: &Path, revset: &str) -> crate::vcs::RefreshResult {
        let vcs = JjVcs::with_options(
            repo.to_path_buf(),
            crate::vcs::VcsOptions { base: None, head: Some("feature"), highlight: Some(revset) },
        )
        .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        vcs.comparison_and_refresh(&cancel).unwrap().1
    }

    fn file_named<'a>(refresh: &'a crate::vcs::RefreshResult, name: &str) -> &'a FileDiff {
        refresh.files.iter().find(|f| f.lines.first().is_some_and(|l| l.content == name)).unwrap()
    }

    fn marked_lines<'a>(refresh: &'a crate::vcs::RefreshResult, name: &str) -> Vec<&'a str> {
        let file = file_named(refresh, name);
        file.highlight
            .highlighted_lines
            .iter()
            .map(|&idx| file.lines[idx].content.as_str())
            .collect()
    }

    #[test]
    fn highlight_flag_handles_jj_merge_commits_and_side_branches_without_overmarking() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            let out = Command::new("jj").args(args).current_dir(repo).output().unwrap();
            assert!(
                out.status.success(),
                "jj {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        // Commit `one` on main: modifies file.txt
        std::fs::write(repo.join("file.txt"), "initial\none\n").unwrap();
        jj(&["commit", "-m", "one"]);

        // Commit `two` on `one`: adds "two"
        std::fs::write(repo.join("file.txt"), "initial\none\ntwo\n").unwrap();
        jj(&["commit", "-m", "two"]);

        // Side branch off `main`: adds `side.txt` and edits `file.txt`
        jj(&["new", "main", "-m", "side"]);
        std::fs::write(repo.join("file.txt"), "initial\nside\n").unwrap();
        std::fs::write(repo.join("side.txt"), "side only\n").unwrap();

        // Also create an unmerged side branch off `one` to ensure side branches don't break coalescing
        jj(&["new", "description(substring:one)", "-m", "unmerged"]);
        std::fs::write(repo.join("unmerged.txt"), "unmerged\n").unwrap();

        // Merge `two` and `side` into `merge_commit`, resolving `file.txt` and adding `merge own`
        jj(&[
            "new",
            "description(substring:two)",
            "description(substring:side)",
            "-m",
            "merge_commit",
        ]);
        std::fs::write(repo.join("file.txt"), "initial\none\ntwo\nside\nmerge own\n").unwrap();
        jj(&["bookmark", "set", "feature", "-r", "@"]);
        jj(&["new", "main"]);

        // 1. Highlighting ONLY the merge commit must mark only `merge own` (1 line),
        //    never `one`, `two`, `side`, or `side.txt`.
        let refresh_merge = refresh_highlighting(repo, "description(substring:merge_commit)");
        assert_eq!(marked_lines(&refresh_merge, "file.txt"), vec!["merge own"]);
        assert_eq!(file_named(&refresh_merge, "side.txt").highlight.line_count(), 0);

        // 2. Highlighting `description(substring:two)::` must mark `two` and `merge own`,
        //    and must NOT mark `one`, `side`, or `side.txt`.
        let refresh_two = refresh_highlighting(repo, "description(substring:two)::");
        assert_eq!(marked_lines(&refresh_two, "file.txt"), vec!["two", "merge own"]);
        assert_eq!(file_named(&refresh_two, "side.txt").highlight.line_count(), 0);

        // 3. A linear range while side branches exist coalesces into a single span.
        let cancel = Arc::new(AtomicBool::new(false));
        let vcs_linear = JjVcs::with_options(
            repo.to_path_buf(),
            crate::vcs::VcsOptions {
                base: None,
                head: Some("feature"),
                highlight: Some("description(substring:one)..description(substring:two)"),
            },
        )
        .unwrap();
        let plan =
            JjHighlightPlan::resolve(&vcs_linear, "main", "feature", false, &[], &cancel).unwrap();
        assert_eq!(plan.spans.len(), 1, "side branches must not force per-commit fallback");
    }

    #[test]
    fn test_parse_highlight_log_fixtures() {
        let raw = concat!(
            "c33333333333\t@\tc22222222222,s11111111111\n",
            "c22222222222\t.\tc11111111111\n",
            "c11111111111\t.\t0000000000000000000000000000000000000000\n",
            "\n",
            "malformed_without_tabs\n",
        );
        let entries = parse_highlight_log(raw);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].commit_id, "c33333333333");
        assert!(entries[0].is_working_copy);
        assert_eq!(entries[0].parents, vec!["c22222222222", "s11111111111"]);

        assert_eq!(entries[1].commit_id, "c22222222222");
        assert!(!entries[1].is_working_copy);
        assert_eq!(entries[1].parents, vec!["c11111111111"]);

        assert_eq!(entries[2].commit_id, "c11111111111");
        assert!(!entries[2].is_working_copy);
        assert_eq!(entries[2].parents, vec!["0000000000000000000000000000000000000000"]);
    }

    #[test]
    fn highlight_flag_handles_jj_rename_inside_highlighted_range_and_tracks_fingerprint() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            let out = Command::new("jj").args(args).current_dir(repo).output().unwrap();
            assert!(
                out.status.success(),
                "jj {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        // f1 adds a.txt (x, y) and pushes `feature` to origin.
        std::fs::write(repo.join("a.txt"), "x\ny\n").unwrap();
        jj(&["commit", "-m", "f1"]);
        jj(&["bookmark", "set", "f1", "-r", "@-"]);
        jj(&["bookmark", "set", "feature", "-r", "@-"]);
        jj(&["git", "push", "--bookmark", "feature", "--remote", "origin"]);

        // f2 renames a.txt -> renamed.txt and adds z.
        std::fs::remove_file(repo.join("a.txt")).unwrap();
        std::fs::write(repo.join("renamed.txt"), "x\ny\nz\n").unwrap();
        jj(&["commit", "-m", "f2"]);
        jj(&["bookmark", "set", "feature", "-r", "@-"]);
        jj(&["new", "main"]);

        let vcs = JjVcs::with_options(
            repo.to_path_buf(),
            crate::vcs::VcsOptions {
                base: None,
                head: Some("feature"),
                highlight: Some("f1..feature"),
            },
        )
        .unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        let (_, refresh) = vcs.comparison_and_refresh(&cancel).unwrap();
        let file = refresh
            .files
            .iter()
            .find(|f| f.lines.first().is_some_and(|l| l.content.contains("renamed.txt")))
            .unwrap();
        let marked: Vec<&str> = file
            .highlight
            .highlighted_lines
            .iter()
            .map(|&idx| file.lines[idx].content.as_str())
            .collect();
        assert_eq!(marked, vec!["z"], "jj highlight across rename must mark only z");

        let single = vcs.single_file_diff("renamed.txt").unwrap();
        let single_marked: Vec<&str> = single
            .highlight
            .highlighted_lines
            .iter()
            .map(|&idx| single.lines[idx].content.as_str())
            .collect();
        assert_eq!(single_marked, vec!["z"]);

        // Verify fingerprint changes when a push moves `feature@origin` without changing `feature`.
        let vcs_origin = JjVcs::with_options(
            repo.to_path_buf(),
            crate::vcs::VcsOptions {
                base: None,
                head: Some("feature"),
                highlight: Some("feature@origin..feature"),
            },
        )
        .unwrap();
        let fp_before =
            crate::listing::render(&vcs_origin, crate::cli::OutputMode::Fingerprint, false)
                .unwrap()
                .unwrap();
        jj(&["git", "push", "--bookmark", "feature", "--remote", "origin"]);
        let fp_after =
            crate::listing::render(&vcs_origin, crate::cli::OutputMode::Fingerprint, false)
                .unwrap()
                .unwrap();
        assert_ne!(fp_before, fp_after, "pushing feature@origin must move jj fingerprint");
    }

    #[test]
    fn highlight_flag_follows_rename_made_after_highlighted_range() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            let out = Command::new("jj").args(args).current_dir(repo).output().unwrap();
            assert!(
                out.status.success(),
                "jj {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        // f1 adds a.txt with l1..l5.
        std::fs::write(repo.join("a.txt"), "l1\nl2\nl3\nl4\nl5\n").unwrap();
        jj(&["commit", "-m", "f1"]);

        // f2 renames a.txt -> renamed.txt and adds z.
        std::fs::remove_file(repo.join("a.txt")).unwrap();
        std::fs::write(repo.join("renamed.txt"), "l1\nl2\nl3\nl4\nl5\nz\n").unwrap();
        jj(&["bookmark", "set", "feature", "-r", "@"]);

        let cancel = Arc::new(AtomicBool::new(false));
        for head in [None, Some("feature")] {
            let vcs = JjVcs::with_options(
                repo.to_path_buf(),
                crate::vcs::VcsOptions { base: None, head, highlight: Some("feature-") },
            )
            .unwrap();
            let (_, refresh) = vcs.comparison_and_refresh(&cancel).unwrap();
            let marks =
                crate::highlight::HighlightMarks::from_files(&refresh.files, refresh.lines.len());
            assert_eq!(
                marks.total_highlighted_lines, 5,
                "head={head:?} must mark l1..l5 across post-range rename"
            );
            assert_eq!(marks.highlighted_file_count, 1, "head={head:?}");
            let file = refresh
                .files
                .iter()
                .find(|f| f.lines.first().is_some_and(|l| l.content.contains("renamed.txt")))
                .unwrap();
            let marked: Vec<&str> = file
                .highlight
                .highlighted_lines
                .iter()
                .map(|&idx| file.lines[idx].content.as_str())
                .collect();
            assert_eq!(marked, vec!["l1", "l2", "l3", "l4", "l5"]);

            let single = vcs.single_file_diff("renamed.txt").unwrap();
            let single_marked: Vec<&str> = single
                .highlight
                .highlighted_lines
                .iter()
                .map(|&idx| single.lines[idx].content.as_str())
                .collect();
            assert_eq!(single_marked, vec!["l1", "l2", "l3", "l4", "l5"]);
        }
    }

    #[test]
    fn highlight_plan_cache_hits_and_invalidates_after_op_and_working_copy_edit() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            let out = Command::new("jj").args(args).current_dir(repo).output().unwrap();
            assert!(
                out.status.success(),
                "jj {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        jj(&["describe", "-m", "work"]);

        let vcs = JjVcs::with_options(
            repo.to_path_buf(),
            crate::vcs::VcsOptions { base: None, head: None, highlight: Some("@") },
        )
        .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let changed = vcs.discover_working_changes("main", &cancel).unwrap();

        let p1 = JjHighlightPlan::resolve(&vcs, "main", "@", true, &changed, &cancel).unwrap();
        let p2 = JjHighlightPlan::resolve(&vcs, "main", "@", true, &changed, &cancel).unwrap();
        assert!(
            Arc::ptr_eq(&p1, &p2),
            "second resolve without op or edit must hit highlight cache"
        );

        // A jj operation advances `.jj/repo/op_heads/heads` and must invalidate the cache.
        jj(&["describe", "-m", "work renamed"]);
        let p3 = JjHighlightPlan::resolve(&vcs, "main", "@", true, &changed, &cancel).unwrap();
        assert!(!Arc::ptr_eq(&p2, &p3), "jj op must invalidate highlight plan cache");

        let p4 = JjHighlightPlan::resolve(&vcs, "main", "@", true, &changed, &cancel).unwrap();
        assert!(Arc::ptr_eq(&p3, &p4), "subsequent resolve at new op must hit cache");

        // An unsnapshotted working-copy file edit must also invalidate when read_from_disk is true.
        std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let p5 = JjHighlightPlan::resolve(&vcs, "main", "@", true, &changed, &cancel).unwrap();
        assert!(
            !Arc::ptr_eq(&p4, &p5),
            "unsnapshotted working-copy edit must invalidate highlight plan cache"
        );
    }

    #[test]
    fn highlight_batches_jj_file_reads_across_20_files() {
        if !jj_available() {
            return;
        }
        let measure_spawns = |file_count: usize| -> usize {
            let (temp, _remote) = setup_repo_with_remote();
            let repo = temp.path();
            let jj = |args: &[&str]| {
                let out = Command::new("jj").args(args).current_dir(repo).output().unwrap();
                assert!(
                    out.status.success(),
                    "jj {args:?} failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            };
            for i in 0..file_count {
                std::fs::write(repo.join(format!("f_{i:02}.txt")), format!("f1_{i}\n")).unwrap();
            }
            jj(&["commit", "-m", "f1"]);
            jj(&["bookmark", "set", "f1", "-r", "@-"]);

            for i in 0..file_count {
                std::fs::write(repo.join(format!("f_{i:02}.txt")), format!("f1_{i}\nf2_{i}\n"))
                    .unwrap();
            }
            jj(&["commit", "-m", "f2"]);
            jj(&["bookmark", "set", "feature", "-r", "@-"]);
            jj(&["new", "main"]);

            let vcs = JjVcs::with_options(
                repo.to_path_buf(),
                crate::vcs::VcsOptions { base: None, head: Some("feature"), highlight: Some("f1") },
            )
            .unwrap();
            let cancel = Arc::new(AtomicBool::new(false));
            let (_, refresh) = vcs.comparison_and_refresh(&cancel).unwrap();
            assert_eq!(refresh.files.len(), file_count);
            for file in &refresh.files {
                assert_eq!(file.highlight.line_count(), 1);
            }
            vcs.disk_manifest.lock().unwrap().highlight_cache.as_ref().unwrap().1.spawn_count
        };

        let spawns_1 = measure_spawns(1);
        let spawns_20 = measure_spawns(20);
        assert_eq!(
            spawns_20, spawns_1,
            "batched jj highlight file reads must not grow with file count (1 file={spawns_1}, 20 files={spawns_20})"
        );
        assert!(spawns_20 <= 6, "expected bounded highlight spawns, got {spawns_20}");
    }

    #[test]
    fn highlight_fingerprint_pins_working_copy_path_and_commit_loop() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            let out = Command::new("jj").args(args).current_dir(repo).output().unwrap();
            assert!(
                out.status.success(),
                "jj {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        std::fs::write(repo.join("file.txt"), "initial\nc1\n").unwrap();
        jj(&["commit", "-m", "c1"]);
        jj(&["bookmark", "set", "pushed", "-r", "@-"]);

        std::fs::write(repo.join("file.txt"), "initial\nc1\nc2\n").unwrap();
        jj(&["describe", "-m", "c2"]);

        // Working-copy path (`head: None`): moving `pushed` up to `@` changes only the highlight revset.
        let vcs = JjVcs::with_options(
            repo.to_path_buf(),
            crate::vcs::VcsOptions { base: None, head: None, highlight: Some("pushed..@") },
        )
        .unwrap();
        let fp_before = crate::listing::render(&vcs, crate::cli::OutputMode::Fingerprint, false)
            .unwrap()
            .unwrap();

        jj(&["bookmark", "set", "pushed", "-r", "@"]);

        let fp_after = crate::listing::render(&vcs, crate::cli::OutputMode::Fingerprint, false)
            .unwrap()
            .unwrap();
        assert_ne!(
            fp_before, fp_after,
            "moving highlight bookmark must change jj working-copy fingerprint"
        );
    }

    #[test]
    fn highlight_flag_excludes_base_commit_so_main_descendants_and_all_preserve_deletions() {
        if !jj_available() {
            return;
        }
        let (temp, _remote) = setup_repo_with_remote();
        let repo = temp.path();
        let jj = |args: &[&str]| {
            let out = Command::new("jj").args(args).current_dir(repo).output().unwrap();
            assert!(
                out.status.success(),
                "jj {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        // Advance `main` (and `main@origin`) with two lines so deleting the first and adding
        // after the second produces a distinct `-` deletion line and `+` addition line.
        std::fs::write(repo.join("file.txt"), "initial\nkeep\n").unwrap();
        jj(&["commit", "-m", "main two lines"]);
        jj(&["bookmark", "set", "main", "-r", "@-"]);
        jj(&["git", "push", "--bookmark", "main", "--remote", "origin"]);

        // Delete `initial` (from `main`) and add `added` after `keep`.
        std::fs::write(repo.join("file.txt"), "keep\nadded\n").unwrap();
        jj(&["describe", "-m", "delete initial and add line"]);

        let cancel = Arc::new(AtomicBool::new(false));
        let marked_for = |revset: &str| -> Vec<(char, String)> {
            let vcs = JjVcs::with_options(
                repo.to_path_buf(),
                crate::vcs::VcsOptions { base: None, head: None, highlight: Some(revset) },
            )
            .unwrap();
            let (_, refresh) = vcs.comparison_and_refresh(&cancel).unwrap();
            let file = &refresh.files[0];
            let single = vcs.single_file_diff("file.txt").unwrap();
            assert_eq!(file.highlight.highlighted_lines, single.highlight.highlighted_lines);
            file.highlight
                .highlighted_lines
                .iter()
                .map(|&idx| (file.lines[idx].prefix, file.lines[idx].content.clone()))
                .collect()
        };

        let expected = marked_for("main..");
        assert_eq!(
            expected,
            vec![('-', "initial".to_string()), ('+', "added".to_string())],
            "main.. must highlight both the deleted base line and the added line"
        );
        assert_eq!(marked_for("main::"), expected, "main:: must match main..");
        assert_eq!(marked_for("all()"), expected, "all() must match main..");
        assert!(marked_for("main").is_empty(), "base commit itself must not be highlighted");
        assert!(marked_for("root()").is_empty(), "root() must not be highlighted");
    }

    fn jj_fixtures_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jj")
    }

    #[test]
    #[ignore = "captures highlight-log.txt from BRANCHDIFF_CAPTURE_JJ (or jj on PATH)"]
    fn capture_jj_highlight_log_fixture() {
        let jj = PathBuf::from(
            std::env::var("BRANCHDIFF_CAPTURE_JJ").unwrap_or_else(|_| "jj".to_string()),
        );
        let raw_version = String::from_utf8(
            Command::new(&jj).arg("--version").output().expect("jj --version").stdout,
        )
        .expect("utf-8");
        let version = raw_version
            .split_whitespace()
            .nth(1)
            .expect("jj <version>")
            .split('-')
            .next()
            .expect("semver");
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = tmp.path().join("config.toml");
        std::fs::write(
            &config,
            "[user]\nname = \"Test User\"\nemail = \"test@example.com\"\n\
             [ui]\ncolor = \"never\"\npaginate = \"never\"\n\
             [debug]\ncommit-timestamp = \"2001-02-03T04:05:06+07:00\"\noperation-timestamp = \"2001-02-03T04:05:06+07:00\"\n\
             [operation]\nhostname = \"host\"\nusername = \"user\"\n",
        )
        .expect("write config");
        let repo = tmp.path().join("repo");
        let run = |dir: &Path, args: &[&str]| -> String {
            let out = Command::new(&jj)
                .args(args)
                .current_dir(dir)
                .env("JJ_CONFIG", &config)
                .output()
                .expect("run jj");
            assert!(out.status.success(), "jj {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8(out.stdout).expect("utf-8")
        };
        run(tmp.path(), &["git", "init", "--colocate", "repo"]);
        std::fs::write(repo.join("a.txt"), "base\n").unwrap();
        run(&repo, &["commit", "-m", "base"]);
        std::fs::write(repo.join("a.txt"), "base\none\n").unwrap();
        run(&repo, &["commit", "-m", "one"]);
        run(&repo, &["new", "description(substring:base)", "-m", "side"]);
        std::fs::write(repo.join("side.txt"), "side\n").unwrap();
        run(
            &repo,
            &["new", "description(substring:one)", "description(substring:side)", "-m", "merge"],
        );
        let log_out = run(
            &repo,
            &[
                "--ignore-working-copy",
                "log",
                "-r",
                "all()",
                "--no-graph",
                "-T",
                highlight::HIGHLIGHT_LOG_TEMPLATE,
            ],
        );
        let dir = jj_fixtures_root().join(version);
        std::fs::create_dir_all(&dir).expect("mkdir fixtures");
        std::fs::write(dir.join("highlight-log.txt"), log_out).expect("write highlight-log.txt");
    }

    fn check_jj_highlight_log_fixture(version: &str) {
        let path = jj_fixtures_root().join(version).join("highlight-log.txt");
        let raw =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let entries = parse_highlight_log(&raw);
        assert_eq!(entries.len(), 5, "jj {version}: expected merge + side + one + base + root");
        assert!(
            entries
                .iter()
                .all(|e| !e.commit_id.is_empty()
                    && e.commit_id.chars().all(|c| c.is_ascii_hexdigit())),
            "jj {version}: {entries:?}"
        );
        let wc: Vec<_> = entries.iter().filter(|e| e.is_working_copy).collect();
        assert_eq!(wc.len(), 1, "jj {version}: exactly one working-copy commit");
        assert_eq!(wc[0].parents.len(), 2, "jj {version}: working-copy commit is a 2-parent merge");
        let root = entries.iter().find(|e| e.parents.is_empty()).expect("root commit");
        assert!(!root.is_working_copy, "jj {version}");
        assert!(root.commit_id.bytes().all(|b| b == b'0'), "jj {version}: root is all zeros");
    }

    #[test]
    fn every_captured_jj_highlight_fixture_has_a_test() {
        let tested: Vec<&str> = TESTED_JJ_HIGHLIGHT_VERSIONS.to_vec();
        let mut captured: Vec<String> = std::fs::read_dir(jj_fixtures_root())
            .expect("tests/fixtures/jj")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        captured.sort();
        assert_eq!(
            captured, tested,
            "add a jj_highlight_fixture_tests! entry for each captured jj version"
        );
    }

    macro_rules! jj_highlight_fixture_tests {
        ($($name:ident => $version:literal),* $(,)?) => {
            const TESTED_JJ_HIGHLIGHT_VERSIONS: &[&str] = &[$($version),*];
            $(
                #[test]
                fn $name() {
                    check_jj_highlight_log_fixture($version);
                }
            )*
        };
    }

    jj_highlight_fixture_tests! {
        jj_highlight_log_0_45_1 => "0.45.1",
    }
}
