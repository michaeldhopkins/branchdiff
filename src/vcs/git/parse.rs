//! Pure parsers for git's stdout, kept apart from the code that runs git so they
//! can be unit-tested and fuzzed without a repository. git's output is input
//! branchdiff does not control: a git upgrade, a config setting or a filename can
//! change its shape.

use std::collections::{HashMap, HashSet};
use std::io::BufRead;

/// One line of `git status --porcelain=v1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusEntry {
    /// The path as shown, after a staged rename's `old -> new` is split.
    pub path: String,
    /// The source of a staged rename.
    pub old_path: Option<String>,
    /// The path column exactly as git printed it, before rename splitting.
    pub raw_path: String,
    /// Second status column is `D`: deleted in the working tree.
    pub worktree_deleted: bool,
    /// Status `??`.
    pub untracked: bool,
}

pub(crate) fn parse_status_porcelain(output: &str) -> Vec<StatusEntry> {
    let mut entries = Vec::new();
    for line in output.lines() {
        if line.len() < 3 {
            continue;
        }

        let status_codes = &line[..2];
        let raw_path = line[3..].to_string();

        let worktree_deleted = status_codes.as_bytes()[1] == b'D';
        let untracked = !worktree_deleted && status_codes == "??";

        let (path, old_path) = if raw_path.contains(" -> ") {
            let parts: Vec<&str> = raw_path.split(" -> ").collect();
            (parts[1].to_string(), Some(parts[0].to_string()))
        } else {
            (raw_path.clone(), None)
        };

        entries.push(StatusEntry { path, old_path, raw_path, worktree_deleted, untracked });
    }
    entries
}

/// Binary files in `git diff --numstat` output, which shows them as
/// `-\t-\t<path>` (or `-\t-\told => new` for a rename, keeping the new name).
pub(crate) fn parse_numstat_binaries(output: &str) -> HashSet<String> {
    let mut binaries = HashSet::new();
    for line in output.lines() {
        if let Some(path) = line.strip_prefix("-\t-\t") {
            let actual_path = if path.contains(" => ") {
                path.split(" => ").last().unwrap_or(path)
            } else {
                path
            };
            binaries.insert(actual_path.to_string());
        }
    }
    binaries
}

/// Read `git cat-file --batch` responses, one per requested path, in order.
///
/// - Blob: `<sha> blob <size>\n<content>\n`
/// - Non-blob (tree/commit/tag): `<sha> <type> <size>\n<content>\n` — skipped
/// - Missing/ambiguous/submodule: `<spec> missing\n` (2-field) — skipped
///
/// Returns path to content for each blob. Stops at the first short read.
pub(crate) fn read_cat_file_batch<R: BufRead>(
    reader: &mut R,
    file_paths: &[&str],
) -> HashMap<String, String> {
    let mut results = HashMap::with_capacity(file_paths.len());
    let mut header_line = String::new();

    for &path in file_paths {
        header_line.clear();
        if reader.read_line(&mut header_line).unwrap_or(0) == 0 {
            break;
        }
        let header = header_line.trim_end();

        // Success format: "<sha> <type> <size>" (3 fields).
        // Error formats have 2 fields: "<spec> missing", "<spec> ambiguous", etc.
        let parts: Vec<&str> = header.splitn(4, ' ').collect();
        if parts.len() < 3 {
            continue;
        }

        let obj_type = parts[1];
        let size: usize = match parts[2].parse() {
            Ok(n) => n,
            Err(_) => continue,
        };

        // Read the content bytes + trailing LF regardless of object type,
        // to keep the stream in sync for subsequent responses.
        let mut content_buf = vec![0u8; size];
        if reader.read_exact(&mut content_buf).is_err() {
            break;
        }
        let mut trailing = [0u8; 1];
        let _ = reader.read_exact(&mut trailing);

        if obj_type == "blob" {
            results.insert(
                path.to_string(),
                String::from_utf8_lossy(&content_buf).into_owned(),
            );
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_splits_a_staged_rename() {
        let entries = parse_status_porcelain("R  old.rs -> new.rs\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "new.rs");
        assert_eq!(entries[0].old_path.as_deref(), Some("old.rs"));
        assert!(!entries[0].worktree_deleted && !entries[0].untracked);
    }

    #[test]
    fn status_flags_worktree_deletions_and_untracked_files() {
        let entries = parse_status_porcelain(" D gone.rs\n?? new.rs\nM  kept.rs\nx\n");
        assert_eq!(entries.len(), 3, "a line shorter than the status columns is skipped");
        assert!(entries[0].worktree_deleted && !entries[0].untracked);
        assert!(entries[1].untracked && !entries[1].worktree_deleted);
        assert!(!entries[2].worktree_deleted && !entries[2].untracked);
        assert_eq!(entries[2].path, "kept.rs");
    }

    #[test]
    fn numstat_keeps_only_binaries_and_a_renames_new_name() {
        let found = parse_numstat_binaries("-\t-\timg.png\n3\t1\ttext.rs\n-\t-\ta.bin => b.bin\n");
        assert_eq!(found, HashSet::from(["img.png".to_string(), "b.bin".to_string()]));
    }

    #[test]
    fn cat_file_batch_keeps_blobs_and_skips_missing_and_trees() {
        let stream = b"abc blob 5\nhello\nabc missing\ndef tree 3\nxyz\nfed blob 0\n\n";
        let got = read_cat_file_batch(&mut &stream[..], &["a", "b", "c", "d"]);
        assert_eq!(got.len(), 2);
        assert_eq!(got["a"], "hello");
        assert_eq!(got["d"], "");
    }

    #[test]
    fn cat_file_batch_stops_at_a_truncated_body() {
        let got = read_cat_file_batch(&mut &b"abc blob 10\nshort"[..], &["a", "b"]);
        assert!(got.is_empty());
    }
}
