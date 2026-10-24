//! Parsers for `jj diff` and `jj log` output.

use std::collections::HashSet;

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
