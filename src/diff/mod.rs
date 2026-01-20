//! Diff computation module for branchdiff
//!
//! This module computes 4-way diffs showing changes across:
//! - base (merge-base with main/master)
//! - head (committed on branch)
//! - index (staged)
//! - working (working tree)

mod inline;
mod output;
mod provenance;

pub use inline::InlineSpan;

pub(crate) use inline::compute_inline_diff_merged;

use std::collections::HashMap;

use output::{build_working_line_output, determine_deletion_source};
use provenance::{build_modification_map, build_provenance_map};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineSource {
    Base,
    Committed,
    Staged,
    Unstaged,
    DeletedBase,
    DeletedCommitted,
    DeletedStaged,
    CanceledCommitted,
    CanceledStaged,
    FileHeader,
    Elided,
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub source: LineSource,
    pub content: String,
    pub prefix: char,
    pub line_number: Option<usize>,
    pub file_path: Option<String>,
    pub inline_spans: Vec<InlineSpan>,
    pub old_content: Option<String>,
    pub change_source: Option<LineSource>,
}

impl DiffLine {
    pub fn new(source: LineSource, content: String, prefix: char, line_number: Option<usize>) -> Self {
        Self {
            source,
            content,
            prefix,
            line_number,
            file_path: None,
            inline_spans: Vec::new(),
            old_content: None,
            change_source: None,
        }
    }

    pub fn with_old_content(mut self, old: &str) -> Self {
        self.old_content = Some(old.to_string());
        self
    }

    pub fn with_change_source(mut self, change_source: LineSource) -> Self {
        self.change_source = Some(change_source);
        self
    }

    pub fn ensure_inline_spans(&mut self) {
        if self.inline_spans.is_empty()
            && let Some(ref old) = self.old_content
        {
            let source = self.change_source.unwrap_or(self.source);
            let result = compute_inline_diff_merged(old, &self.content, source);
            self.inline_spans = result.spans;
        }
    }

    pub fn with_file_path(mut self, path: &str) -> Self {
        self.file_path = Some(path.to_string());
        self
    }

    pub fn file_header(path: &str) -> Self {
        Self {
            source: LineSource::FileHeader,
            content: path.to_string(),
            prefix: ' ',
            line_number: None,
            file_path: Some(path.to_string()),
            inline_spans: Vec::new(),
            old_content: None,
            change_source: None,
        }
    }

    pub fn deleted_file_header(path: &str) -> Self {
        Self {
            source: LineSource::FileHeader,
            content: format!("{} (deleted)", path),
            prefix: ' ',
            line_number: None,
            file_path: Some(path.to_string()),
            inline_spans: Vec::new(),
            old_content: None,
            change_source: None,
        }
    }

    pub fn renamed_file_header(old_path: &str, new_path: &str) -> Self {
        Self {
            source: LineSource::FileHeader,
            content: format!("{} → {}", old_path, new_path),
            prefix: ' ',
            line_number: None,
            file_path: Some(new_path.to_string()),
            inline_spans: Vec::new(),
            old_content: None,
            change_source: None,
        }
    }

    pub fn elided(count: usize) -> Self {
        Self {
            source: LineSource::Elided,
            content: format!("{} lines", count),
            prefix: ' ',
            line_number: None,
            file_path: None,
            inline_spans: Vec::new(),
            old_content: None,
            change_source: None,
        }
    }
}

#[derive(Debug)]
pub struct FileDiff {
    pub lines: Vec<DiffLine>,
}


fn index_survives_to_working(index_idx: usize, working_from_index: &[Option<usize>]) -> bool {
    working_from_index.contains(&Some(index_idx))
}

fn index_line_in_working(
    index_idx: usize,
    working_from_index: &[Option<usize>],
    index_working_mods: &HashMap<usize, (usize, &str)>,
) -> bool {
    if index_survives_to_working(index_idx, working_from_index) {
        return true;
    }
    index_working_mods.values().any(|(src_idx, _)| *src_idx == index_idx)
}

fn head_survives_to_working(
    head_idx: usize,
    index_from_head: &[Option<usize>],
    working_from_index: &[Option<usize>],
) -> bool {
    for (index_idx, &prov) in index_from_head.iter().enumerate() {
        if prov == Some(head_idx) && index_survives_to_working(index_idx, working_from_index) {
            return true;
        }
    }
    false
}

fn collect_canceled_simple(
    head_lines: &[&str],
    index_lines: &[&str],
    head_from_base: &[Option<usize>],
    index_from_head: &[Option<usize>],
    working_from_index: &[Option<usize>],
    path: &str,
) -> Vec<DiffLine> {
    let mut result = Vec::new();

    // Canceled committed: lines added in head but not in working
    for (head_idx, head_line) in head_lines.iter().enumerate() {
        if head_from_base.get(head_idx).copied().flatten().is_some() {
            continue;
        }
        if !head_survives_to_working(head_idx, index_from_head, working_from_index) {
            result.push(
                DiffLine::new(LineSource::CanceledCommitted, head_line.trim_end().to_string(), '±', None)
                    .with_file_path(path),
            );
        }
    }

    // Canceled staged: lines added in index but not in working
    for (index_idx, index_line) in index_lines.iter().enumerate() {
        if index_from_head.get(index_idx).copied().flatten().is_some() {
            continue;
        }
        if !index_survives_to_working(index_idx, working_from_index) {
            result.push(
                DiffLine::new(LineSource::CanceledStaged, index_line.trim_end().to_string(), '±', None)
                    .with_file_path(path),
            );
        }
    }

    result
}

fn collect_canceled_committed(
    head_lines: &[&str],
    head_from_base: &[Option<usize>],
    index_from_head: &[Option<usize>],
    working_from_index: &[Option<usize>],
    head_index_mods: &HashMap<usize, (usize, &str)>,
    index_working_mods: &HashMap<usize, (usize, &str)>,
) -> Vec<(usize, String)> {
    let mut result = Vec::new();

    for (head_idx, head_line) in head_lines.iter().enumerate() {
        if head_from_base.get(head_idx).copied().flatten().is_some() {
            continue;
        }

        let mut in_working = false;

        // Check via direct provenance
        for (index_idx, &prov) in index_from_head.iter().enumerate() {
            if prov == Some(head_idx)
                && index_line_in_working(index_idx, working_from_index, index_working_mods)
            {
                in_working = true;
                break;
            }
        }

        // Check via modification maps
        if !in_working {
            for (index_idx, (src_head_idx, _)) in head_index_mods {
                if *src_head_idx == head_idx
                    && index_line_in_working(*index_idx, working_from_index, index_working_mods)
                {
                    in_working = true;
                    break;
                }
            }
        }

        if !in_working {
            result.push((head_idx, head_line.trim_end().to_string()));
        }
    }

    result
}

fn collect_canceled_staged(
    index_lines: &[&str],
    index_from_head: &[Option<usize>],
    working_from_index: &[Option<usize>],
    index_working_mods: &HashMap<usize, (usize, &str)>,
) -> Vec<(usize, String)> {
    let mut result = Vec::new();

    for (index_idx, index_line) in index_lines.iter().enumerate() {
        if index_from_head.get(index_idx).copied().flatten().is_some() {
            continue;
        }

        if !index_line_in_working(index_idx, working_from_index, index_working_mods) {
            result.push((index_idx, index_line.trim_end().to_string()));
        }
    }

    result
}

fn find_insertion_position(positions: &[Option<usize>], target_idx: usize) -> usize {
    for (i, &pos) in positions.iter().enumerate().rev() {
        if let Some(p) = pos
            && p < target_idx
        {
            return i + 1;
        }
    }
    positions.len()
}

fn insert_canceled_lines(
    lines: &mut Vec<DiffLine>,
    canceled: Vec<(usize, String)>,
    source: LineSource,
    path: &str,
    positions: &mut Vec<Option<usize>>,
) {
    for (idx, content) in canceled.into_iter().rev() {
        let insert_pos = find_insertion_position(positions, idx);
        let canceled_line = DiffLine::new(source, content, '±', None).with_file_path(path);
        lines.insert(insert_pos, canceled_line);
        positions.insert(insert_pos, Some(idx));
    }
}

