//! Fork point, upstream divergence, and the mutable stack tip above `@`.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vcs_runner::{run_jj, run_jj_cancellable};

use super::{is_trunk_base, no_snapshot, parse_jj_summary};
use crate::vcs::UpstreamDivergence;

/// Find the fork point: the most recent common ancestor of trunk() and @.
/// Returns None if @ is already on top of trunk (no divergence) or if
/// from_rev isn't trunk().
pub(super) fn resolve_fork_point(
    repo_path: &Path,
    from_rev: &str,
    cancel: &Arc<AtomicBool>,
) -> Option<String> {
    if !is_trunk_base(from_rev) {
        return None;
    }

    let fork_revset = format!("heads(::({from_rev}) & ::@)");
    let args = no_snapshot(&[
        "log", "-r", &fork_revset, "--no-graph", "--limit", "1", "-T", "commit_id.short(12)",
    ]);
    let fork_id = run_jj_cancellable(repo_path, &args, Arc::clone(cancel))
        .ok()
        .map(|o| o.stdout_lossy().trim().to_string())
        .filter(|s| !s.is_empty())?;

    // Check if fork point equals trunk tip — if so, no divergence
    let trunk_commit = {
        let args = no_snapshot(&[
            "log", "-r", from_rev, "--no-graph", "--limit", "1", "-T", "commit_id.short(12)",
        ]);
        run_jj_cancellable(repo_path, &args, Arc::clone(cancel))
            .ok()
            .map(|o| o.stdout_lossy().trim().to_string())?
    };

    if fork_id == trunk_commit {
        // @ is already on top of trunk — no divergence
        return None;
    }

    Some(fork_id)
}

