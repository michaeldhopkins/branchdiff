//! The machine-readable list of files in the diff, with status and line counts.

use std::fmt::Write as _;

use crate::diff::{FileDiff, LineSource};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl FileStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            FileStatus::Added => "added",
            FileStatus::Modified => "modified",
            FileStatus::Deleted => "deleted",
            FileStatus::Renamed => "renamed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    /// Where a renamed file came from.
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub additions: usize,
    pub deletions: usize,
}

/// The files in the diff plus the ids it was taken between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileList {
    pub base: String,
    pub head: String,
    pub files: Vec<FileEntry>,
}

/// Classify a file by what its diff rows hold: nothing but additions is a new
/// file, nothing but deletions a removed one. Renames are known from the path.
fn status_of(is_rename: bool, additions: usize, deletions: usize, other_rows: usize) -> FileStatus {
    if is_rename {
        FileStatus::Renamed
    } else if additions > 0 && deletions == 0 && other_rows == 0 {
        FileStatus::Added
    } else if deletions > 0 && additions == 0 && other_rows == 0 {
        FileStatus::Deleted
    } else {
        FileStatus::Modified
    }
}

/// Entries for the files of a refresh. A file with no rows besides its header
/// is left out unless it is a rename, as in every other output.
pub fn entries(files: &[FileDiff]) -> Vec<FileEntry> {
    files
        .iter()
        .filter_map(|file| {
            let header = file.lines.first().filter(|l| l.source == LineSource::FileHeader)?;
            let rows = file.lines.iter().filter(|l| l.source != LineSource::FileHeader);
            let (mut additions, mut deletions, mut other) = (0, 0, 0);
            for row in rows {
                if row.is_addition() {
                    additions += 1;
                } else if row.is_deletion() {
                    deletions += 1;
                } else {
                    other += 1;
                }
            }
            if additions + deletions + other == 0 && !file.is_rename() {
                return None;
            }
            let path = match &file.old_path {
                Some(old) => header
                    .content
                    .strip_prefix(&format!("{old} → "))
                    .unwrap_or(&header.content)
                    .to_string(),
                None => header.content.clone(),
            };
            Some(FileEntry {
                path,
                old_path: file.old_path.clone(),
                status: status_of(file.is_rename(), additions, deletions, other),
                additions,
                deletions,
            })
        })
        .collect()
}

/// A JSON string literal. Control characters, quotes and backslashes are
/// escaped; everything else (including non-ASCII) passes through as UTF-8.
pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Tab-separated rows, one per file: status, `+additions`, `-deletions`, path.
/// Two comment lines first carry the ids. Names with tabs or newlines are not
/// escaped here; `--json` is the format for those.
pub fn render_text(list: &FileList) -> String {
    let mut out = format!("# base {}\n# head {}\n", list.base, list.head);
    for f in &list.files {
        let _ = writeln!(out, "{}\t+{}\t-{}\t{}", f.status.as_str(), f.additions, f.deletions, f.path);
    }
    out
}

