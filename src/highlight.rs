//! Commit-range highlight provenance (`--highlight <REVSET>`) and rendering helpers.
//!
//! Marks the lines and hunks in a branch diff that were introduced by a subset
//! of commits (such as unpushed commits `feature@origin..feature`), while
//! keeping `DiffLine`'s memory layout and hot-path allocation counts unchanged
//! when `--highlight` is not active.

use std::collections::{HashMap, HashSet};

use crate::diff::provenance::build_provenance_map;
use crate::diff::{ChangeBlock, DiffLine, FileDiff, LineSource};

/// Highlight provenance for a single [`FileDiff`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileHighlight {
    /// Label of the active `--highlight` revset, when `--highlight` was passed.
    pub label: Option<String>,
    /// Whether the highlighted commit(s) modified this file at all.
    pub touched: bool,
    /// Sorted 0-based indices into `FileDiff::lines` introduced by the highlighted commit(s).
    pub highlighted_lines: Vec<usize>,
    /// Sorted 0-based indices into `FileDiff::lines` belonging to a hunk (`ChangeBlock`)
    /// that contains at least one highlighted line.
    pub highlighted_hunk_lines: Vec<usize>,
}

impl FileHighlight {
    /// True if `--highlight` is active on this file.
    pub fn is_active(&self) -> bool {
        self.label.is_some() || self.is_highlighted()
    }

    /// True if this file has any highlighted lines or was touched by the highlighted commit(s).
    pub fn is_highlighted(&self) -> bool {
        self.touched || !self.highlighted_lines.is_empty()
    }

    /// Number of highlighted diff lines in this file.
    pub fn line_count(&self) -> usize {
        self.highlighted_lines.len()
    }

    /// True if the diff line at `line_idx` was introduced by the highlighted commit(s).
    pub fn is_line_highlighted(&self, line_idx: usize) -> bool {
        self.highlighted_lines.binary_search(&line_idx).is_ok()
    }

    /// True if the diff line at `line_idx` is inside a highlighted hunk.
    pub fn is_in_highlighted_hunk(&self, line_idx: usize) -> bool {
        self.highlighted_hunk_lines.binary_search(&line_idx).is_ok()
    }

    /// Remap line and hunk indices from `FileDiff::lines` to `context_lines`
    /// using the `index_mapping` returned by `build_context_lines_with_mapping`.
    pub fn remap_to_context(&self, index_mapping: &[Option<usize>]) -> Self {
        if self.label.is_none() && !self.is_highlighted() {
            return Self::default();
        }
        let mut highlighted_lines = Vec::new();
        let mut highlighted_hunk_lines = Vec::new();
        for (ctx_idx, orig) in index_mapping.iter().enumerate() {
            if let Some(orig_idx) = orig {
                if self.is_line_highlighted(*orig_idx) {
                    highlighted_lines.push(ctx_idx);
                }
                if self.is_in_highlighted_hunk(*orig_idx) {
                    highlighted_hunk_lines.push(ctx_idx);
                }
            }
        }
        Self {
            label: self.label.clone(),
            touched: self.touched,
            highlighted_lines,
            highlighted_hunk_lines,
        }
    }
}

/// Before/after content of a single file across one highlighted commit or range.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HighlightPair<'a> {
    pub before: Option<&'a str>,
    pub after: Option<&'a str>,
}

/// Before/after content of a single file across one highlighted span, supporting
/// multiple parent trees when the span is a merge commit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HighlightSpan<'a> {
    pub befores: &'a [Option<&'a str>],
    pub after: Option<&'a str>,
}

fn is_diff_change_line(line: &DiffLine) -> bool {
    !matches!(line.source, LineSource::FileHeader | LineSource::Elided)
        && (line.is_change()
            || line.change_source.is_some()
            || line.old_content.is_some()
            || !line.inline_spans.is_empty())
}