fn build_deletion_diff(path: &str, content: &str, source: LineSource) -> FileDiff {
    let mut lines = vec![DiffLine::deleted_file_header(path)];
    for (i, line) in content.lines().enumerate() {
        lines.push(
            DiffLine::new(source, line.to_string(), '-', Some(i + 1)).with_file_path(path),
        );
    }
    FileDiff { lines }
}

fn check_file_deletion(
    path: &str,
    base_content: Option<&str>,
    head_content: Option<&str>,
    index_content: Option<&str>,
    working_content: Option<&str>,
) -> Option<FileDiff> {
    // Unstaged deletion: file exists in index but not working tree
    if working_content.is_none()
        && let Some(content) = index_content
    {
        return Some(build_deletion_diff(path, content, LineSource::DeletedStaged));
    }

    // Staged deletion: file exists in HEAD but not in index or working
    if index_content.is_none()
        && working_content.is_none()
        && let Some(content) = head_content
    {
        return Some(build_deletion_diff(path, content, LineSource::DeletedCommitted));
    }

    // Committed deletion: file exists in base but not in HEAD/index/working
    if head_content.is_none()
        && index_content.is_none()
        && working_content.is_none()
        && let Some(content) = base_content
    {
        return Some(build_deletion_diff(path, content, LineSource::DeletedBase));
    }

    None
}

