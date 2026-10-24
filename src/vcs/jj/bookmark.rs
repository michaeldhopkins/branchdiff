//! Bookmark boundary resolution and per-line bookmark provenance for jj stacks.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vcs_runner::run_jj_cancellable;

use super::{is_trunk_base, no_snapshot, remote_of};

/// Build the revset that finds the bookmark boundary below `change_id`.
///
/// Remote bookmarks are scoped to `trunk_remote`. An unscoped
/// `remote_bookmarks()` matches *every* remote, and because `latest()` picks by
/// committer timestamp rather than by remote precedence, a bookmark on a deploy
/// remote (heroku etc.) sitting inside the stack range can displace the real
/// stack segment and truncate the current bookmark's file scope. With no known
/// trunk remote, consider local bookmarks only.
pub(super) fn boundary_revset(
    change_id: &str,
    from_rev: &str,
    trunk_remote: Option<&str>,
) -> String {
    let bookmarks = match trunk_remote {
        Some(remote) => format!(r#"bookmarks() | remote_bookmarks(remote=exact:"{remote}")"#),
        None => "bookmarks()".to_string(),
    };
    format!("latest((({from_rev})..\"{change_id}\"-) & ({bookmarks}))")
}

/// Info about the bookmark boundary for the current stack position.
pub(super) struct BookmarkBoundary {
    /// Name of the current bookmark.
    pub(super) bookmark_name: String,
    /// Files changed between the boundary and target rev, used to filter which files
    /// belong to the current bookmark vs earlier bookmarks.
    pub(super) changed_files: HashSet<String>,
}

#[cfg(test)]
pub(super) fn resolve_bookmark_boundary(
    repo_path: &Path,
    from_rev: &str,
    cancel: &Arc<AtomicBool>,
) -> Option<BookmarkBoundary> {
    resolve_bookmark_boundary_for(repo_path, from_rev, "@", cancel)
}

/// Find the bookmark boundary: the nearest ancestor bookmark below the target
/// revision's bookmark scope. Returns the boundary revision and current bookmark name.
///
/// Strategy:
/// 1. Find the bookmark at or above `target_rev` (the bookmark it belongs to).
/// 2. Find the nearest ancestor bookmark below that one.
pub(super) fn resolve_bookmark_boundary_for(
    repo_path: &Path,
    from_rev: &str,
    target_rev: &str,
    cancel: &Arc<AtomicBool>,
) -> Option<BookmarkBoundary> {
    if !is_trunk_base(from_rev) {
        return None;
    }

    // Step 1: Find the current bookmark — at target_rev or the nearest descendant with a bookmark
    let current_bm_revset = format!("latest((({target_rev}):: | ({target_rev})) & bookmarks())");
    let template = r#"bookmarks.join(",") ++ "\0" ++ change_id.short(12)"#;
    let args = no_snapshot(&[
        "log", "-r", &current_bm_revset, "--no-graph", "--limit", "1", "-T", template,
    ]);
    let output = run_jj_cancellable(repo_path, &args, Arc::clone(cancel)).ok()?;
    let stdout = output.stdout_lossy();
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }

    let parts: Vec<&str> = trimmed.splitn(2, '\0').collect();
    if parts.len() < 2 {
        return None;
    }
    let bookmark_name = parts[0].trim().trim_end_matches('*').to_string();
    let current_bm_id = parts[1].trim().to_string();

    if bookmark_name.is_empty() {
        return None;
    }

    // Step 2: Find the previous bookmark (nearest ancestor bookmark below the current one).
    // Local bookmarks plus trunk's remote — stacks often have only remote-tracking
    // bookmarks for already-pushed segments, but other remotes must not participate.
    let remote = remote_of(repo_path, from_rev, cancel);
    let revset = boundary_revset(&current_bm_id, from_rev, remote.as_deref());
    let prev_args = no_snapshot(&[
        "log", "-r", &revset, "--no-graph", "--limit", "1", "-T", "change_id.short(12)",
    ]);
    let boundary_id = run_jj_cancellable(repo_path, &prev_args, Arc::clone(cancel))
        .ok()
        .map(|o| o.stdout_lossy().trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| from_rev.to_string());

    // Step 3: Get the list of files changed between boundary and target_rev
    let range = format!("\"{boundary_id}\"..({target_rev})");
    let diff_args = no_snapshot(&["diff", "-r", &range, "--name-only"]);
    let changed_files: HashSet<String> =
        run_jj_cancellable(repo_path, &diff_args, Arc::clone(cancel))
            .map(|o| {
                o.stdout_lossy()
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            })
            .unwrap_or_default();

    Some(BookmarkBoundary { bookmark_name, changed_files })
}

/// Mark each line's bookmark provenance based on whether the file was changed
/// in the current bookmark's scope (between the previous bookmark and target rev).
pub(super) fn mark_bookmark_provenance(
    file_diff: &mut crate::diff::FileDiff,
    file_in_current_bookmark: bool,
) {
    use crate::diff::LineSource;

    for line in &mut file_diff.lines {
        line.in_current_bookmark = Some(match line.source {
            LineSource::Committed
            | LineSource::CanceledCommitted
            | LineSource::DeletedBase
            | LineSource::Staged
            | LineSource::DeletedCommitted
            | LineSource::CanceledStaged
            | LineSource::Unstaged
            | LineSource::DeletedStaged => file_in_current_bookmark,

            LineSource::Base if line.change_source.is_some() => file_in_current_bookmark,

            LineSource::Base | LineSource::FileHeader | LineSource::Elided => false,
        });
    }
}