fn compute_hunk_indices(
    lines: &[DiffLine],
    blocks: &[ChangeBlock],
    highlighted_lines: &[usize],
) -> Vec<usize> {
    if highlighted_lines.is_empty() {
        return Vec::new();
    }
    let mut hunk_lines: Vec<usize> = highlighted_lines.to_vec();
    if !blocks.is_empty() {
        for block in blocks {
            if highlighted_lines.iter().any(|&idx| block.range.contains(&idx)) {
                hunk_lines.extend(block.range.clone());
            }
        }
    } else {
        let mut idx = 0;
        while idx < lines.len() {
            if is_diff_change_line(&lines[idx]) {
                let start = idx;
                while idx < lines.len() && is_diff_change_line(&lines[idx]) {
                    idx += 1;
                }
                let range = start..idx;
                if highlighted_lines.iter().any(|&h| range.contains(&h)) {
                    hunk_lines.extend(range);
                }
            } else {
                idx += 1;
            }
        }
    }
    hunk_lines.sort_unstable();
    hunk_lines.dedup();
    hunk_lines
}

/// Compute which lines and hunks in `file_diff` were introduced by `pairs`.
pub fn compute_file_highlight(
    file_diff: &FileDiff,
    base_content: Option<&str>,
    working_content: Option<&str>,
    label: &str,
    pairs: &[HighlightPair<'_>],
) -> FileHighlight {
    let before_slices: Vec<[Option<&str>; 1]> = pairs.iter().map(|p| [p.before]).collect();
    let spans: Vec<HighlightSpan<'_>> = pairs
        .iter()
        .zip(&before_slices)
        .map(|(p, b)| HighlightSpan { befores: b.as_slice(), after: p.after })
        .collect();
    compute_file_highlight_spans(file_diff, base_content, working_content, label, &spans)
}

/// Compute which lines and hunks in `file_diff` were introduced by `spans`.
pub fn compute_file_highlight_spans<'a>(
    file_diff: &FileDiff,
    base_content: Option<&str>,
    working_content: Option<&str>,
    label: &str,
    spans: &[HighlightSpan<'a>],
) -> FileHighlight {
    let none_parent: [Option<&'a str>; 1] = [None];
    let parents_of = |s: &HighlightSpan<'a>| -> &[Option<&'a str>] {
        if s.befores.is_empty() { none_parent.as_slice() } else { s.befores }
    };
    if !spans.iter().any(|s| parents_of(s).iter().all(|&b| b != s.after)) {
        return FileHighlight { label: Some(label.to_string()), ..FileHighlight::default() };
    }

    let base_lines: Vec<&str> = base_content.map(|s| s.lines().collect()).unwrap_or_default();
    let working_lines: Vec<&str> = working_content.map(|s| s.lines().collect()).unwrap_or_default();
    let mut added_working_indices = HashSet::new();
    let mut deleted_base_indices = HashSet::new();
    let mut deleted_other_contents = HashSet::new();

    for span in spans {
        let befores = parents_of(span);
        if befores.contains(&span.after) {
            continue;
        }
        let after_lines: Vec<&str> = span.after.map(|s| s.lines().collect()).unwrap_or_default();
        let working_from_after = build_provenance_map(&after_lines, &working_lines);
        let mut span_added_after = vec![true; after_lines.len()];
        let mut span_del_base: Option<HashSet<usize>> = None;
        let mut span_del_other: Option<HashSet<&str>> = None;

        for &before in befores {
            let before_lines: Vec<&str> = before.map(|s| s.lines().collect()).unwrap_or_default();
            let after_from_before = build_provenance_map(&before_lines, &after_lines);
            let before_from_base = build_provenance_map(&base_lines, &before_lines);
            for (a_idx, orig) in after_from_before.iter().enumerate() {
                if orig.is_some() {
                    span_added_after[a_idx] = false;
                }
            }
            let surviving: HashSet<usize> = after_from_before.iter().copied().flatten().collect();
            let (mut p_base, mut p_other) = (HashSet::new(), HashSet::new());
            for (b_idx, orig_base) in before_from_base.iter().enumerate() {
                if !surviving.contains(&b_idx) {
                    if let Some(base_idx) = orig_base {
                        p_base.insert(*base_idx);
                    } else if let Some(text) = before_lines.get(b_idx) {
                        p_other.insert(text.trim_end());
                    }
                }
            }
            span_del_base = Some(match span_del_base {
                None => p_base,
                Some(mut acc) => {
                    acc.retain(|k| p_base.contains(k));
                    acc
                }
            });
            span_del_other = Some(match span_del_other {
                None => p_other,
                Some(mut acc) => {
                    acc.retain(|k| p_other.contains(k));
                    acc
                }
            });
        }
        for (w_idx, orig_after) in working_from_after.iter().enumerate() {
            if orig_after.is_some_and(|a| span_added_after.get(a).copied().unwrap_or(false)) {
                added_working_indices.insert(w_idx);
            }
        }
        deleted_base_indices.extend(span_del_base.into_iter().flatten());
        deleted_other_contents.extend(span_del_other.into_iter().flatten());
    }

    let mut highlighted_lines = Vec::new();
    for (idx, line) in file_diff.lines.iter().enumerate() {
        if !is_diff_change_line(line) {
            continue;
        }
        let is_hl = if line.source.is_deletion() {
            match line.old_line() {
                Some(old_num) => deleted_base_indices.contains(&old_num.saturating_sub(1)),
                None => deleted_other_contents.contains(line.content.trim_end()),
            }
        } else if let Some(new_num) = line.line_number {
            added_working_indices.contains(&new_num.saturating_sub(1))
        } else {
            false
        };
        if is_hl {
            highlighted_lines.push(idx);
        }
    }

    let touched = !highlighted_lines.is_empty();
    let highlighted_hunk_lines =
        compute_hunk_indices(&file_diff.lines, &file_diff.blocks, &highlighted_lines);

    FileHighlight {
        label: Some(label.to_string()),
        touched,
        highlighted_lines,
        highlighted_hunk_lines,
    }
}

/// Flattened highlight marks across `App::lines`, keyed by global line index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HighlightMarks {
    pub label: Option<String>,
    line_indices: Vec<usize>,
    hunk_indices: Vec<usize>,
    file_badges: HashMap<String, String>,
    pub total_highlighted_lines: usize,
    pub highlighted_file_count: usize,
}