/// Compute upstream divergence for jj: how many commits and which files
/// changed between the fork point and trunk().
pub(super) fn compute_jj_divergence(
    repo_path: &Path,
    fork_point: &str,
    from_rev: &str,
    cancel: &Arc<AtomicBool>,
) -> Option<UpstreamDivergence> {
    // Count commits between fork point and trunk
    let revset = format!("\"{fork_point}\"..({from_rev})");
    let count_args = no_snapshot(&["log", "-r", &revset, "--no-graph", "-T", r#""\n""#]);
    let behind_count = run_jj_cancellable(repo_path, &count_args, Arc::clone(cancel))
        .map(|o| o.stdout_lossy().lines().filter(|l| !l.is_empty()).count())
        .unwrap_or(0);

    if behind_count == 0 {
        return None;
    }

    // Get files changed between fork point and trunk
    let diff_args = no_snapshot(&["diff", "--from", fork_point, "--to", from_rev, "--summary"]);
    let upstream_files = run_jj_cancellable(repo_path, &diff_args, Arc::clone(cancel))
        .map(|o| parse_jj_summary(&o.stdout_lossy()).into_iter().map(|f| f.path).collect())
        .unwrap_or_default();

    Some(UpstreamDivergence { behind_count, upstream_files })
}

/// Info about the stack tip when @ is not at the top.
#[derive(Debug)]
pub(super) struct StackTip {
    /// Short change_id of the chosen tip commit.
    pub(super) change_id: String,
    /// How many independent heads descend from @ (1 = linear stack).
    pub(super) head_count: usize,
}

/// Find the tip(s) of the mutable stack above @.
/// Returns None when @ is already the tip or from_rev isn't trunk().
pub(super) fn resolve_stack_tip(
    repo_path: &Path,
    from_rev: &str,
    cancel: &Arc<AtomicBool>,
) -> Option<StackTip> {
    if !is_trunk_base(from_rev) {
        return None;
    }

    let tip_revset = format!("heads(({from_rev})..(@::))");
    let args = no_snapshot(&[
        "log",
        "-r",
        &tip_revset,
        "--no-graph",
        "-T",
        r#"change_id.short(12) ++ "\n""#,
    ]);
    let output = run_jj_cancellable(repo_path, &args, Arc::clone(cancel)).ok()?;
    let stdout = output.stdout_lossy();
    let heads: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();

    if heads.is_empty() {
        return None;
    }

    // Check if the only head IS @ (meaning @ is already at the tip)
    let at_id = get_change_id_static(repo_path, "@", cancel)?;
    if heads.len() == 1 && heads[0].trim() == at_id.trim() {
        return None;
    }

    Some(StackTip { change_id: heads[0].trim().to_string(), head_count: heads.len() })
}

/// Recover a useful tip when `@` is an empty commit orphaned by abandoning the
/// commit you were editing. In that case jj moves `@` to a fresh empty commit on
/// the abandoned commit's parent and rebases the remainder as a *sibling* line,
/// so `resolve_stack_tip` (which only walks `@`'s own descendants) finds nothing
/// and the diff collapses to `@`'s empty range. Follow to the most recent
/// non-empty leaf on the sibling line instead.
///
/// Returns `None` in every normal case. The discriminator is "`@` is empty AND
/// its parent has another child": a normal working copy — whether on a linear
/// stack or one of several independent stacks — is its parent's only child, so
/// there is no sibling and we never navigate away from a deliberate `@`.
pub(super) fn resolve_orphaned_tip(
    repo_path: &Path,
    from_rev: &str,
    cancel: &Arc<AtomicBool>,
) -> Option<StackTip> {
    if !is_trunk_base(from_rev) {
        return None;
    }

    // Cheap bail for the common case: `(@-)+ ~ @` is the set of `@`'s siblings,
    // which is empty for any ordinary working copy. This keeps the extra jj
    // calls below off the hot path.
    let siblings = run_jj_cancellable(
        repo_path,
        &no_snapshot(&["log", "-r", "(@-)+ ~ @", "--no-graph", "--limit", "1", "-T", r#""x""#]),
        Arc::clone(cancel),
    )
    .ok()?;
    if siblings.stdout_lossy().trim().is_empty() {
        return None;
    }

    // Only re-anchor when `@` itself carries no work — a non-empty `@` with a
    // sibling is deliberate forked development we must not jump away from.
    // This check deliberately snapshots the working copy (no `--ignore-working-copy`):
    // `empty()` reflects committed state, so uncommitted edits in `@` would
    // otherwise read as empty and we'd wrongly navigate away from them. Safe to
    // snapshot here because we only reach this point in the rare sibling case.
    let at_empty = run_jj_cancellable(
        repo_path,
        &["log", "-r", "@ & empty()", "--no-graph", "--limit", "1", "-T", r#""x""#],
        Arc::clone(cancel),
    )
    .ok()?;
    if at_empty.stdout_lossy().trim().is_empty() {
        return None;
    }

    // Most recent non-empty leaf descending from `@`'s parent, excluding `@`'s
    // own (empty) line — i.e. the rebased remainder of the abandoned stack.
    //
    // `~ working_copies() ~ ::working_copies()` excludes *other workspaces*: when
    // several jj workspaces branch from the same trunk commit, that commit is the
    // shared parent, so every sibling workspace's `@` (and the work beneath it)
    // would otherwise land in the candidate set and a fresh, empty workspace would
    // show another workspace's diff. A genuinely abandoned tip in *this* workspace
    // is neither a working copy nor an ancestor of one, so it survives.
    let args = no_snapshot(&[
        "log",
        "-r",
        "latest((heads((@-)::) ~ (@::) ~ working_copies() ~ ::working_copies()) ~ empty())",
        "--no-graph",
        "--limit",
        "1",
        "-T",
        "change_id.short(12)",
    ]);
    let id = run_jj_cancellable(repo_path, &args, Arc::clone(cancel))
        .ok()
        .map(|o| o.stdout_lossy().trim().to_string())
        .filter(|s| !s.is_empty())?;

    // head_count is unused for an orphaned tip (compute_stack_position returns
    // None because `@` is not an ancestor of the sibling tip), so 1 is fine.
    Some(StackTip { change_id: id, head_count: 1 })
}

/// Get a change_id without requiring a JjVcs instance.
pub(super) fn get_change_id_static(
    repo_path: &Path,
    rev: &str,
    cancel: &Arc<AtomicBool>,
) -> Option<String> {
    let args =
        no_snapshot(&["log", "-r", rev, "--no-graph", "--limit", "1", "-T", "change_id.short(12)"]);
    run_jj_cancellable(repo_path, &args, Arc::clone(cancel))
        .ok()
        .map(|o| o.stdout_lossy().trim().to_string())
}

/// Compute @'s position in the stack from trunk to tip.
/// Returns (1-based position of @, total commits in stack).
pub(super) fn compute_stack_position(
    repo_path: &Path,
    tip_id: &str,
    from_rev: &str,
) -> Option<(usize, usize)> {
    let revset = format!("({from_rev})..\"{tip_id}\"");
    let args = no_snapshot(&[
        "log",
        "-r",
        &revset,
        "--no-graph",
        "-T",
        r#"if(self.contained_in("@"), "@", ".") ++ "\n""#,
    ]);
    let output = run_jj(repo_path, &args).ok()?;
    let stdout = output.stdout_lossy();
    let entries: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    let total = entries.len();
    if total == 0 {
        return None;
    }

    // jj log outputs newest-first; @ marker tells us position
    // Position from bottom: total - index_from_top
    let index_from_top = entries.iter().position(|e| e.trim() == "@")?;
    let current = total - index_from_top;

    Some((current, total))
}

/// What a refresh needs to know about the commits around `@` before it can
/// list changed files.
pub(super) struct Anchors<B> {
    pub(super) fork_point: Option<String>,
    pub(super) divergence: Option<UpstreamDivergence>,
    pub(super) stack_tip: Option<StackTip>,
    /// The sibling tip standing in for an empty orphaned `@`; looked for only
    /// when `@` has no stack above it.
    pub(super) orphaned: Option<StackTip>,
    pub(super) bookmark_boundary: Option<B>,
}

/// Resolves the fork point (then divergence), the stack tip (then the
/// orphaned tip) and the bookmark boundary at once: the three chains are
/// independent read-only jj calls, each about 15 ms, run serially before.
pub(super) fn resolve_anchors<B: Send>(
    repo_path: &Path,
    from_rev: &str,
    cancel: &Arc<AtomicBool>,
    bookmark_boundary: impl FnOnce() -> Option<B> + Send,
) -> Anchors<B> {
    std::thread::scope(|scope| {
        let fork = scope.spawn(|| {
            let fork_point = resolve_fork_point(repo_path, from_rev, cancel);
            let divergence = fork_point
                .as_deref()
                .and_then(|fp| compute_jj_divergence(repo_path, fp, from_rev, cancel));
            (fork_point, divergence)
        });
        let stack = scope.spawn(|| {
            let stack_tip = resolve_stack_tip(repo_path, from_rev, cancel);
            let orphaned = match stack_tip {
                None => resolve_orphaned_tip(repo_path, from_rev, cancel),
                Some(_) => None,
            };
            (stack_tip, orphaned)
        });
        let bookmark_boundary = bookmark_boundary();
        let (fork_point, divergence) = joined(fork);
        let (stack_tip, orphaned) = joined(stack);
        Anchors { fork_point, divergence, stack_tip, orphaned, bookmark_boundary }
    })
}

pub(super) fn joined<T>(handle: std::thread::ScopedJoinHandle<'_, T>) -> T {
    handle.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}