pub fn render_json(list: &FileList) -> String {
    let mut out = format!(
        "{{\"base\":{},\"head\":{},\"files\":[",
        json_string(&list.base),
        json_string(&list.head)
    );
    for (i, f) in list.files.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let old = f.old_path.as_deref().map_or_else(|| "null".to_string(), json_string);
        let _ = write!(
            out,
            "{{\"path\":{},\"old_path\":{},\"status\":\"{}\",\"additions\":{},\"deletions\":{}}}",
            json_string(&f.path),
            old,
            f.status.as_str(),
            f.additions,
            f.deletions
        );
    }
    out.push_str("]}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{compute_four_way_diff, DiffInput};
    use proptest::prelude::*;

    fn diff(path: &str, base: Option<&str>, head: Option<&str>, working: Option<&str>, old: Option<&str>) -> FileDiff {
        compute_four_way_diff(DiffInput {
            path,
            base,
            head,
            index: working,
            working,
            old_path: old,
        })
    }

    #[test]
    fn a_new_file_is_added_with_its_line_count() {
        let e = entries(&[diff("n.txt", None, None, Some("a\nb\n"), None)]);
        assert_eq!(e.len(), 1);
        assert_eq!((e[0].status, e[0].additions, e[0].deletions), (FileStatus::Added, 2, 0));
        assert_eq!(e[0].path, "n.txt");
    }

    #[test]
    fn a_removed_file_is_deleted() {
        let e = entries(&[diff("d.txt", Some("a\nb\n"), Some("a\nb\n"), None, None)]);
        assert_eq!(e.len(), 1);
        assert_eq!((e[0].status, e[0].additions, e[0].deletions), (FileStatus::Deleted, 0, 2));
    }

    #[test]
    fn an_edited_file_is_modified_with_both_counts() {
        let e = entries(&[diff("m.txt", Some("a\nb\nc\n"), Some("a\nb\nc\n"), Some("a\nB\nc\nd\n"), None)]);
        assert_eq!(e[0].status, FileStatus::Modified);
        assert_eq!((e[0].additions, e[0].deletions), (2, 1));
    }

    #[test]
    fn a_pure_rename_keeps_both_paths_and_zero_counts() {
        let e = entries(&[diff("new.txt", Some("a\n"), Some("a\n"), Some("a\n"), Some("old.txt"))]);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].status, FileStatus::Renamed);
        assert_eq!(e[0].path, "new.txt");
        assert_eq!(e[0].old_path.as_deref(), Some("old.txt"));
    }

    fn list() -> FileList {
        FileList {
            base: "b1".into(),
            head: "h1".into(),
            files: vec![FileEntry {
                path: "a \"q\".rs".into(),
                old_path: None,
                status: FileStatus::Modified,
                additions: 3,
                deletions: 1,
            }],
        }
    }

    #[test]
    fn text_has_the_ids_then_one_row_per_file() {
        assert_eq!(render_text(&list()), "# base b1\n# head h1\nmodified\t+3\t-1\ta \"q\".rs\n");
    }

    #[test]
    fn json_has_ids_and_every_field() {
        assert_eq!(
            render_json(&list()),
            "{\"base\":\"b1\",\"head\":\"h1\",\"files\":[{\"path\":\"a \\\"q\\\".rs\",\"old_path\":null,\"status\":\"modified\",\"additions\":3,\"deletions\":1}]}\n"
        );
    }

    #[test]
    fn an_empty_diff_is_an_empty_array() {
        let l = FileList { base: "b".into(), head: "h".into(), files: vec![] };
        assert_eq!(render_json(&l), "{\"base\":\"b\",\"head\":\"h\",\"files\":[]}\n");
        assert_eq!(render_text(&l), "# base b\n# head h\n");
    }

    proptest! {
        /// Oracle: serde_json parses what we write back to the same string.
        #[test]
        fn json_strings_roundtrip(s in "\\PC{0,40}|[\\x00-\\x1f\"\\\\a-z]{0,20}") {
            let parsed: String = serde_json::from_str(&json_string(&s)).unwrap();
            prop_assert_eq!(parsed, s);
        }

        #[test]
        fn the_json_list_always_parses_and_keeps_every_file(
            paths in proptest::collection::vec("\\PC{1,20}", 0..6),
            adds in 0usize..1000,
        ) {
            let l = FileList {
                base: "b".into(),
                head: "h".into(),
                files: paths.iter().map(|p| FileEntry {
                    path: p.clone(), old_path: Some(p.clone()), status: FileStatus::Renamed,
                    additions: adds, deletions: 0,
                }).collect(),
            };
            let v: serde_json::Value = serde_json::from_str(&render_json(&l)).unwrap();
            let files = v["files"].as_array().unwrap();
            prop_assert_eq!(files.len(), paths.len());
            for (f, p) in files.iter().zip(&paths) {
                prop_assert_eq!(f["path"].as_str().unwrap(), p.as_str());
                prop_assert_eq!(f["additions"].as_u64().unwrap() as usize, adds);
            }
        }
    }
}