impl HighlightMarks {
    /// Build flattened highlight marks from `App::files`.
    /// Returns `Self::default()` with zero allocations when `--highlight` is inactive.
    pub fn from_files(files: &[FileDiff], total_lines: usize) -> Self {
        if files.iter().all(|f| f.highlight.label.is_none() && !f.highlight.is_highlighted()) {
            return Self::default();
        }
        let label = files.iter().find_map(|f| f.highlight.label.clone());
        let (mut line_indices, mut hunk_indices, mut file_badges) =
            (Vec::new(), Vec::new(), HashMap::new());
        let (mut total_highlighted_lines, mut highlighted_file_count, mut offset) = (0, 0, 0);

        for file in files {
            let hl = &file.highlight;
            if hl.is_highlighted() {
                highlighted_file_count += 1;
                total_highlighted_lines += hl.line_count();
                if let Some(path) = file.lines.first().and_then(|l| l.file_path.as_ref())
                    && let Some(badge) = format_badge_text(hl)
                {
                    file_badges.insert(path.clone(), badge);
                }
            }
            line_indices.extend(hl.highlighted_lines.iter().map(|&rel| offset + rel));
            hunk_indices.extend(hl.highlighted_hunk_lines.iter().map(|&rel| offset + rel));
            offset += file.lines.len() + 1;
        }
        debug_assert_eq!(
            offset, total_lines,
            "HighlightMarks::from_files: flattened line count ({total_lines}) disagrees with files + separator rows ({offset})"
        );
        if offset != total_lines {
            return Self::default();
        }
        Self {
            label,
            line_indices,
            hunk_indices,
            file_badges,
            total_highlighted_lines,
            highlighted_file_count,
        }
    }

    pub fn is_line_highlighted(&self, global_line_idx: usize) -> bool {
        self.line_indices.binary_search(&global_line_idx).is_ok()
    }

    pub fn is_in_highlighted_hunk(&self, global_line_idx: usize) -> bool {
        self.hunk_indices.binary_search(&global_line_idx).is_ok()
    }

    pub fn file_badge(&self, path: &str) -> Option<&str> {
        self.file_badges.get(path).map(String::as_str)
    }

    pub fn legend_text(&self) -> Option<String> {
        self.label.as_deref().map(|l| {
            format_legend_text(l, self.total_highlighted_lines, self.highlighted_file_count)
        })
    }
}

