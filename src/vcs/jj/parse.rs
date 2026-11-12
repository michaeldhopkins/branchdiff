//! Parsers for `jj diff` and `jj log` output.

use std::collections::{HashMap, HashSet};

/// Changed file from `jj diff --summary` output.
#[derive(Debug, Clone)]
pub(crate) struct ChangedFile {
    pub(super) path: String,
    pub(super) old_path: Option<String>,
}

/// Parse combined `bookmarks ++ "\0" ++ change_id ++ "\0" ++ shortest_id` template output.
/// Returns `(change_id, display_label)` where label annotates bookmarks with the shortest
/// unique change ID prefix: `"knmq (main)"`.
pub(crate) fn parse_rev_metadata(raw: &str) -> (String, String) {
    let raw = raw.trim();
    let parts: Vec<&str> = raw.splitn(3, '\0').collect();
    if parts.len() >= 2 {
        let bookmarks = parts[0].trim().trim_end_matches('*');
        let change_id = parts[1].trim().to_string();
        let shortest_id = parts.get(2).map(|s| s.trim()).unwrap_or("");
        let label = if bookmarks.is_empty() {
            if shortest_id.is_empty() { change_id.clone() } else { shortest_id.to_string() }
        } else if shortest_id.is_empty() {
            bookmarks.to_string()
        } else {
            format!("{shortest_id} ({bookmarks})")
        };
        (change_id, label)
    } else {
        (raw.to_string(), raw.to_string())
    }
}

/// Parse `jj diff --summary` output into changed files.
///
/// Renames use the format `R {old_path => new_path}`.
pub(crate) fn parse_jj_summary(output: &str) -> Vec<ChangedFile> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let first = line.chars().next()?;
            if !matches!(first, 'M' | 'A' | 'D' | 'R' | 'C') {
                return None;
            }
            let rest = line[1..].trim();
            if rest.is_empty() {
                return None;
            }
            if first == 'R' {
                parse_rename(rest)
            } else {
                Some(ChangedFile { path: rest.to_string(), old_path: None })
            }
        })
        .collect()
}

/// Parse a jj `--summary` rename entry into old/new paths.
///
/// jj factors the common directory prefix (and any common suffix) *outside* the
/// brace, wrapping only the differing segment:
///   `{old => new}`            (root)
///   `sub/{old => new}`        (common prefix)
///   `app/{old => new}/f.js`   (common prefix and suffix)
/// The earlier parser anchored the brace at the start of the string and so
/// matched only the root form, silently dropping every nested rename. jj always
/// braces renames in `--summary`, so a string without a `{ … => … }` is treated
/// as malformed and skipped.
pub(crate) fn parse_rename(s: &str) -> Option<ChangedFile> {
    let open = s.find('{')?;
    let close = s.find('}')?;
    if close < open {
        return None;
    }
    let prefix = &s[..open];
    let suffix = &s[close + 1..];
    let (old, new) = s[open + 1..close].split_once(" => ")?;
    let (old, new) = (old.trim(), new.trim());
    if new.is_empty() {
        return None;
    }
    Some(ChangedFile {
        path: format!("{prefix}{new}{suffix}"),
        old_path: Some(format!("{prefix}{old}{suffix}")),
    })
}

/// Parse `jj diff --stat` output to find binary files (marked with "(binary)").
///
/// Handles renamed files: `{old => new} | (binary)` extracts just the new name.
pub(crate) fn parse_binary_from_stat(output: &str) -> HashSet<String> {
    output
        .lines()
        .filter(|line| line.contains("(binary)"))
        .filter_map(|line| {
            let raw_path = line.split('|').next()?.trim();
            if raw_path.is_empty() {
                return None;
            }
            // Renames show as "{old => new}" — extract the new name
            let path =
                if let Some(inner) = raw_path.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                    inner.split(" => ").last().unwrap_or(inner).trim()
                } else if raw_path.contains(" => ") {
                    raw_path.split(" => ").last().unwrap_or(raw_path).trim()
                } else {
                    raw_path
                };
            if path.is_empty() { None } else { Some(path.to_string()) }
        })
        .collect()
}

/// One commit row from the batched `--highlight` `jj log` template:
/// `commit_id ++ "\t" ++ if(self.contained_in("@"), "@", ".") ++ "\t" ++ parents.map(|p| p.commit_id()).join(",")`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HighlightLogEntry {
    pub(crate) commit_id: String,
    pub(crate) is_working_copy: bool,
    pub(crate) parents: Vec<String>,
}

