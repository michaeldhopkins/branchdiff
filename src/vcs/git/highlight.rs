use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};

use crate::diff::FileDiff;
use crate::highlight::{FileHighlight, HighlightSpan, compute_file_highlight_spans};

use super::changed_files::{ChangedFile, get_changed_files_between};
use super::commands::{batch_file_contents, get_file_at_ref, get_merge_base_between, rev_id};
use super::highlight_range::{clamp_range, commit_parents, is_ancestor};
use super::resolve_explicit_ref;

/// Parsed `--highlight <REVSET>` revision expression for a git repository.
#[derive(Debug, Clone)]
pub(crate) enum GitHighlightKind {
    /// `A...B`: `merge-base(A, B)..B`
    TripleDot { left: String, right: String },
    /// `A..B`: `merge-base(A, B).unwrap_or(A)..B`
    DoubleDot { left: String, right: String },
    /// `<REV>`: `<REV>^@..<REV>` (all parents of `<REV>`; root commits are excluded)
    Single { rev: String },
}

/// Validated `--highlight <REVSET>` specification for a git repository.
#[derive(Debug, Clone)]
pub(crate) struct GitHighlightSpec {
    pub label: String,
    pub kind: GitHighlightKind,
}

pub(super) fn unhighlighted(label: &str) -> FileHighlight {
    FileHighlight { label: Some(label.to_string()), ..FileHighlight::default() }
}

/// `None` when a ref no longer resolves (a deleted branch); that is the only
/// resolution failure `resolve_explicit_ref` reports.
fn resolve_pair(repo_path: &Path, left: &str, right: &str) -> Option<(String, String)> {
    let left_ref = resolve_explicit_ref(repo_path, "--highlight", left).ok()?.0;
    let right_ref = resolve_explicit_ref(repo_path, "--highlight", right).ok()?.0;
    Some((left_ref, right_ref))
}

impl GitHighlightSpec {
    /// Resolve `(from_shas, to_sha)` for this specification, clamped to
    /// `(merge_base..=effective_head]` and excluding root commits.
    /// Holds multiple SHAs when highlighting a merge commit.
    /// `Ok(None)` means a named ref is gone or the two sides share no
    /// ancestor, so nothing can be marked; any git failure is an `Err`.
    pub fn resolve_for_head(
        &self,
        repo_path: &Path,
        merge_base: &str,
        head_ref: Option<&str>,
    ) -> Result<Option<(Vec<String>, String)>> {
        let effective_head = head_ref.unwrap_or("HEAD");
        match &self.kind {
            GitHighlightKind::TripleDot { left, right } => {
                let Some((left_ref, to_ref)) = resolve_pair(repo_path, left, right) else {
                    return Ok(None);
                };
                let Ok(mb) = get_merge_base_between(repo_path, &left_ref, &to_ref) else {
                    return Ok(None);
                };
                let (from_sha, to_sha) =
                    clamp_range(repo_path, &mb, &to_ref, merge_base, effective_head)?;
                Ok(Some((vec![from_sha], to_sha)))
            }
            GitHighlightKind::DoubleDot { left, right } => {
                let Some((left_ref, to_ref)) = resolve_pair(repo_path, left, right) else {
                    return Ok(None);
                };
                let raw_from = get_merge_base_between(repo_path, &left_ref, &to_ref)
                    .ok()
                    .or_else(|| rev_id(repo_path, &left_ref))
                    .unwrap_or(left_ref);
                let (from_sha, to_sha) =
                    clamp_range(repo_path, &raw_from, &to_ref, merge_base, effective_head)?;
                Ok(Some((vec![from_sha], to_sha)))
            }
            GitHighlightKind::Single { rev } => {
                let Ok((to_ref, _)) = resolve_explicit_ref(repo_path, "--highlight", rev) else {
                    return Ok(None);
                };
                let to_sha = rev_id(repo_path, &to_ref).unwrap_or(to_ref);
                let in_window = is_ancestor(repo_path, &to_sha, effective_head)?
                    && (merge_base.is_empty() || !is_ancestor(repo_path, &to_sha, merge_base)?);
                let parents = if in_window { commit_parents(repo_path, &to_sha)? } else { vec![] };
                let from_shas = if parents.is_empty() { vec![to_sha.clone()] } else { parents };
                Ok(Some((from_shas, to_sha)))
            }
        }
    }
}