/// Format the per-file count badge text (e.g. `"★ 3 highlighted"`).
pub fn format_badge_text(highlight: &FileHighlight) -> Option<String> {
    if !highlight.is_highlighted() {
        return None;
    }
    let count = highlight.line_count();
    Some(if count > 0 { format!("★ {count} highlighted") } else { "★ highlighted".to_string() })
}

/// Format the highlight legend string shown in `--print`, `--html`, and the TUI.
pub fn format_legend_text(label: &str, total_lines: usize, file_count: usize) -> String {
    let line_word = if total_lines == 1 { "line" } else { "lines" };
    let file_word = if file_count == 1 { "file" } else { "files" };
    format!(
        "★ highlighted ({label}): {total_lines} {line_word} in {file_count} {file_word} (★ line, ┃ hunk)"
    )
}

/// Build the 4-column TUI prefix string for a diff line, embedding `★` or `┃` in column 2.
pub fn tui_line_prefix(
    is_moved: bool,
    default_char: char,
    status_sym: &str,
    highlights: &HighlightMarks,
    line_idx: usize,
) -> String {
    let mark = if highlights.is_line_highlighted(line_idx) {
        '★'
    } else if highlights.is_in_highlighted_hunk(line_idx) {
        '┃'
    } else {
        ' '
    };
    if is_moved { format!("M{mark}  ") } else { format!("{default_char}{mark}{status_sym} ") }
}

/// ANSI-colored marker for `--print` lines (`★` for highlighted line, `┃` for highlighted hunk).
pub fn print_highlight_marker(highlight: &FileHighlight, line_idx: usize) -> &'static str {
    if !highlight.is_active() {
        ""
    } else if highlight.is_line_highlighted(line_idx) {
        "\x1b[33m★\x1b[0m"
    } else if highlight.is_in_highlighted_hunk(line_idx) {
        "\x1b[33m┃\x1b[0m"
    } else {
        " "
    }
}