/// Parse batched `jj log` output for `--highlight` into commit entries.
pub(crate) fn parse_highlight_log(output: &str) -> Vec<HighlightLogEntry> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let mut parts = line.splitn(3, '\t');
            let commit_id = parts.next()?.trim();
            if commit_id.is_empty() {
                return None;
            }
            let marker = parts.next()?.trim();
            let parents_raw = parts.next().unwrap_or("");
            let parents = parents_raw
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect();
            Some(HighlightLogEntry {
                commit_id: commit_id.to_string(),
                is_working_copy: marker == "@",
                parents,
            })
        })
        .collect()
}

/// Marker prefix used with `jj file show -T` to frame multiple files in one call.
pub(crate) const FILE_SHOW_MARKER: &str = "\0\0branchdiff-file:";

/// Parse batched `jj file show` output framed with [`FILE_SHOW_MARKER`].
pub(crate) fn parse_batched_file_show(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for chunk in raw.split(FILE_SHOW_MARKER).skip(1) {
        if let Some((path, content)) = chunk.split_once('\0') {
            out.insert(path.replace('\\', "/"), content.to_string());
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SpanTarget {
    Revision(String),
    WorkingCopy,
}

fn target_for(entry: &HighlightLogEntry, read_from_disk: bool) -> SpanTarget {
    if read_from_disk && entry.is_working_copy {
        SpanTarget::WorkingCopy
    } else {
        SpanTarget::Revision(entry.commit_id.clone())
    }
}

pub(super) fn coalesce_highlight_entries(
    entries: &[HighlightLogEntry],
    read_from_disk: bool,
) -> Vec<(Vec<String>, SpanTarget)> {
    let entries: Vec<&HighlightLogEntry> = entries.iter().collect();
    if entries.is_empty() {
        return Vec::new();
    }
    let in_set: HashSet<&str> = entries.iter().map(|e| e.commit_id.as_str()).collect();
    let mut child_count: HashMap<&str, usize> = HashMap::new();
    let mut external_parents: Vec<&str> = Vec::new();
    for entry in &entries {
        for p in &entry.parents {
            if in_set.contains(p.as_str()) {
                *child_count.entry(p.as_str()).or_insert(0) += 1;
            } else if !external_parents.contains(&p.as_str()) {
                external_parents.push(p.as_str());
            }
        }
    }
    let heads: Vec<&HighlightLogEntry> = entries
        .iter()
        .copied()
        .filter(|e| child_count.get(e.commit_id.as_str()).copied().unwrap_or(0) == 0)
        .collect();
    if heads.len() == 1 && external_parents.len() <= 1 {
        let from_revs = external_parents.into_iter().map(String::from).collect();
        return vec![(from_revs, target_for(heads[0], read_from_disk))];
    }
    let mut raw_spans: Vec<(Vec<String>, SpanTarget)> = Vec::new();
    for entry in entries.iter().rev() {
        let next_target = target_for(entry, read_from_disk);
        if entry.parents.len() == 1 {
            let parent = &entry.parents[0];
            if in_set.contains(parent.as_str())
                && child_count.get(parent.as_str()).copied().unwrap_or(0) == 1
                && let Some((_, to_target)) = raw_spans.iter_mut().find(|(from_revs, to)| {
                    from_revs.len() <= 1 && matches!(to, SpanTarget::Revision(rev) if rev == parent)
                })
            {
                *to_target = next_target;
                continue;
            }
        }
        raw_spans.push((entry.parents.clone(), next_target));
    }
    raw_spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rename_is_none_when_the_brace_closes_first() {
        assert!(parse_rename("a}b{c").is_none());
        let ok = parse_rename("sub/{old => new}/f.js").unwrap();
        assert_eq!(ok.path, "sub/new/f.js");
        assert_eq!(ok.old_path.as_deref(), Some("sub/old/f.js"));
    }

    /// `{` and `}` are different bytes, so `find` cannot return one index for
    /// both. `close <= open` therefore agrees with `close < open` on every
    /// string. The walk is the finite check; the byte inequality is the proof.
    #[test]
    fn opening_and_closing_braces_never_share_an_index() {
        assert_ne!(b'{', b'}');
        let mut buf = String::new();
        brace_strings(4, &['a', '{', '}', 'x'], &mut buf);
    }

    fn brace_strings(remaining: usize, alphabet: &[char], buf: &mut String) {
        if let (Some(open), Some(close)) = (buf.find('{'), buf.find('}')) {
            assert_ne!(open, close, "{buf:?}");
        }
        if remaining == 0 {
            return;
        }
        for ch in alphabet {
            buf.push(*ch);
            brace_strings(remaining - 1, alphabet, buf);
            buf.pop();
        }
    }
}