/// Resolve and validate `--highlight <REVSET>` in a git repository.
pub(crate) fn resolve_highlight_spec(repo_path: &Path, spec: &str) -> Result<GitHighlightSpec> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        anyhow::bail!("--highlight revision range cannot be empty");
    }
    let or_head = |s: &str| if s.is_empty() { "HEAD".to_string() } else { s.to_string() };
    let kind = if let Some((l, r)) = trimmed.split_once("...") {
        GitHighlightKind::TripleDot { left: or_head(l), right: or_head(r) }
    } else if let Some((l, r)) = trimmed.split_once("..") {
        GitHighlightKind::DoubleDot { left: or_head(l), right: or_head(r) }
    } else {
        GitHighlightKind::Single { rev: trimmed.to_string() }
    };
    let parsed = GitHighlightSpec { label: spec.to_string(), kind };
    // Startup reports each failure; a later refresh treats them as "nothing to mark".
    match &parsed.kind {
        GitHighlightKind::TripleDot { left, right } => {
            let (left_ref, to_ref) = (
                resolve_explicit_ref(repo_path, "--highlight", left)?.0,
                resolve_explicit_ref(repo_path, "--highlight", right)?.0,
            );
            get_merge_base_between(repo_path, &left_ref, &to_ref).with_context(|| {
                format!(
                    "--highlight {:?} has no common ancestor between {left} and {right}",
                    parsed.label
                )
            })?;
        }
        GitHighlightKind::DoubleDot { left, right } => {
            resolve_explicit_ref(repo_path, "--highlight", left)?;
            resolve_explicit_ref(repo_path, "--highlight", right)?;
        }
        GitHighlightKind::Single { rev } => {
            resolve_explicit_ref(repo_path, "--highlight", rev)?;
        }
    }
    parsed.resolve_for_head(repo_path, "", None)?;
    Ok(parsed)
}

fn post_range_renames(
    repo_path: &Path,
    to_sha: &str,
    head_ref: Option<&str>,
) -> HashMap<String, String> {
    let effective_head = head_ref.unwrap_or("HEAD");
    if rev_id(repo_path, effective_head).as_deref() == Some(to_sha) {
        return HashMap::new();
    }
    get_changed_files_between(repo_path, to_sha, effective_head)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|f| f.old_path.map(|old| (f.path, old)))
        .collect()
}

fn resolve_highlight_paths<'a>(
    changed_files: &'a HashMap<String, Option<String>>,
    post_renames: &'a HashMap<String, String>,
    file_path: &'a str,
    branch_old_path: Option<&'a str>,
) -> Option<(&'a str, &'a str)> {
    let after_candidate = post_renames
        .get(file_path)
        .map(String::as_str)
        .or_else(|| branch_old_path.and_then(|old| post_renames.get(old).map(String::as_str)))
        .unwrap_or(file_path);
    if let Some(hl_old) = changed_files.get(after_candidate) {
        return Some((hl_old.as_deref().unwrap_or(after_candidate), after_candidate));
    }
    if let Some(hl_old) = changed_files.get(file_path) {
        return Some((hl_old.as_deref().unwrap_or(file_path), file_path));
    }
    if let Some((old, hl_old)) =
        branch_old_path.and_then(|old| changed_files.get(old).map(|hl_old| (old, hl_old)))
    {
        return Some((hl_old.as_deref().unwrap_or(old), old));
    }
    None
}

/// Pre-fetched content for files touched by a git `--highlight` range during a full refresh.
pub(crate) struct GitHighlightBatch {
    pub label: String,
    parent_changes: Vec<HashMap<String, Option<String>>>,
    post_renames: HashMap<String, String>,
    parent_befores: Vec<HashMap<String, String>>,
    after_contents: HashMap<String, String>,
}

impl GitHighlightBatch {
    fn empty(label: &str) -> Self {
        Self {
            label: label.to_string(),
            parent_changes: Vec::new(),
            post_renames: HashMap::new(),
            parent_befores: Vec::new(),
            after_contents: HashMap::new(),
        }
    }

    pub fn fetch(
        repo_path: &Path,
        spec: &GitHighlightSpec,
        merge_base: &str,
        head_ref: Option<&str>,
        binary_files: &HashSet<String>,
    ) -> Result<Self> {
        // Only a ref that no longer resolves (a deleted branch) drops the marks;
        // any other git failure is returned so the refresh reports it.
        let Some((from_shas, to_sha)) = spec.resolve_for_head(repo_path, merge_base, head_ref)?
        else {
            return Ok(Self::empty(&spec.label));
        };
        if from_shas.is_empty() || (from_shas.len() == 1 && from_shas[0] == to_sha) {
            return Ok(Self::empty(&spec.label));
        }

        let (changed_lists, post_renames) = std::thread::scope(|s| {
            let handles: Vec<_> = from_shas
                .iter()
                .map(|from| {
                    let to_sha = &to_sha;
                    s.spawn(move || get_changed_files_between(repo_path, from, to_sha))
                })
                .collect();
            let post = post_range_renames(repo_path, &to_sha, head_ref);
            let lists: Vec<Result<Vec<ChangedFile>>> = handles
                .into_iter()
                .map(|h| h.join().expect("highlight changed files thread panicked"))
                .collect();
            (lists, post)
        });

        let mut parent_changes = Vec::with_capacity(changed_lists.len());
        let mut fetch_paths: Vec<String> = Vec::new();
        for changed in changed_lists {
            let changed = changed.context("Failed to get changed files for --highlight")?;
            let mut map = HashMap::with_capacity(changed.len());
            for file in changed {
                if !binary_files.contains(&file.path) {
                    fetch_paths.push(file.path.clone());
                    if let Some(old) = &file.old_path {
                        fetch_paths.push(old.clone());
                    }
                }
                map.insert(file.path, file.old_path);
            }
            parent_changes.push(map);
        }
        fetch_paths.sort_unstable();
        fetch_paths.dedup();
        let fetch_refs: Vec<&str> = fetch_paths.iter().map(String::as_str).collect();

        let (parent_befores, after_contents) = std::thread::scope(|s| {
            let before_handles: Vec<_> = from_shas
                .iter()
                .map(|from| {
                    let fetch_refs = &fetch_refs;
                    s.spawn(move || batch_file_contents(repo_path, fetch_refs, from))
                })
                .collect();
            let after = s.spawn(|| batch_file_contents(repo_path, &fetch_refs, &to_sha));
            let befores = before_handles
                .into_iter()
                .map(|h| h.join().expect("highlight before batch thread panicked"))
                .collect();
            (befores, after.join().expect("highlight after batch thread panicked"))
        });

        Ok(Self {
            label: spec.label.clone(),
            parent_changes,
            post_renames,
            parent_befores,
            after_contents,
        })
    }