/// Compute 4-way diff: base→head→index→working.
/// Uses provenance maps (not content similarity) to determine line sources.
/// Inline diffs only created from explicit modification maps.
pub fn compute_file_diff_v2(
    path: &str,
    base_content: Option<&str>,
    head_content: Option<&str>,
    index_content: Option<&str>,
    working_content: Option<&str>,
    old_path: Option<&str>,
) -> FileDiff {
    if let Some(deletion_diff) = check_file_deletion(path, base_content, head_content, index_content, working_content) {
        return deletion_diff;
    }

    let header = match old_path {
        Some(old) => DiffLine::renamed_file_header(old, path),
        None => DiffLine::file_header(path),
    };
    let mut lines = vec![header];

    let base = base_content.unwrap_or("");
    let head = head_content.unwrap_or(base);
    let index = index_content.unwrap_or(head);
    let working = working_content.unwrap_or(index);

    let base_lines: Vec<&str> = base.lines().collect();
    let head_lines: Vec<&str> = head.lines().collect();
    let index_lines: Vec<&str> = index.lines().collect();
    let working_lines: Vec<&str> = working.lines().collect();

    // If base == working, only show "canceled" lines (added then removed)
    if base == working {
        let head_from_base = build_provenance_map(&base_lines, &head_lines);
        let index_from_head = build_provenance_map(&head_lines, &index_lines);
        let working_from_index = build_provenance_map(&index_lines, &working_lines);

        lines.extend(collect_canceled_simple(
            &head_lines,
            &index_lines,
            &head_from_base,
            &index_from_head,
            &working_from_index,
            path,
        ));

        return FileDiff { lines };
    }

    // Build provenance maps: provenance[new_idx] = Some(old_idx) if line came from old
    let head_from_base = build_provenance_map(&base_lines, &head_lines);
    let index_from_head = build_provenance_map(&head_lines, &index_lines);
    let working_from_index = build_provenance_map(&index_lines, &working_lines);

    // Build modification maps for adjacent delete-insert pairs with meaningful similarity
    let base_head_mods = build_modification_map(&base_lines, &head_lines, LineSource::Committed);
    let head_index_mods = build_modification_map(&head_lines, &index_lines, LineSource::Staged);
    let index_working_mods = build_modification_map(&index_lines, &working_lines, LineSource::Unstaged);

    // Build reverse provenance: base_to_working[base_idx] = Some(working_idx) if still present
    let mut base_to_working: Vec<Option<usize>> = vec![None; base_lines.len()];

    for working_idx in 0..working_lines.len() {
        if let Some(index_idx) = working_from_index.get(working_idx).copied().flatten()
            && let Some(head_idx) = index_from_head.get(index_idx).copied().flatten()
            && let Some(base_idx) = head_from_base.get(head_idx).copied().flatten()
        {
            base_to_working[base_idx] = Some(working_idx);
        }
    }

    // Modified base lines should not show as deletions - they're merged into inline diffs
    for (head_idx, (base_idx, _)) in &base_head_mods {
        for working_idx in 0..working_lines.len() {
            if let Some(index_idx) = working_from_index.get(working_idx).copied().flatten()
                && let Some(h_idx) = index_from_head.get(index_idx).copied().flatten()
                && h_idx == *head_idx
            {
                base_to_working[*base_idx] = Some(working_idx);
                break;
            }
        }
    }

    for (index_idx, (head_idx, _)) in &head_index_mods {
        if let Some(base_idx) = head_from_base.get(*head_idx).copied().flatten() {
            for working_idx in 0..working_lines.len() {
                if working_from_index.get(working_idx).copied().flatten() == Some(*index_idx) {
                    base_to_working[base_idx] = Some(working_idx);
                    break;
                }
            }
        }
    }

    for (working_idx, (index_idx, _)) in &index_working_mods {
        if let Some(head_idx) = index_from_head.get(*index_idx).copied().flatten()
            && let Some(base_idx) = head_from_base.get(head_idx).copied().flatten()
        {
            base_to_working[base_idx] = Some(*working_idx);
        }
    }

    let trace_source = |working_idx: usize| -> LineSource {
        if let Some(index_idx) = working_from_index.get(working_idx).copied().flatten() {
            if let Some(head_idx) = index_from_head.get(index_idx).copied().flatten() {
                if head_from_base.get(head_idx).copied().flatten().is_some() {
                    LineSource::Base
                } else {
                    LineSource::Committed
                }
            } else {
                LineSource::Staged
            }
        } else {
            LineSource::Unstaged
        }
    };

    let trace_index_source = |index_idx: usize| -> LineSource {
        if let Some(head_idx) = index_from_head.get(index_idx).copied().flatten() {
            if head_from_base.get(head_idx).copied().flatten().is_some() {
                LineSource::Base
            } else {
                LineSource::Committed
            }
        } else {
            LineSource::Staged
        }
    };

    let trace_head_source = |head_idx: usize| -> LineSource {
        if head_from_base.get(head_idx).copied().flatten().is_some() {
            LineSource::Base
        } else {
            LineSource::Committed
        }
    };

    // Find base position for a working line (via provenance or modification maps)
    let get_working_base_pos = |working_idx: usize| -> Option<usize> {
        if let Some(index_idx) = working_from_index.get(working_idx).copied().flatten()
            && let Some(head_idx) = index_from_head.get(index_idx).copied().flatten()
            && let Some(base_idx) = head_from_base.get(head_idx).copied().flatten()
        {
            return Some(base_idx);
        }

        if let Some((index_idx, _)) = index_working_mods.get(&working_idx)
            && let Some(head_idx) = index_from_head.get(*index_idx).copied().flatten()
            && let Some(base_idx) = head_from_base.get(head_idx).copied().flatten()
        {
            return Some(base_idx);
        }

        None
    };

    // Find head position for a working line (via provenance or modification maps)
    let get_working_head_idx = |working_idx: usize| -> Option<usize> {
        if let Some(index_idx) = working_from_index.get(working_idx).copied().flatten()
            && let Some(head_idx) = index_from_head.get(index_idx).copied().flatten()
        {
            return Some(head_idx);
        }

        if let Some((index_idx, _)) = index_working_mods.get(&working_idx)
            && let Some(head_idx) = index_from_head.get(*index_idx).copied().flatten()
        {
            return Some(head_idx);
        }

        None
    };

    let mut line_num = 1usize;
    let mut next_base_deletion = 0usize;
    let mut output_head_positions: Vec<Option<usize>> = Vec::new();

    for working_idx in 0..working_lines.len() {
        let working_content = working_lines[working_idx].trim_end();
        let working_base_pos = get_working_base_pos(working_idx);

        // Deletion boundary: output deletions before working lines from later base positions.
        // For insertions, look ahead to find the next base position.
        let deletion_boundary = if let Some(pos) = working_base_pos {
            Some(pos)
        } else {
            let mut next_base = None;
            for future_idx in (working_idx + 1)..working_lines.len() {
                if let Some(pos) = get_working_base_pos(future_idx) {
                    next_base = Some(pos);
                    break;
                }
            }
            next_base
        };

        if let Some(boundary) = deletion_boundary {
            while next_base_deletion < boundary {
                if base_to_working[next_base_deletion].is_none() {
                    let base_content = base_lines[next_base_deletion].trim_end();
                    let delete_source = determine_deletion_source(
                        next_base_deletion,
                        &base_lines,
                        &head_lines,
                        &index_lines,
                        &head_from_base,
                        &index_from_head,
                    );

                    lines.push(DiffLine::new(
                        delete_source,
                        base_content.to_string(),
                        '-',
                        None,
                    ).with_file_path(path));
                    let head_idx_for_deletion = head_from_base.iter()
                        .position(|&h| h == Some(next_base_deletion));
                    output_head_positions.push(head_idx_for_deletion);
                }
                next_base_deletion += 1;
            }
        }

        let source = trace_source(working_idx);
        let working_head_idx = get_working_head_idx(working_idx);
        output_head_positions.push(working_head_idx);
        let output_line = build_working_line_output(
            working_idx,
            working_content,
            source,
            line_num,
            path,
            &working_from_index,
            &index_from_head,
            &head_from_base,
            &index_working_mods,
            &base_head_mods,
            &head_index_mods,
            &index_lines,
            &head_lines,
            &trace_index_source,
            &trace_head_source,
        );

        lines.push(output_line);
        line_num += 1;

        if let Some(base_pos) = working_base_pos {
            next_base_deletion = next_base_deletion.max(base_pos + 1);
        }
    }

    while next_base_deletion < base_lines.len() {
        if base_to_working[next_base_deletion].is_none() {
            let base_content = base_lines[next_base_deletion].trim_end();
            let delete_source = determine_deletion_source(
                next_base_deletion,
                &base_lines,
                &head_lines,
                &index_lines,
                &head_from_base,
                &index_from_head,
            );
            lines.push(DiffLine::new(
                delete_source,
                base_content.to_string(),
                '-',
                None,
            ).with_file_path(path));
            let head_idx_for_deletion = head_from_base.iter()
                .position(|&h| h == Some(next_base_deletion));
            output_head_positions.push(head_idx_for_deletion);
        }
        next_base_deletion += 1;
    }

    // Collect and insert canceled lines (added in commits/staging but removed in working)
    let canceled_committed = collect_canceled_committed(
        &head_lines,
        &head_from_base,
        &index_from_head,
        &working_from_index,
        &head_index_mods,
        &index_working_mods,
    );
    insert_canceled_lines(
        &mut lines,
        canceled_committed,
        LineSource::CanceledCommitted,
        path,
        &mut output_head_positions,
    );

    let canceled_staged = collect_canceled_staged(
        &index_lines,
        &index_from_head,
        &working_from_index,
        &index_working_mods,
    );
    let mut output_index_positions: Vec<Option<usize>> = lines
        .iter()
        .map(|line| index_lines.iter().position(|h| h.trim_end() == line.content))
        .collect();
    insert_canceled_lines(
        &mut lines,
        canceled_staged,
        LineSource::CanceledStaged,
        path,
        &mut output_index_positions,
    );

    FileDiff { lines }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compute_file_diff_v2_with_inline(
        path: &str,
        base: Option<&str>,
        head: Option<&str>,
        index: Option<&str>,
        working: Option<&str>,
    ) -> FileDiff {
        let mut diff = compute_file_diff_v2(path, base, head, index, working, None);
        for line in &mut diff.lines {
            line.ensure_inline_spans();
        }
        diff
    }

    fn content_lines(diff: &FileDiff) -> Vec<&DiffLine> {
        diff.lines.iter().filter(|l| l.source != LineSource::FileHeader).collect()
    }

    #[test]
    fn test_no_changes() {
        let content = "line1\nline2\nline3";
        let diff = compute_file_diff_v2_with_inline("test.txt", Some(content), Some(content), Some(content), Some(content));

        assert_eq!(diff.lines.len(), 1);
        assert_eq!(diff.lines[0].source, LineSource::FileHeader);
    }

    #[test]
    fn test_renamed_file_header() {
        let header = DiffLine::renamed_file_header("old/path.rs", "new/path.rs");
        assert_eq!(header.source, LineSource::FileHeader);
        assert_eq!(header.content, "old/path.rs → new/path.rs");
        assert_eq!(header.file_path, Some("new/path.rs".to_string()));
    }

    #[test]
    fn test_compute_file_diff_with_rename() {
        let content = "line1\nline2";
        let diff = compute_file_diff_v2(
            "new/path.rs",
            Some(content),
            Some(content),
            Some(content),
            Some(content),
            Some("old/path.rs"),
        );
        assert_eq!(diff.lines[0].source, LineSource::FileHeader);
        assert_eq!(diff.lines[0].content, "old/path.rs → new/path.rs");
    }

    #[test]
    fn test_committed_addition() {
        let base = "line1\nline2";
        let head = "line1\nline2\nline3";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(head), Some(head));

        let committed_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::Committed)
            .collect();

        assert!(!committed_lines.is_empty());
        assert!(committed_lines.iter().any(|l| l.content == "line3" && l.prefix == '+'));
    }

    #[test]
    fn test_canceled_committed_line() {
        let base = "line1\nline2";
        let head = "line1\nline2\ncommitted_line";
        let working = "line1\nline2";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(head), Some(working));

        let canceled_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::CanceledCommitted)
            .collect();

        assert_eq!(canceled_lines.len(), 1);
        assert_eq!(canceled_lines[0].content, "committed_line");
        assert_eq!(canceled_lines[0].prefix, '±');
    }

    #[test]
    fn test_canceled_staged_line() {
        let base = "line1\nline2";
        let index = "line1\nline2\nstaged_line";
        let working = "line1\nline2";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(index), Some(working));

        let canceled_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::CanceledStaged)
            .collect();

        assert_eq!(canceled_lines.len(), 1);
        assert_eq!(canceled_lines[0].content, "staged_line");
        assert_eq!(canceled_lines[0].prefix, '±');
    }

    #[test]
    fn test_committed_then_modified_not_canceled() {
        let base = "line1\nline2";
        let head = "line1\nline2\nversion1";
        let working = "line1\nline2\nversion2";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(head), Some(working));

        let canceled_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::CanceledCommitted)
            .collect();

        assert_eq!(canceled_lines.len(), 0, "modified line should not be canceled");
    }

    #[test]
    fn test_staged_then_modified_not_canceled() {
        let base = "line1\nline2";
        let index = "line1\nline2\nversion1";
        let working = "line1\nline2\nversion2";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(index), Some(working));

        let canceled_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::CanceledStaged)
            .collect();

        assert_eq!(canceled_lines.len(), 0, "modified line should not be canceled");
    }

    #[test]
    fn test_unstaged_addition() {
        let content = "line1\nline2";
        let working = "line1\nline2\nline3";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(content), Some(content), Some(content), Some(working));

        let unstaged_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::Unstaged)
            .collect();

        assert!(!unstaged_lines.is_empty());
        assert!(unstaged_lines.iter().any(|l| l.content == "line3" && l.prefix == '+'));
    }

    #[test]
    fn test_staged_addition() {
        let base = "line1\nline2";
        let index = "line1\nline2\nline3";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(index), Some(index));

        let staged_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::Staged)
            .collect();

        assert!(!staged_lines.is_empty());
        assert!(staged_lines.iter().any(|l| l.content == "line3" && l.prefix == '+'));
    }

    #[test]
    fn test_new_file() {
        let working = "line1\nline2";

        let diff = compute_file_diff_v2_with_inline("test.txt", None, None, None, Some(working));

        let unstaged_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::Unstaged)
            .collect();

        assert_eq!(unstaged_lines.len(), 2);
        assert!(unstaged_lines.iter().all(|l| l.prefix == '+'));
    }

    #[test]
    fn test_deleted_file_staged_deletion() {
        let base = "line1\nline2";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), None, None);

        assert_eq!(diff.lines[0].source, LineSource::FileHeader);
        assert_eq!(diff.lines[0].content, "test.txt (deleted)");

        let deleted_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::DeletedCommitted)
            .collect();

        assert_eq!(deleted_lines.len(), 2);
        assert!(deleted_lines.iter().all(|l| l.prefix == '-'));
        assert_eq!(deleted_lines[0].content, "line1");
        assert_eq!(deleted_lines[1].content, "line2");
    }

    #[test]
    fn test_deleted_file_unstaged_deletion() {
        let content = "line1\nline2\nline3";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(content), Some(content), Some(content), None);

        assert_eq!(diff.lines[0].source, LineSource::FileHeader);
        assert_eq!(diff.lines[0].content, "test.txt (deleted)");

        let deleted_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::DeletedStaged)
            .collect();

        assert_eq!(deleted_lines.len(), 3);
        assert!(deleted_lines.iter().all(|l| l.prefix == '-'));
        assert_eq!(deleted_lines[0].content, "line1");
        assert_eq!(deleted_lines[1].content, "line2");
        assert_eq!(deleted_lines[2].content, "line3");
    }

    #[test]
    fn test_deleted_file_committed_deletion() {
        let base = "old content\nmore old content";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), None, None, None);

        assert_eq!(diff.lines[0].source, LineSource::FileHeader);
        assert_eq!(diff.lines[0].content, "test.txt (deleted)");

        let deleted_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::DeletedBase)
            .collect();

        assert_eq!(deleted_lines.len(), 2);
        assert!(deleted_lines.iter().all(|l| l.prefix == '-'));
        assert_eq!(deleted_lines[0].content, "old content");
        assert_eq!(deleted_lines[1].content, "more old content");
    }

    #[test]
    fn test_modified_line_shows_merged_with_inline_spans() {
        let base = "line1\nold content\nline3";
        let working = "line1\nnew content\nline3";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();

        assert_eq!(with_spans.len(), 1);
        assert_eq!(with_spans[0].content, "new content");
        assert!(with_spans[0].prefix == ' ');
    }

    #[test]
    fn test_modified_line_position_preserved() {
        let base = "before\nprocess_data(input)\nafter";
        let working = "before\nprocess_data(input, options)\nafter";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let contents: Vec<_> = lines.iter().map(|l| l.content.as_str()).collect();
        assert_eq!(contents, vec!["before", "process_data(input, options)", "after"]);

        let modified = lines.iter().find(|l| l.content == "process_data(input, options)").unwrap();
        assert!(!modified.inline_spans.is_empty());
    }

    #[test]
    fn test_multiple_modifications_show_merged() {
        let base = "line1\nprocess_item(data1)\nline3\nprocess_item(data2)\nline5";
        let working = "line1\nprocess_item(data1, options)\nline3\nprocess_item(data2, options)\nline5";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();

        assert_eq!(with_spans.len(), 2);
        assert_eq!(with_spans[0].content, "process_item(data1, options)");
        assert_eq!(with_spans[1].content, "process_item(data2, options)");
    }

    #[test]
    fn test_committed_modification_shows_merged() {
        let base = "line1\nfunction getData()\nline3";
        let head = "line1\nfunction getData(params)\nline3";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(with_spans.len(), 1);
        assert_eq!(with_spans[0].content, "function getData(params)");

        let changed: Vec<_> = with_spans[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Committed))
            .collect();
        assert!(!changed.is_empty());
    }

    #[test]
    fn test_committed_modification_with_additions_before() {
        // Simulates: new lines added at top, AND existing lines modified
        // This is the "workon" bug scenario - realistic file structure
        let base = r#"layout {
    pane size=1 borderless=true {
        plugin location="tab-bar"
    }
    pane split_direction="vertical" {
        pane split_direction="horizontal" size="50%" {
            pane size="70%" {
                command "claude"
            }
            pane size="30%" {
            }
        }
        pane size="50%" {
            command "branchdiff"
        }
    }
    pane size=1 borderless=true {
        plugin location="status-bar"
    }
}"#;

        let head = r#"keybinds {
    unbind "Alt f"
}