/// Extra CSS row classes and gutter marker symbol for `--html` rows.
pub fn html_highlight_classes_and_marker(
    is_highlighted: bool,
    is_in_hunk: bool,
) -> (&'static str, &'static str) {
    match (is_highlighted, is_in_hunk) {
        (true, _) => (" line-highlighted hunk-highlighted", "★"),
        (false, true) => (" hunk-highlighted", "┃"),
        (false, false) => ("", ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffInput, InlineSpan, compute_four_way_diff};
    use proptest::prelude::*;

    #[test]
    fn marks_only_lines_and_hunks_introduced_by_highlighted_commit() {
        let base = "a\nitem_b_original\nc\nd\ne\nf\ng\nh\nitem_i_original\nitem_j_original\n";
        let commit1 = "a\nitem_b_commit1\nc\nd\ne\nf\ng\nh\nitem_i_original\nitem_j_original\n";
        let commit2 = "a\nitem_b_commit1\nc\nd\ne\nf\ng\nh\nitem_i_commit2\nitem_j_commit2\n";

        let mut diff = compute_four_way_diff(DiffInput {
            path: "src/lib.rs",
            base: Some(base),
            head: Some(commit2),
            index: Some(commit2),
            working: Some(commit2),
            old_path: None,
        });
        diff.highlight = compute_file_highlight(
            &diff,
            Some(base),
            Some(commit2),
            "feature@origin..feature",
            &[HighlightPair { before: Some(commit1), after: Some(commit2) }],
        );

        assert!(diff.highlight.is_highlighted());
        assert_eq!(diff.highlight.line_count(), 2);
        assert_eq!(format_badge_text(&diff.highlight).as_deref(), Some("★ 2 highlighted"));

        for (idx, line) in diff.lines.iter().enumerate() {
            if line.content.contains("commit2") {
                assert!(diff.highlight.is_line_highlighted(idx));
                assert!(diff.highlight.is_in_highlighted_hunk(idx));
            } else if line.content.contains("commit1") {
                assert!(!diff.highlight.is_line_highlighted(idx));
                assert!(!diff.highlight.is_in_highlighted_hunk(idx));
            }
        }
    }

    #[test]
    fn shared_hunk_marks_sibling_change_lines_as_in_hunk_only() {
        let base = "one\ntwo\nthree\n";
        let commit1 = "one\nTWO_1\nthree\n";
        let commit2 = "one\nTWO_1\nTHREE_2\n";

        let diff = compute_four_way_diff(DiffInput {
            path: "a.txt",
            base: Some(base),
            head: Some(commit2),
            index: Some(commit2),
            working: Some(commit2),
            old_path: None,
        });
        let hl = compute_file_highlight(
            &diff,
            Some(base),
            Some(commit2),
            "c2",
            &[HighlightPair { before: Some(commit1), after: Some(commit2) }],
        );

        let idx_two = diff.lines.iter().position(|l| l.content == "TWO_1").unwrap();
        let idx_three = diff.lines.iter().position(|l| l.content == "THREE_2").unwrap();
        assert!(!hl.is_line_highlighted(idx_two));
        assert!(hl.is_in_highlighted_hunk(idx_two));
        assert!(hl.is_line_highlighted(idx_three));
        assert!(hl.is_in_highlighted_hunk(idx_three));
    }

    #[test]
    fn deletions_in_highlighted_commit_are_marked() {
        let base = "keep\ndrop_in_c1\ndrop_in_c2\n";
        let commit1 = "keep\ndrop_in_c2\n";
        let commit2 = "keep\n";

        let diff = compute_four_way_diff(DiffInput {
            path: "del.txt",
            base: Some(base),
            head: Some(commit2),
            index: Some(commit2),
            working: Some(commit2),
            old_path: None,
        });
        let hl = compute_file_highlight(
            &diff,
            Some(base),
            Some(commit2),
            "c2",
            &[HighlightPair { before: Some(commit1), after: Some(commit2) }],
        );

        let idx_c1 = diff.lines.iter().position(|l| l.content == "drop_in_c1").unwrap();
        let idx_c2 = diff.lines.iter().position(|l| l.content == "drop_in_c2").unwrap();
        assert!(!hl.is_line_highlighted(idx_c1));
        assert!(hl.is_line_highlighted(idx_c2));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "disagrees with files + separator rows")]
    fn from_files_panics_when_total_lines_omits_separator_rows() {
        let base = "a\nb\n";
        let work = "a\nB\n";
        let mut diff = compute_four_way_diff(DiffInput {
            path: "a.txt",
            base: Some(base),
            head: Some(work),
            index: Some(work),
            working: Some(work),
            old_path: None,
        });
        diff.highlight = compute_file_highlight(
            &diff,
            Some(base),
            Some(work),
            "c1",
            &[HighlightPair { before: Some(base), after: Some(work) }],
        );
        let unseparated_len = diff.lines.len();
        let _ = HighlightMarks::from_files(&[diff], unseparated_len);
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn from_files_yields_no_marks_when_total_lines_omits_separator_rows() {
        let work = "a\nB\n";
        let mut diff = compute_four_way_diff(DiffInput {
            path: "a.txt",
            base: Some("a\nb\n"),
            head: Some(work),
            index: Some(work),
            working: Some(work),
            old_path: None,
        });
        diff.highlight = compute_file_highlight(
            &diff,
            Some("a\nb\n"),
            Some(work),
            "c1",
            &[HighlightPair { before: Some("a\nb\n"), after: Some(work) }],
        );
        let unseparated_len = diff.lines.len();
        let marks = HighlightMarks::from_files(&[diff], unseparated_len);
        assert_eq!(marks.total_highlighted_lines, 0);
        assert!(marks.line_indices.is_empty());
    }

    #[test]
    fn is_active_when_labeled_or_when_marked_without_a_label() {
        let labeled = FileHighlight {
            label: Some("feature@origin..feature".into()),
            ..FileHighlight::default()
        };
        assert!(labeled.is_active());
        assert!(!labeled.is_highlighted());

        let marked = FileHighlight { touched: true, ..FileHighlight::default() };
        assert!(marked.label.is_none());
        assert!(marked.is_active());
    }

    #[test]
    fn remap_keeps_marks_on_a_highlighted_file_that_has_no_label() {
        let hl = FileHighlight {
            touched: true,
            highlighted_lines: vec![1],
            highlighted_hunk_lines: vec![1, 2],
            ..FileHighlight::default()
        };
        let remapped = hl.remap_to_context(&[None, Some(1), Some(2)]);
        assert!(remapped.label.is_none());
        assert!(remapped.touched);
        assert_eq!(remapped.highlighted_lines, vec![1]);
        assert_eq!(remapped.highlighted_hunk_lines, vec![1, 2]);
    }

    #[test]
    fn remap_keeps_a_label_when_the_file_has_no_marks() {
        let hl = FileHighlight {
            label: Some("feature@origin..feature".into()),
            ..FileHighlight::default()
        };
        let remapped = hl.remap_to_context(&[Some(0)]);
        assert_eq!(remapped.label.as_deref(), Some("feature@origin..feature"));
        assert!(remapped.highlighted_lines.is_empty());
    }

    fn file_with(highlight: FileHighlight) -> (FileDiff, usize) {
        let mut file = FileDiff::new(vec![
            DiffLine::file_header("a.txt"),
            DiffLine::new(LineSource::Committed, "x".into(), '+', Some(1)).with_file_path("a.txt"),
        ]);
        let total = file.lines.len() + 1;
        file.highlight = highlight;
        (file, total)
    }

    #[test]
    fn from_files_keeps_the_label_when_nothing_is_highlighted() {
        let (file, total) = file_with(FileHighlight {
            label: Some("feature@origin..feature".into()),
            ..FileHighlight::default()
        });
        let marks = HighlightMarks::from_files(&[file], total);
        assert_eq!(marks.label.as_deref(), Some("feature@origin..feature"));
        assert_eq!(marks.highlighted_file_count, 0);
        assert_eq!(marks.total_highlighted_lines, 0);
        assert_eq!(
            marks.legend_text().as_deref(),
            Some("★ highlighted (feature@origin..feature): 0 lines in 0 files (★ line, ┃ hunk)")
        );
    }

    #[test]
    fn from_files_keeps_marks_when_the_file_has_no_label() {
        let (file, total) = file_with(FileHighlight {
            touched: true,
            highlighted_lines: vec![1],
            highlighted_hunk_lines: vec![1],
            ..FileHighlight::default()
        });
        let marks = HighlightMarks::from_files(&[file], total);
        assert!(marks.label.is_none());
        assert_eq!(marks.total_highlighted_lines, 1);
        assert_eq!(marks.highlighted_file_count, 1);
        assert!(marks.is_line_highlighted(1));
        assert!(!marks.is_line_highlighted(0));
    }

    #[test]
    fn a_global_line_outside_the_hunk_is_not_inside_it() {
        let (file, total) = file_with(FileHighlight {
            label: Some("c1".into()),
            touched: true,
            highlighted_lines: vec![1],
            highlighted_hunk_lines: vec![1],
        });
        let marks = HighlightMarks::from_files(&[file], total);
        assert!(marks.is_in_highlighted_hunk(1));
        assert!(!marks.is_in_highlighted_hunk(0));
    }

    #[test]
    fn a_touched_file_with_no_highlighted_lines_badges_without_a_count() {
        let hl = FileHighlight {
            label: Some("feature@origin..feature".into()),
            touched: true,
            ..FileHighlight::default()
        };
        assert_eq!(format_badge_text(&hl).as_deref(), Some("★ highlighted"));
        assert_eq!(
            format_badge_text(&FileHighlight {
                highlighted_lines: vec![3, 4],
                touched: true,
                ..FileHighlight::default()
            })
            .as_deref(),
            Some("★ 2 highlighted")
        );
    }

    #[test]
    fn a_span_that_changes_nothing_keeps_its_label_and_marks_no_lines() {
        let same = "alpha\nbeta\n";
        let diff = compute_four_way_diff(DiffInput {
            path: "a.txt",
            base: Some(same),
            head: Some(same),
            index: Some(same),
            working: Some(same),
            old_path: None,
        });
        let hl = compute_file_highlight(
            &diff,
            Some(same),
            Some(same),
            "feature@origin..feature",
            &[HighlightPair { before: Some(same), after: Some(same) }],
        );
        assert_eq!(hl.label.as_deref(), Some("feature@origin..feature"));
        assert!(hl.highlighted_lines.is_empty());
        assert!(hl.highlighted_hunk_lines.is_empty());
        assert!(!hl.touched);
        assert!(hl.is_active());
    }

    #[test]
    fn is_diff_change_line_rejects_context_and_accepts_old_content_or_spans() {
        assert!(!is_diff_change_line(&DiffLine::file_header("a.txt")));
        assert!(!is_diff_change_line(&DiffLine::elided(4)));
        let context = DiffLine::new(LineSource::Base, "keep".into(), ' ', Some(1));
        assert!(!is_diff_change_line(&context));

        let added = DiffLine::new(LineSource::Committed, "added".into(), '+', Some(2));
        assert!(is_diff_change_line(&added));

        let mut old_only = DiffLine::new(LineSource::Base, "now".into(), ' ', Some(3));
        old_only.old_content = Some("was".into());
        assert!(is_diff_change_line(&old_only));

        let mut spanned = DiffLine::new(LineSource::Base, "now".into(), ' ', Some(4));
        spanned.inline_spans = vec![InlineSpan {
            text: "now".into(),
            source: Some(LineSource::Committed),
            is_deletion: false,
        }];
        assert!(is_diff_change_line(&spanned));
    }

    #[test]
    fn empty_blocks_expand_only_the_change_run_holding_a_highlighted_line() {
        // 0 header, 1 elided, 2 context, 3 an earlier change, 4 context,
        // 5 sibling, 6 old-content highlight, 7 sibling at end of file.
        let mut marked = DiffLine::new(LineSource::Base, "marked".into(), ' ', Some(5));
        marked.old_content = Some("was".into());
        let lines = vec![
            DiffLine::file_header("a.txt"),
            DiffLine::elided(3),
            DiffLine::new(LineSource::Base, "before".into(), ' ', Some(1)),
            DiffLine::new(LineSource::Committed, "old-hunk".into(), '+', Some(2)),
            DiffLine::new(LineSource::Base, "gap".into(), ' ', Some(3)),
            DiffLine::new(LineSource::Committed, "sib-before".into(), '+', Some(4)),
            marked,
            DiffLine::new(LineSource::Committed, "sib-after".into(), '+', Some(6)),
        ];
        assert_eq!(compute_hunk_indices(&lines, &[], &[6]), vec![5, 6, 7]);
    }

    #[test]
    fn whitespace_only_changes_have_no_blocks_and_still_share_a_hunk() {
        // extract_blocks drops a run whose text is all whitespace, so
        // compute_hunk_indices takes its empty-block fallback for this diff.
        let base = "keep\n";
        let mid = "keep\n \n";
        let work = "keep\n \n  \n";
        let diff = compute_four_way_diff(DiffInput {
            path: "blank.txt",
            base: Some(base),
            head: Some(work),
            index: Some(work),
            working: Some(work),
            old_path: None,
        });
        assert!(
            diff.blocks.is_empty(),
            "whitespace-only changes still produced blocks {:?}",
            diff.blocks.iter().map(|b| b.range.clone()).collect::<Vec<_>>()
        );
        let hl = compute_file_highlight(
            &diff,
            Some(base),
            Some(work),
            "c2",
            &[HighlightPair { before: Some(mid), after: Some(work) }],
        );
        let ws: Vec<usize> = diff
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.is_change() && line.content.trim().is_empty())
            .map(|(idx, _)| idx)
            .collect();
        assert_eq!(
            ws.len(),
            2,
            "lines {:?}",
            diff.lines
                .iter()
                .map(|l| (l.source, l.content.as_str(), l.line_number))
                .collect::<Vec<_>>()
        );
        let marked: Vec<usize> =
            ws.iter().copied().filter(|&idx| hl.is_line_highlighted(idx)).collect();
        assert_eq!(marked, vec![ws[1]], "hunk {:?}", hl.highlighted_hunk_lines);
        for idx in ws {
            assert!(hl.is_in_highlighted_hunk(idx), "hunk {:?}", hl.highlighted_hunk_lines);
        }
        let keep = diff.lines.iter().position(|line| line.content == "keep").expect("keep");
        assert!(!hl.is_line_highlighted(keep));
        assert!(!hl.is_in_highlighted_hunk(keep));
    }

    proptest! {
        #[test]
        fn prop_highlighted_lines_are_valid_sorted_subset_of_hunk_lines(
            base_lines in prop::collection::vec("[a-e]{1,4}", 0..12),
            mid_lines in prop::collection::vec("[a-e]{1,4}", 0..12),
            work_lines in prop::collection::vec("[a-e]{1,4}", 0..12),
        ) {
            let base = base_lines.join("\n");
            let mid = mid_lines.join("\n");
            let work = work_lines.join("\n");

            let diff = compute_four_way_diff(DiffInput {
                path: "prop.txt",
                base: Some(&base),
                head: Some(&work),
                index: Some(&work),
                working: Some(&work),
                old_path: None,
            });
            let hl = compute_file_highlight(
                &diff,
                Some(&base),
                Some(&work),
                "mid..work",
                &[HighlightPair { before: Some(&mid), after: Some(&work) }],
            );

            for w in hl.highlighted_lines.windows(2) {
                prop_assert!(w[0] < w[1]);
            }
            for &idx in &hl.highlighted_lines {
                prop_assert!(idx < diff.lines.len());
                prop_assert!(hl.is_in_highlighted_hunk(idx));
            }
            for &idx in &hl.highlighted_hunk_lines {
                prop_assert!(idx < diff.lines.len());
            }

            let (shown, mapping) = crate::output::apply_view_mode_with_mapping(
                &diff.lines,
                &crate::app::ViewMode::Full,
            );
            prop_assert_eq!(shown.len(), diff.lines.len());
            let remapped = hl.remap_to_context(&mapping);
            prop_assert_eq!(remapped, hl);
        }

        #[test]
        fn prop_remap_marks_never_exceed_shown_lines_and_unmarked_stay_unmarked(
            base_lines in prop::collection::vec("[a-e]{1,4}", 0..25),
            mid_lines in prop::collection::vec("[a-e]{1,4}", 0..25),
            work_lines in prop::collection::vec("[a-e]{1,4}", 0..25),
        ) {
            let base = base_lines.join("\n");
            let mid = mid_lines.join("\n");
            let work = work_lines.join("\n");

            let mut diff = compute_four_way_diff(DiffInput {
                path: "prop.txt",
                base: Some(&base),
                head: Some(&work),
                index: Some(&work),
                working: Some(&work),
                old_path: None,
            });
            let hl = compute_file_highlight(
                &diff,
                Some(&base),
                Some(&work),
                "mid..work",
                &[HighlightPair { before: Some(&mid), after: Some(&work) }],
            );
            diff.highlight = hl.clone();

            let vis = crate::output::context_visibility(&diff.lines);
            prop_assert_eq!(vis.len(), diff.lines.len());
            let (shown, mapping) = crate::output::apply_view_mode_with_mapping(
                &diff.lines,
                &crate::app::ViewMode::Context,
            );
            prop_assert_eq!(shown.len(), mapping.len());

            let remapped = hl.remap_to_context(&mapping);
            for &idx in &remapped.highlighted_lines {
                prop_assert!(idx < shown.len(), "mark {idx} >= shown {}", shown.len());
            }
            for &idx in &remapped.highlighted_hunk_lines {
                prop_assert!(idx < shown.len(), "hunk mark {idx} >= shown {}", shown.len());
            }
            for (shown_idx, orig) in mapping.iter().enumerate() {
                match orig {
                    None => {
                        prop_assert!(!remapped.is_line_highlighted(shown_idx));
                        prop_assert!(!remapped.is_in_highlighted_hunk(shown_idx));
                    }
                    Some(orig_idx) => {
                        prop_assert!(vis[*orig_idx]);
                        prop_assert_eq!(
                            remapped.is_line_highlighted(shown_idx),
                            hl.is_line_highlighted(*orig_idx)
                        );
                        prop_assert_eq!(
                            remapped.is_in_highlighted_hunk(shown_idx),
                            hl.is_in_highlighted_hunk(*orig_idx)
                        );
                    }
                }
            }

            let total_lines = diff.lines.len() + 1;
            let flat = HighlightMarks::from_files(&[diff], total_lines);
            for &idx in &flat.line_indices {
                prop_assert!(idx < total_lines);
            }
            for &idx in &flat.hunk_indices {
                prop_assert!(idx < total_lines);
            }
        }
    }
}