    pub fn compute_for_file(
        &self,
        file_diff: &FileDiff,
        file_path: &str,
        old_path: Option<&str>,
        base_content: Option<&str>,
        working_content: Option<&str>,
    ) -> FileHighlight {
        if self.parent_changes.is_empty() {
            return unhighlighted(&self.label);
        }
        let mut befores: Vec<Option<&str>> = Vec::with_capacity(self.parent_changes.len());
        let mut after_path_resolved: Option<&str> = None;
        for (changed_map, before_map) in self.parent_changes.iter().zip(&self.parent_befores) {
            let Some((before_path, after_path)) =
                resolve_highlight_paths(changed_map, &self.post_renames, file_path, old_path)
            else {
                return unhighlighted(&self.label);
            };
            after_path_resolved = Some(after_path);
            befores.push(before_map.get(before_path).map(String::as_str));
        }
        let after =
            after_path_resolved.and_then(|p| self.after_contents.get(p).map(String::as_str));

        compute_file_highlight_spans(
            file_diff,
            base_content,
            working_content,
            &self.label,
            &[HighlightSpan { befores: &befores, after }],
        )
    }

    /// Stamp binary/image files and any untouched files after assembly so the
    /// highlight label and binary/image `touched` flag are preserved.
    pub fn stamp_assembled_files(&self, files: &mut [FileDiff]) {
        for file in files {
            if file.highlight.label.is_none() {
                file.highlight.label = Some(self.label.clone());
                if !self.parent_changes.is_empty()
                    && let Some(path) = file.lines.first().and_then(|l| l.file_path.as_deref())
                    && self.parent_changes.iter().all(|m| {
                        resolve_highlight_paths(m, &self.post_renames, path, None).is_some()
                    })
                {
                    file.highlight.touched = true;
                }
            }
        }
    }
}

/// Compute highlight provenance for a single file in `GitVcs::single_file_diff`.
pub(crate) fn compute_single_file_highlight(
    repo_path: &Path,
    spec: &GitHighlightSpec,
    merge_base: &str,
    head_ref: Option<&str>,
    file_diff: &FileDiff,
    (file_path, old_path): (&str, Option<&str>),
    (base_content, working_content): (Option<&str>, Option<&str>),
) -> Result<FileHighlight> {
    let Some((from_shas, to_sha)) = spec.resolve_for_head(repo_path, merge_base, head_ref)? else {
        return Ok(unhighlighted(&spec.label));
    };
    if from_shas.is_empty() || (from_shas.len() == 1 && from_shas[0] == to_sha) {
        return Ok(unhighlighted(&spec.label));
    }
    let post_renames = post_range_renames(repo_path, &to_sha, head_ref);

    let mut owned_befores: Vec<Option<String>> = Vec::with_capacity(from_shas.len());
    let mut after_path_owned: Option<String> = None;
    for from in &from_shas {
        let changed_map: HashMap<String, Option<String>> =
            get_changed_files_between(repo_path, from, &to_sha)
                .context("Failed to get changed files for --highlight")?
                .into_iter()
                .map(|f| (f.path, f.old_path))
                .collect();
        let Some((before_path, after_path)) =
            resolve_highlight_paths(&changed_map, &post_renames, file_path, old_path)
        else {
            return Ok(unhighlighted(&spec.label));
        };
        owned_befores.push(get_file_at_ref(repo_path, before_path, from).ok().flatten());
        after_path_owned = Some(after_path.to_string());
    }
    let after = after_path_owned
        .as_deref()
        .and_then(|after_path| get_file_at_ref(repo_path, after_path, &to_sha).ok().flatten());
    let before_refs: Vec<Option<&str>> = owned_befores.iter().map(Option::as_deref).collect();

    Ok(compute_file_highlight_spans(
        file_diff,
        base_content,
        working_content,
        &spec.label,
        &[HighlightSpan { befores: &before_refs, after: after.as_deref() }],
    ))
}