layout {
    pane size=1 borderless=true {
        plugin location="tab-bar"
    }
    pane split_direction="vertical" {
        pane split_direction="horizontal" size="50%" {
            pane size="80%" {
                command "claude"
            }
            pane size="20%" {
            }
        }
        pane size="50%" {
            command "branchdiff"
        }
    }
    pane size=1 borderless=true {
        plugin location="status-bar"
    }
}"#;

        let diff = compute_file_diff_v2_with_inline("workon.kdl", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        // The "keybinds" lines should be Committed additions
        let additions: Vec<_> = lines.iter()
            .filter(|l| l.source == LineSource::Committed && l.prefix == '+')
            .collect();
        assert!(additions.iter().any(|l| l.content.contains("keybinds")),
            "Should have committed addition for 'keybinds', got additions: {:?}",
            additions.iter().map(|l| &l.content).collect::<Vec<_>>());

        // The modified line (70% -> 80%) should show as modified with inline spans
        // It should NOT be shown as Base
        let modified_line = lines.iter()
            .find(|l| l.content.contains("80%"));
        assert!(modified_line.is_some(), "Should have a line containing '80%'");

        let modified = modified_line.unwrap();
        // Modified lines show as Base (gray context) with inline highlighting
        assert_eq!(modified.source, LineSource::Base,
            "Modified line '{}' should be Base (with inline highlighting), not {:?}",
            modified.content, modified.source);
        // Must have old_content set for inline diff computation
        assert!(modified.old_content.is_some(),
            "Modified line should have old_content set");
        assert!(!modified.inline_spans.is_empty(),
            "Modified line '{}' should have inline spans showing the change from 70% to 80%",
            modified.content);

        // Also check the 30% -> 20% modification
        let modified_line_2 = lines.iter()
            .find(|l| l.content.contains("20%"));
        assert!(modified_line_2.is_some(), "Should have a line containing '20%'");

        let modified2 = modified_line_2.unwrap();
        assert_eq!(modified2.source, LineSource::Base,
            "Modified line '{}' should be Base (with inline highlighting), not {:?}",
            modified2.content, modified2.source);
        assert!(modified2.old_content.is_some(),
            "Modified line should have old_content set");
        assert!(!modified2.inline_spans.is_empty(),
            "Modified line '{}' should have inline spans showing the change from 30% to 20%",
            modified2.content);
    }

    #[test]
    fn test_staged_modification_shows_merged() {
        let base = "line1\nfunction getData()\nline3";
        let index = "line1\nfunction getData(params)\nline3";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(index), Some(index));
        let lines = content_lines(&diff);

        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(with_spans.len(), 1);
        assert_eq!(with_spans[0].content, "function getData(params)");

        let changed: Vec<_> = with_spans[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Staged))
            .collect();
        assert!(!changed.is_empty());
    }

    #[test]
    fn test_context_lines_preserved() {
        let base = "line1\nline2\nline3\nline4\nline5";
        let working = "line1\nline2\nmodified\nline4\nline5";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let pure_context: Vec<_> = lines.iter()
            .filter(|l| l.source == LineSource::Base && l.inline_spans.is_empty())
            .collect();

        assert_eq!(pure_context.len(), 4);
        assert!(pure_context.iter().all(|l| l.prefix == ' '));
    }

    #[test]
    fn test_line_numbers_correct_after_deletion() {
        let base = "line1\nto_delete\nline3";
        let working = "line1\nline3";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        assert!(deleted.iter().all(|l| l.line_number.is_none()));

        let with_numbers: Vec<_> = lines.iter()
            .filter(|l| l.line_number.is_some())
            .collect();

        assert_eq!(with_numbers.len(), 2);
        assert_eq!(with_numbers[0].line_number, Some(1));
        assert_eq!(with_numbers[1].line_number, Some(2));
    }

    #[test]
    fn test_line_numbers_correct_after_addition() {
        let base = "line1\nline2";
        let working = "line1\nnew_line\nline2";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let with_numbers: Vec<_> = lines.iter()
            .filter(|l| l.line_number.is_some())
            .map(|l| (l.content.as_str(), l.line_number.unwrap()))
            .collect();

        assert_eq!(with_numbers, vec![
            ("line1", 1),
            ("new_line", 2),
            ("line2", 3),
        ]);
    }

    #[test]
    fn test_modify_committed_line_in_working_tree() {
        let base = "line1\n";
        let head = "line1\ncommitted line\n";
        let working = "line1\ncommitted line # with comment\n";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(head), Some(working));
        let lines = content_lines(&diff);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].content, "committed line # with comment");

        let unchanged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() && !s.is_deletion)
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        assert!(!unchanged_spans.is_empty());
        assert!(!unstaged_spans.is_empty());
    }

    #[test]
    fn test_modify_staged_line_in_working_tree() {
        let base = "line1\n";
        let head = "line1\n";
        let index = "line1\nstaged line\n";
        let working = "line1\nstaged line modified\n";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].content, "staged line modified");

        let unchanged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() && !s.is_deletion)
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        assert!(!unchanged_spans.is_empty());
        assert!(!unstaged_spans.is_empty());
    }

    #[test]
    fn test_modify_base_line_in_commit() {
        let base = "do_thing(data)\n";
        let head = "do_thing(data, params)\n";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(with_spans.len(), 1);
        assert_eq!(with_spans[0].content, "do_thing(data, params)");

        let changed: Vec<_> = with_spans[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Committed))
            .collect();
        assert!(!changed.is_empty());
    }

    #[test]
    fn test_chain_of_modifications() {
        let base = "original\n";
        let head = "committed version\n";
        let index = "staged version\n";
        let working = "working version\n";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(with_spans.len(), 1);
        assert_eq!(with_spans[0].content, "working version");

        let changed: Vec<_> = with_spans[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();
        assert!(!changed.is_empty());
    }

    #[test]
    fn test_committed_line_unchanged_through_stages() {
        let base = "line1\n";
        let head = "line1\ncommitted line\n";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        let added: Vec<_> = lines.iter().filter(|l| l.prefix == '+').collect();

        assert_eq!(added.len(), 1);
        assert_eq!(added[0].content, "committed line");
        assert_eq!(added[0].source, LineSource::Committed);
    }

    #[test]
    fn test_staged_line_unchanged_in_working() {
        let base = "line1\n";
        let head = "line1\n";
        let index = "line1\nstaged line\n";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(index), Some(index));
        let lines = content_lines(&diff);

        let added: Vec<_> = lines.iter().filter(|l| l.prefix == '+').collect();

        assert_eq!(added.len(), 1);
        assert_eq!(added[0].content, "staged line");
        assert_eq!(added[0].source, LineSource::Staged);
    }

    #[test]
    fn test_inline_diff_merged_simple_addition() {
        let result = compute_inline_diff_merged("do_thing(data)", "do_thing(data, parameters)", LineSource::Unstaged);

        assert!(result.is_meaningful);
        assert!(!result.spans.is_empty());

        let changed: Vec<_> = result.spans.iter().filter(|s| s.source.is_some()).collect();
        let unchanged: Vec<_> = result.spans.iter().filter(|s| s.source.is_none()).collect();

        assert!(!changed.is_empty());
        assert!(!unchanged.is_empty());

        let changed_text: String = changed.iter().map(|s| s.text.as_str()).collect();
        assert!(changed_text.contains(", parameters"));

        let unchanged_text: String = unchanged.iter().map(|s| s.text.as_str()).collect();
        assert!(unchanged_text.contains("do_thing(data"));
    }

    #[test]
    fn test_inline_diff_merged_modification() {
        let result = compute_inline_diff_merged("hello world", "hello earth", LineSource::Unstaged);

        assert!(result.is_meaningful);

        let changed: Vec<_> = result.spans.iter().filter(|s| s.source.is_some()).collect();
        let unchanged: Vec<_> = result.spans.iter().filter(|s| s.source.is_none()).collect();

        assert!(!changed.is_empty());
        assert!(!unchanged.is_empty());

        let unchanged_text: String = unchanged.iter().map(|s| s.text.as_str()).collect();
        assert!(unchanged_text.contains("hello "));
    }

    #[test]
    fn test_inline_diff_merged_no_change() {
        let result = compute_inline_diff_merged("unchanged line", "unchanged line", LineSource::Unstaged);

        let changed: Vec<_> = result.spans.iter().filter(|s| s.source.is_some()).collect();
        assert!(changed.is_empty());
    }

    #[test]
    fn test_inline_diff_merged_complete_replacement() {
        let result = compute_inline_diff_merged("abc", "xyz", LineSource::Unstaged);

        assert!(!result.is_meaningful);

        let deleted: Vec<_> = result.spans.iter().filter(|s| s.is_deletion).collect();
        let inserted: Vec<_> = result.spans.iter().filter(|s| !s.is_deletion && s.source.is_some()).collect();
        let unchanged: Vec<_> = result.spans.iter().filter(|s| s.source.is_none()).collect();

        assert!(!deleted.is_empty());
        assert!(!inserted.is_empty());
        assert!(unchanged.is_empty());

        let deleted_text: String = deleted.iter().map(|s| s.text.as_str()).collect();
        let inserted_text: String = inserted.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(deleted_text, "abc");
        assert_eq!(inserted_text, "xyz");
    }

    #[test]
    fn test_inline_diff_not_meaningful_falls_back_to_pair() {
        let base = "abcdefgh\n";
        let working = "xyz12345\n";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        let added: Vec<_> = lines.iter().filter(|l| l.prefix == '+').collect();

        assert_eq!(deleted.len(), 1);
        assert_eq!(added.len(), 1);
        assert_eq!(deleted[0].content, "abcdefgh");
        assert_eq!(added[0].content, "xyz12345");
    }

    #[test]
    fn test_block_of_changes_no_inline_merge() {
        let base = "context\nalpha: aaa,\nbeta: bbb,\ngamma: ccc,\nend";
        let working = "context\nxray: xxx,\nyankee: yyy,\nzulu: zzz,\nend";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        let added: Vec<_> = lines.iter().filter(|l| l.prefix == '+').collect();
        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();

        assert_eq!(deleted.len(), 3);
        assert_eq!(added.len(), 3);
        assert_eq!(merged.len(), 0);

        for line in &added {
            assert!(line.inline_spans.is_empty());
        }
    }

    #[test]
    fn test_single_line_modification_with_context_shows_inline() {
        let base = "before\ndescribed_class.new(bond).execute\nafter";
        let working = "before\ndescribed_class.new(bond).execute # and add some color commentary\nafter";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        assert_eq!(deleted.len(), 0);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);

        assert!(merged[0].content.contains("# and add some color commentary"));

        let unchanged: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none())
            .collect();
        let changed: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_some())
            .collect();

        assert!(!unchanged.is_empty());
        assert!(!changed.is_empty());

        let unchanged_text: String = unchanged.iter().map(|s| s.text.as_str()).collect();
        assert!(unchanged_text.contains("described_class.new(bond).execute"));
    }

    #[test]
    fn test_single_line_committed_modification_shows_inline() {
        let base = "before\ndescribed_class.new(bond).execute\nafter";
        let head = "before\ndescribed_class.new(bond).execute # and add some color commentary\nafter";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        assert_eq!(deleted.len(), 0);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);

        assert!(merged[0].content.contains("# and add some color commentary"));

        let unchanged: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none())
            .collect();
        let changed: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_some())
            .collect();

        assert!(!unchanged.is_empty());
        assert!(!changed.is_empty());

        let committed_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Committed))
            .collect();
        assert!(!committed_spans.is_empty());

        let unchanged_text: String = unchanged.iter().map(|s| s.text.as_str()).collect();
        assert!(unchanged_text.contains("described_class.new(bond).execute"));
    }

    #[test]
    fn test_modification_with_adjacent_empty_line_inserts_shows_inline() {
        let base = "before\ndescribed_class.new(bond).execute\nafter";
        let head = "before\n\ndescribed_class.new(bond).execute # comment\n\nafter";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);

        assert!(merged[0].content.contains("# comment"));

        let unchanged: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none())
            .collect();
        let changed: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_some())
            .collect();

        assert!(!unchanged.is_empty());
        assert!(!changed.is_empty());
    }

    #[test]
    fn test_unstaged_modification_of_committed_line_shows_inline() {
        let base = "before\nafter";
        let head = "before\ndescribed_class.new(bond).execute\nafter";
        let index = "before\ndescribed_class.new(bond).execute\nafter";
        let working = "before\ndescribed_class.new(bond).execute # and add some color commentary\nafter";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);

        assert_eq!(merged[0].content, "described_class.new(bond).execute # and add some color commentary");

        let unchanged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() && !s.is_deletion)
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        assert!(!unchanged_spans.is_empty());
        let unchanged_text: String = unchanged_spans.iter().map(|s| s.text.as_str()).collect();
        assert!(unchanged_text.contains("described_class.new(bond).execute"));

        assert!(!unstaged_spans.is_empty());
        let unstaged_text: String = unstaged_spans.iter().map(|s| s.text.as_str()).collect();
        assert!(unstaged_text.contains("# and add some color commentary"));
    }

    #[test]
    fn test_unstaged_modification_of_base_line_shows_gray_and_yellow() {
        let base = "before\noriginal_code()\nafter";
        let head = "before\noriginal_code()\nafter";
        let index = "before\noriginal_code()\nafter";
        let working = "before\noriginal_code() # added comment\nafter";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);

        let base_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() || s.source == Some(LineSource::Base))
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        let committed_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Committed))
            .collect();
        assert!(committed_spans.is_empty(), "line from master shouldn't be Committed");

        assert!(!base_spans.is_empty());
        assert!(!unstaged_spans.is_empty());

        let unstaged_text: String = unstaged_spans.iter().map(|s| s.text.as_str()).collect();
        assert!(unstaged_text.contains("# added comment"));
    }

    #[test]
    fn test_duplicate_lines_correct_source_attribution() {
        let base = "context 'first' do\n  it 'test' do\n  end\nend\n";
        let head = "context 'first' do\n  it 'test' do\n  end\n  it 'new test' do\n  end\nend\n";
        let index = head;
        let working = "context 'first' do\n  it 'test' do\n  end\n  it 'new test' do\n  end # added comment\nend\n";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);

        let unchanged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() && !s.is_deletion)
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        assert!(!unchanged_spans.is_empty());
        assert!(!unstaged_spans.is_empty());
    }

    #[test]
    fn test_duplicate_lines_earlier_base_line_doesnt_affect_committed_line() {
        let base = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end
";
        let head = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end

context 'new' do
  it 'new test' do
    described_class.new(bond).execute
  end
end
";
        let index = head;
        // Modify the SECOND "described_class.new(bond).execute" (the one added in commit)
        let working = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end

context 'new' do
  it 'new test' do
    described_class.new(bond).execute # added comment
  end
end
";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);

        assert!(merged[0].content.contains("described_class.new(bond).execute # added comment"));

        let unchanged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() && !s.is_deletion)
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        assert!(!unchanged_spans.is_empty());

        assert!(!unstaged_spans.is_empty());
        let unstaged_text: String = unstaged_spans.iter().map(|s| s.text.as_str()).collect();
        assert!(unstaged_text.contains("# added comment"));
    }

    #[test]
    fn test_last_test_in_committed_block_shows_committed_not_base() {
        let base = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end
";
        let head = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end

context 'first new' do
  it 'first new test' do
    described_class.new(bond).execute
  end
end

context 'second new' do
  it 'second new test' do
    described_class.new(bond).execute
  end
end

context 'third new' do
  it 'third new test' do
    described_class.new(bond).execute
  end
end
";
        let index = head;
        let working = head;

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let execute_lines: Vec<_> = lines.iter()
            .filter(|l| l.content == "    described_class.new(bond).execute")
            .collect();

        assert_eq!(execute_lines.len(), 4);

        assert_eq!(execute_lines[0].source, LineSource::Base);

        for line in execute_lines.iter().skip(1) {
            assert_eq!(line.source, LineSource::Committed);
        }
    }

    #[test]
    fn test_committed_block_with_shared_end_line() {
        let base = "context 'existing' do
  it 'test' do
    described_class.new(bond).execute
  end
end
";
        let head = "context 'existing' do
  it 'test' do
    described_class.new(bond).execute
  end
end

context 'new' do
  it 'uses bond data' do
    expected_address = bond.principal_mailing_address

    described_class.new(bond).execute

    notice = Commercial::PremiumDueNotice.new(bond)
    expect(notice.pdf_fields_hash[:address]).to eq(expected_address)
  end
end
";
        let index = head;
        let working = head;

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let new_test_lines: Vec<_> = lines.iter()
            .enumerate()
            .filter(|(_, l)| {
                l.content.contains("uses bond data") ||
                l.content.contains("expected_address") ||
                l.content.contains("notice = Commercial") ||
                l.content.contains("expect(notice")
            })
            .collect();

        for (_, line) in &new_test_lines {
            assert_eq!(line.source, LineSource::Committed);
        }

        let execute_lines: Vec<_> = lines.iter()
            .enumerate()
            .filter(|(_, l)| l.content.trim() == "described_class.new(bond).execute")
            .collect();

        assert_eq!(execute_lines.len(), 2);
        assert_eq!(execute_lines[0].1.source, LineSource::Base);
        assert_eq!(execute_lines[1].1.source, LineSource::Committed);
    }

    #[test]
    fn test_blank_line_in_committed_block_shows_committed() {
        let base = "context 'existing' do
  it 'test' do
    existing_code

    described_class.new(bond).execute
  end
end
";
        let head = "context 'existing' do
  it 'test' do
    existing_code

    described_class.new(bond).execute
  end
end

context 'new' do
  it 'new test' do
    expected_address = bond.principal_mailing_address

    described_class.new(bond).execute

    notice = Commercial::PremiumDueNotice.new(bond)
  end
end
";
        let index = head;
        let working = head;

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let new_context_idx = lines.iter().position(|l| l.content.contains("context 'new'"));
        assert!(new_context_idx.is_some());

        let new_context_idx = new_context_idx.unwrap();

        for (i, line) in lines.iter().enumerate().skip(new_context_idx) {
            if line.content.trim().is_empty() {
                assert_eq!(line.source, LineSource::Committed,
                    "Blank line at {} should be Committed", i);
            }
        }
    }

    #[test]
    fn test_third_test_in_block_of_three_shows_committed() {
        let base = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end
";
        let head = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end

context 'first new' do
  it 'first test' do
    expected = bond.first_attribute

    described_class.new(bond).execute

    expect(result).to eq(expected)
  end
end

context 'second new' do
  it 'second test' do
    expected = bond.second_attribute

    described_class.new(bond).execute

    expect(result).to eq(expected)
  end
end

context 'third new' do
  it 'third test' do
    expected = bond.third_attribute

    described_class.new(bond).execute

    expect(result).to eq(expected)
  end
end
";
        let index = head;
        let working = head;

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let third_context_idx = lines.iter().position(|l| l.content.contains("context 'third new'"));
        assert!(third_context_idx.is_some());
        let third_context_idx = third_context_idx.unwrap();

        for (_, line) in lines.iter().enumerate().skip(third_context_idx) {
            if line.content.contains("third test") ||
               line.content.contains("third_attribute") ||
               line.content.contains("described_class") ||
               line.content.contains("expect(result)") {
                assert_eq!(line.source, LineSource::Committed);
            }
        }

        let execute_lines: Vec<_> = lines.iter().enumerate()
            .filter(|(_, l)| l.content.trim() == "described_class.new(bond).execute")
            .collect();

        assert_eq!(execute_lines.len(), 4);
        assert_eq!(execute_lines[0].1.source, LineSource::Base);
        assert_eq!(execute_lines[1].1.source, LineSource::Committed);
        assert_eq!(execute_lines[2].1.source, LineSource::Committed);
        assert_eq!(execute_lines[3].1.source, LineSource::Committed);
    }

    #[test]
    fn test_modified_line_shows_as_single_merged_line() {
        let base = "do_thing(data)\n";
        let working = "do_thing(data, parameters)\n";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        let modified: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();

        assert_eq!(deleted.len(), 0);
        assert_eq!(modified.len(), 1);
        assert_eq!(modified[0].content, "do_thing(data, parameters)");

        let changed: Vec<_> = modified[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();
        assert!(!changed.is_empty());

        let changed_text: String = changed.iter().map(|s| s.text.as_str()).collect();
        assert!(changed_text.contains(", parameters"));
    }

    #[test]
    fn test_new_line_addition_no_inline_spans() {
        let base = "line1\n";
        let working = "line1\nnew line\n";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let added: Vec<_> = lines.iter().filter(|l| l.prefix == '+').collect();
        assert_eq!(added.len(), 1);
        assert!(added[0].inline_spans.is_empty());
    }

    #[test]
    fn test_pure_deletion_still_shows_minus() {
        let base = "line1\nto_delete\nline3\n";
        let working = "line1\nline3\n";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].content, "to_delete");
    }

    #[test]
    fn test_two_adjacent_committed_modifications() {
        let base = r#"            principal_zip: "00000",
            effective_date: "2022-08-30",
            expiration_date: "2024-08-30",
"#;
        let head = r#"            principal_zip: "00000",
            effective_date: "2023-08-30",
            expiration_date: "2025-08-30",
"#;

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        let effective_lines: Vec<_> = lines.iter()
            .filter(|l| l.content.contains("effective_date"))
            .collect();
        let expiration_lines: Vec<_> = lines.iter()
            .filter(|l| l.content.contains("expiration_date"))
            .collect();

        assert_eq!(effective_lines.len(), 1);
        assert!(effective_lines[0].content.contains("2023"));
        assert!(!effective_lines[0].inline_spans.is_empty());

        assert_eq!(expiration_lines.len(), 1);
        assert!(expiration_lines[0].content.contains("2025"));
        assert!(!expiration_lines[0].inline_spans.is_empty());

        let effective_has_committed = effective_lines[0].inline_spans.iter()
            .any(|s| s.source == Some(LineSource::Committed));
        let expiration_has_committed = expiration_lines[0].inline_spans.iter()
            .any(|s| s.source == Some(LineSource::Committed));

        assert!(effective_has_committed);
        assert!(expiration_has_committed);
    }

    #[test]
    fn test_deletion_positioned_correctly_with_insertions_before() {
        let base = "line1\nline2\nline3\nto_delete\nline5";
        let working = "line1\nline2\nNEW_LINE\nline3\nline5";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let line_contents: Vec<&str> = lines.iter().map(|l| l.content.as_str()).collect();

        let new_line_pos = line_contents.iter().position(|&c| c == "NEW_LINE").unwrap();
        let to_delete_pos = line_contents.iter().position(|&c| c == "to_delete").unwrap();
        let line3_pos = line_contents.iter().position(|&c| c == "line3").unwrap();

        assert!(to_delete_pos > line3_pos);
        assert!(to_delete_pos > new_line_pos);

        let deleted_line = &lines[to_delete_pos];
        assert_eq!(deleted_line.prefix, '-');
    }

    #[test]
    fn test_deletion_before_insertion_at_same_position() {
        let base = "def principal_mailing_address\n  commercial_renewal.principal_mailing_address\nend";
        let working = "def principal_mailing_address\n  \"new content\"\nend";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let line_contents: Vec<&str> = lines.iter().map(|l| l.content.as_str()).collect();
        let prefixes: Vec<char> = lines.iter().map(|l| l.prefix).collect();

        let deleted_pos = line_contents.iter().position(|&c| c.contains("commercial_renewal")).unwrap();
        let inserted_pos = line_contents.iter().position(|&c| c.contains("new content")).unwrap();

        assert!(deleted_pos < inserted_pos);
        assert_eq!(prefixes[deleted_pos], '-');
        assert_eq!(prefixes[inserted_pos], '+');
    }

    #[test]
    fn test_inline_diff_thresholds() {
        let test_cases = [
            ("  body_line", "  \"new body\"", false),
            ("do_thing(data)", "do_thing(data, parameters)", true),
            ("hello world", "hello earth", true),
        ];

        for (old, new, expected) in test_cases {
            let result = compute_inline_diff_merged(old, new, LineSource::Unstaged);
            assert_eq!(result.is_meaningful, expected);
        }
    }

    #[test]
    fn test_deletion_appears_after_preceding_context_line() {
        let base = "def foo\n  body_line\nend";
        let working = "def foo\n  \"new body\"\nend";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let line_contents: Vec<&str> = lines.iter().map(|l| l.content.as_str()).collect();

        let def_pos = line_contents.iter().position(|&c| c.contains("def foo")).unwrap();
        let deleted_pos = line_contents.iter().position(|&c| c.contains("body_line")).unwrap();
        let inserted_pos = line_contents.iter().position(|&c| c.contains("new body")).unwrap();
        let end_pos = line_contents.iter().position(|&c| c == "end").unwrap();

        assert!(deleted_pos > def_pos);
        assert!(deleted_pos < inserted_pos);
        assert!(deleted_pos < end_pos);

        assert_eq!(def_pos, 0);
        assert_eq!(deleted_pos, 1);
        assert_eq!(inserted_pos, 2);
        assert_eq!(end_pos, 3);
    }

    #[test]
    fn test_deletion_after_modified_line() {
        let base = "def principal_mailing_address\n  commercial_renewal.principal_mailing_address\nend";
        let working = "def pribond_descripal_mailtiong_address\n  \"new content\"\nend";

        let diff = compute_file_diff_v2_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let line_contents: Vec<&str> = lines.iter().map(|l| l.content.as_str()).collect();
        let prefixes: Vec<char> = lines.iter().map(|l| l.prefix).collect();

        let deleted_principal_pos = line_contents.iter().position(|&c| c.contains("principal_mailing_address")).unwrap();
        let inserted_pribond_pos = line_contents.iter().position(|&c| c.contains("pribond")).unwrap();
        let deleted_commercial_pos = line_contents.iter().position(|&c| c.contains("commercial_renewal")).unwrap();
        let inserted_new_content_pos = line_contents.iter().position(|&c| c.contains("new content")).unwrap();
        let end_pos = line_contents.iter().position(|&c| c == "end").unwrap();

        assert_eq!(prefixes[deleted_principal_pos], '-', "principal should be deleted");
        assert_eq!(prefixes[inserted_pribond_pos], '+', "pribond should be inserted");
        assert_eq!(prefixes[deleted_commercial_pos], '-', "commercial_renewal should be deleted");
        assert_eq!(prefixes[inserted_new_content_pos], '+', "new content should be inserted");
        assert_eq!(prefixes[end_pos], ' ', "end should be unchanged context");

        assert!(deleted_principal_pos < deleted_commercial_pos, "both deletions should come together");
        assert!(deleted_commercial_pos < inserted_pribond_pos, "deletions should come before insertions");
        assert!(inserted_pribond_pos < inserted_new_content_pos, "both insertions should come together");
    }

    #[test]
    fn test_unstaged_modification_of_newly_committed_method_appears_in_correct_position() {
        let base = "def abeyance_required?
  abeyance? || active?
end

def from_domino?
  legacy_unid.present?
end
";
        let head = "def abeyance_required?
  abeyance? || active?
end

def can_request_letter_of_bondability?
  !commercial? && (active? || abeyance?)
end

def from_domino?
  legacy_unid.present?
end
";
        let index = head;
        let working = "def abeyance_required?
  abeyance? || active?
end

def can_request_letter_of_bondability?
  !inactive? && status != \"Destroy\"
end

def from_domino?
  legacy_unid.present?
end
";

        let diff = compute_file_diff_v2_with_inline("principal.rb", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let abeyance_pos = lines.iter().position(|l| l.content.contains("abeyance_required")).unwrap();
        let can_request_pos = lines.iter().position(|l| l.content.contains("can_request_letter_of_bondability")).unwrap();
        let from_domino_pos = lines.iter().position(|l| l.content.contains("from_domino")).unwrap();

        let commercial_line_pos = lines.iter().position(|l| l.content.contains("!commercial?"));
        let inactive_line_pos = lines.iter().position(|l| l.content.contains("!inactive?"));

        assert!(can_request_pos > abeyance_pos, "can_request method should come after abeyance_required");
        assert!(can_request_pos < from_domino_pos, "can_request method should come before from_domino");

        if let Some(commercial_pos) = commercial_line_pos {
            assert!(
                commercial_pos > abeyance_pos && commercial_pos < from_domino_pos,
                "!commercial? line should appear between abeyance_required and from_domino, not at pos {} (abeyance={}, from_domino={})",
                commercial_pos, abeyance_pos, from_domino_pos
            );
        }

        if let Some(inactive_pos) = inactive_line_pos {
            assert!(
                inactive_pos > abeyance_pos && inactive_pos < from_domino_pos,
                "!inactive? line should appear between abeyance_required and from_domino, not at pos {} (abeyance={}, from_domino={})",
                inactive_pos, abeyance_pos, from_domino_pos
            );
        }
    }

    #[test]
    fn test_trailing_context_after_addition() {
        let base = "def foo\nend\nend";
        let working = "def foo\nnew_line\nend\nend";

        let diff = compute_file_diff_v2_with_inline("test.rb", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        assert_eq!(lines.len(), 4);

        assert_eq!(lines[0].content, "def foo");
        assert_eq!(lines[0].source, LineSource::Base);

        assert_eq!(lines[1].content, "new_line");
        assert_eq!(lines[1].prefix, '+');

        assert_eq!(lines[2].content, "end");
        assert_eq!(lines[2].source, LineSource::Base);

        assert_eq!(lines[3].content, "end");
        assert_eq!(lines[3].source, LineSource::Base);
    }

    #[test]
    fn test_trailing_context_after_committed_addition() {
        let base = "def foo\nend\nend";
        let head = "def foo\nnew_line\nend\nend";

        let diff = compute_file_diff_v2_with_inline("test.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);
        assert_eq!(lines.len(), 4);

        assert_eq!(lines[0].content, "def foo");
        assert_eq!(lines[0].source, LineSource::Base);

        assert_eq!(lines[1].content, "new_line");
        assert_eq!(lines[1].source, LineSource::Committed);
        assert_eq!(lines[1].prefix, '+');

        assert_eq!(lines[2].content, "end");
        assert_eq!(lines[2].source, LineSource::Base);

        assert_eq!(lines[3].content, "end");
        assert_eq!(lines[3].source, LineSource::Base);
    }

    #[test]
    fn test_addition_at_end_of_file_with_trailing_context() {
        let base = "class Foo\n  def bar\n  end\nend";
        let head = "class Foo\n  def bar\n    new_line\n  end\nend";

        let diff = compute_file_diff_v2_with_inline("test.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        assert_eq!(lines.len(), 5);

        assert_eq!(lines[0].content, "class Foo");
        assert_eq!(lines[1].content, "  def bar");
        assert_eq!(lines[2].content, "    new_line");
        assert_eq!(lines[2].source, LineSource::Committed);
        assert_eq!(lines[3].content, "  end");
        assert_eq!(lines[3].source, LineSource::Base);
        assert_eq!(lines[4].content, "end");
        assert_eq!(lines[4].source, LineSource::Base);
    }

    #[test]
    fn test_addition_before_two_trailing_ends() {
        let base = "do\n  body\nend\nend";
        let head = "do\n  body\n  new_end\nend\nend";

        let diff = compute_file_diff_v2_with_inline("test.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        assert_eq!(lines.len(), 5);

        assert_eq!(lines[0].content, "do");
        assert_eq!(lines[0].source, LineSource::Base);

        assert_eq!(lines[1].content, "  body");
        assert_eq!(lines[1].source, LineSource::Base);

        assert_eq!(lines[2].content, "  new_end");
        assert_eq!(lines[2].source, LineSource::Committed);
        assert_eq!(lines[2].prefix, '+');

        assert_eq!(lines[3].content, "end");
        assert_eq!(lines[3].source, LineSource::Base);

        assert_eq!(lines[4].content, "end");
        assert_eq!(lines[4].source, LineSource::Base);
    }

    #[test]
    fn test_final_file_ends_with_addition() {
        let base = "do\n  body\nend";
        let head = "do\n  body\nend\n  extra";

        let diff = compute_file_diff_v2_with_inline("test.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        assert_eq!(lines.len(), 4);

        assert_eq!(lines[0].content, "do");
        assert_eq!(lines[0].source, LineSource::Base);

        assert_eq!(lines[1].content, "  body");
        assert_eq!(lines[1].source, LineSource::Base);

        assert_eq!(lines[2].content, "end");
        assert_eq!(lines[2].source, LineSource::Base);

        assert_eq!(lines[3].content, "  extra");
        assert_eq!(lines[3].source, LineSource::Committed);
        assert_eq!(lines[3].prefix, '+');
    }

    #[test]
    fn test_file_without_trailing_newline() {
        let base = "line1\nline2\nend\nend\nend";
        let head = "line1\nline2\nnew_line\nend\nend\nend";

        let diff = compute_file_diff_v2_with_inline("test.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0].content, "line1");
        assert_eq!(lines[1].content, "line2");
        assert_eq!(lines[2].content, "new_line");
        assert_eq!(lines[2].source, LineSource::Committed);
        assert_eq!(lines[3].content, "end");
        assert_eq!(lines[3].source, LineSource::Base);
        assert_eq!(lines[4].content, "end");
        assert_eq!(lines[4].source, LineSource::Base);
        assert_eq!(lines[5].content, "end");
        assert_eq!(lines[5].source, LineSource::Base);
    }

    #[test]
    fn test_exact_scenario_from_bug_report() {
        // EXACT scenario from the bug report:
        // Base file ends with:
        //   ...some tests...
        //   end  <- end of describe block
        //   end  <- end of RSpec.describe
        //
        // Head adds a new test BEFORE the final two ends:
        //   ...some tests...
        //   (empty line)
        //   it "new test" do
        //     expect(...)
        //   end   <- end of new test (ADDED)
        //   end   <- end of describe block (BASE)
        //   end   <- end of RSpec.describe (BASE)

        let base = r##"  describe "#method" do
    it "test 1" do
      expect(true).to be(true)
    end
  end
end"##;

        let head = r##"  describe "#method" do
    it "test 1" do
      expect(true).to be(true)
    end

    it "new test" do
      expect(false).to be(false)
    end
  end
end"##;

        let diff = compute_file_diff_v2_with_inline("spec.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        // Expected:
        // [0] describe "#method" do       (base)
        // [1]   it "test 1" do            (base)
        // [2]     expect(true)...         (base)
        // [3]   end                       (base)
        // [4] (empty line)                (committed +)
        // [5]   it "new test" do          (committed +)
        // [6]     expect(false)...        (committed +)
        // [7]   end                       (committed +)
        // [8] end                         (base) <-- trailing
        // [9] end                         (base) <-- trailing

        assert!(lines.len() >= 10, "Should have at least 10 lines, got {}", lines.len());

        // Check the trailing base lines exist
        let last_two: Vec<_> = lines.iter().rev().take(2).collect();
        assert_eq!(last_two[0].content, "end", "Last line should be 'end'");
        assert_eq!(last_two[0].source, LineSource::Base, "Last line should be Base");
        // Second to last is "  end" (with indent)
        assert_eq!(last_two[1].content, "  end", "Second to last should be '  end'");
        assert_eq!(last_two[1].source, LineSource::Base, "Second to last should be Base");

        // Check that the added "end" (end of new test) is Committed
        let added_end = lines.iter().find(|l| l.content == "    end" && l.source == LineSource::Committed);
        assert!(added_end.is_some(), "Should have a committed '    end' line");

        // CRITICAL: All 10 lines should be present
        assert_eq!(lines.len(), 10, "All 10 lines should be in the diff output");
    }

    #[test]
    fn test_staging_changes_line_source_from_unstaged_to_staged() {
        let base = "line1\nline2";
        let modified = "line1\nline2\nline3";

        // Before staging: change is only in working tree
        let diff_before = compute_file_diff_v2(
            "test.txt",
            Some(base),
            Some(base),
            Some(base),      // index same as base
            Some(modified),  // working has the change
            None,
        );

        let unstaged_lines: Vec<_> = diff_before.lines.iter()
            .filter(|l| l.source == LineSource::Unstaged && l.content == "line3")
            .collect();
        assert_eq!(unstaged_lines.len(), 1, "line3 should be Unstaged before staging");

        // After staging: change is in index and working tree
        let diff_after = compute_file_diff_v2(
            "test.txt",
            Some(base),
            Some(base),
            Some(modified),  // index now has the change
            Some(modified),  // working same as index
            None,
        );

        let staged_lines: Vec<_> = diff_after.lines.iter()
            .filter(|l| l.source == LineSource::Staged && l.content == "line3")
            .collect();
        assert_eq!(staged_lines.len(), 1, "line3 should be Staged after staging");

        // Verify line3 is NOT unstaged after staging
        let still_unstaged: Vec<_> = diff_after.lines.iter()
            .filter(|l| l.source == LineSource::Unstaged && l.content == "line3")
            .collect();
        assert_eq!(still_unstaged.len(), 0, "line3 should NOT be Unstaged after staging");
    }
}
