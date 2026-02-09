//! Cancellation detection for diff lines.
//!
//! "Canceled" lines are those that were added in one stage but removed in a later stage.
//! For example, a line added in a commit but removed in staging is "CanceledCommitted".

use std::collections::HashMap;

use super::provenance::{find_sources, survives_chain, survives_in};
use super::{DiffLine, LineSource};

/// Check if an index line survives to working (by provenance or modification).
pub(super) fn index_line_in_working(
    index_idx: usize,
    working_from_index: &[Option<usize>],
    index_working_mods: &HashMap<usize, (usize, &str)>,
) -> bool {
    if survives_in(working_from_index, index_idx) {
        return true;
    }
    index_working_mods
        .values()
        .any(|(src_idx, _)| *src_idx == index_idx)
}

/// Collect canceled lines for the simple case (base == working).
/// Returns DiffLines for both canceled committed and canceled staged lines.
pub(super) fn collect_canceled_simple(
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
        if !survives_chain(head_idx, index_from_head, working_from_index) {
            result.push(
                DiffLine::new(
                    LineSource::CanceledCommitted,
                    head_line.trim_end().to_string(),
                    '±',
                    None,
                )
                .with_file_path(path),
            );
        }
    }

    // Canceled staged: lines added in index but not in working
    for (index_idx, index_line) in index_lines.iter().enumerate() {
        if index_from_head.get(index_idx).copied().flatten().is_some() {
            continue;
        }
        if !survives_in(working_from_index, index_idx) {
            result.push(
                DiffLine::new(
                    LineSource::CanceledStaged,
                    index_line.trim_end().to_string(),
                    '±',
                    None,
                )
                .with_file_path(path),
            );
        }
    }

    result
}

/// Collect canceled committed lines (added in HEAD but not in working).
/// Returns (head_idx, content) pairs for ordering.
pub(super) fn collect_canceled_committed(
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

        // Check via direct provenance
        let in_working_via_provenance = find_sources(index_from_head, head_idx)
            .any(|index_idx| index_line_in_working(index_idx, working_from_index, index_working_mods));

        // Check via modification maps
        let in_working_via_mods = head_index_mods.iter().any(|(index_idx, (src_head_idx, _))| {
            *src_head_idx == head_idx
                && index_line_in_working(*index_idx, working_from_index, index_working_mods)
        });

        if !in_working_via_provenance && !in_working_via_mods {
            result.push((head_idx, head_line.trim_end().to_string()));
        }
    }

    result
}

/// Collect canceled staged lines (added in index but not in working).
/// Returns (index_idx, content) pairs for ordering.
pub(super) fn collect_canceled_staged(
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

/// Find where to insert a canceled line based on its original position.
pub(super) fn find_insertion_position(positions: &[Option<usize>], target_idx: usize) -> usize {
    for (i, &pos) in positions.iter().enumerate().rev() {
        if let Some(p) = pos
            && p < target_idx
        {
            return i + 1;
        }
    }
    positions.len()
}

/// Insert canceled lines at appropriate positions to maintain visual order.
pub(super) fn insert_canceled_lines(
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

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== index_line_in_working tests ====================

    #[test]
    fn test_index_line_in_working_via_provenance() {
        // Index line 0 survives to working line 0
        let working_from_index = vec![Some(0), Some(1)];
        let index_working_mods = HashMap::new();

        assert!(index_line_in_working(0, &working_from_index, &index_working_mods));
        assert!(index_line_in_working(1, &working_from_index, &index_working_mods));
    }

    #[test]
    fn test_index_line_in_working_not_present() {
        // Index line 2 doesn't survive (not in provenance or mods)
        let working_from_index = vec![Some(0), Some(1)];
        let index_working_mods = HashMap::new();

        assert!(!index_line_in_working(2, &working_from_index, &index_working_mods));
    }

    #[test]
    fn test_index_line_in_working_via_modification() {
        // Index line 2 doesn't survive via provenance but is in modification map
        let working_from_index = vec![Some(0), None]; // working line 1 is new
        let mut index_working_mods = HashMap::new();
        // working line 1 was modified from index line 2
        index_working_mods.insert(1, (2, "modified content"));

        assert!(index_line_in_working(2, &working_from_index, &index_working_mods));
    }

    #[test]
    fn test_index_line_in_working_empty_maps() {
        let working_from_index: Vec<Option<usize>> = vec![];
        let index_working_mods = HashMap::new();

        assert!(!index_line_in_working(0, &working_from_index, &index_working_mods));
    }

    // ==================== find_insertion_position tests ====================

    #[test]
    fn test_find_insertion_position_at_end() {
        // All positions are before target, insert at end
        let positions = vec![Some(0), Some(1), Some(2)];
        assert_eq!(find_insertion_position(&positions, 5), 3);
    }

    #[test]
    fn test_find_insertion_position_in_middle() {
        // Target 2 should go after position 1 (index 1) but before position 3 (index 2)
        let positions = vec![Some(0), Some(1), Some(3), Some(4)];
        assert_eq!(find_insertion_position(&positions, 2), 2);
    }

    #[test]
    fn test_find_insertion_position_at_start() {
        // Target 0 with all positions >= target, goes at end (no position < target)
        let positions = vec![Some(1), Some(2), Some(3)];
        assert_eq!(find_insertion_position(&positions, 0), 3);
    }

    #[test]
    fn test_find_insertion_position_with_nones() {
        // None values should be skipped, finds position after Some(0) which is < 2
        let positions = vec![Some(0), None, Some(3), None];
        assert_eq!(find_insertion_position(&positions, 2), 1);
    }

    #[test]
    fn test_find_insertion_position_empty() {
        let positions: Vec<Option<usize>> = vec![];
        assert_eq!(find_insertion_position(&positions, 5), 0);
    }

    #[test]
    fn test_find_insertion_position_all_none() {
        let positions = vec![None, None, None];
        assert_eq!(find_insertion_position(&positions, 5), 3);
    }

    // ==================== collect_canceled_simple tests ====================

    #[test]
    fn test_collect_canceled_simple_no_cancellations() {
        // All lines survive through the chain
        let head_lines = vec!["line1", "line2"];
        let index_lines = vec!["line1", "line2"];
        let head_from_base = vec![Some(0), Some(1)]; // both from base
        let index_from_head = vec![Some(0), Some(1)];
        let working_from_index = vec![Some(0), Some(1)];

        let result = collect_canceled_simple(
            &head_lines,
            &index_lines,
            &head_from_base,
            &index_from_head,
            &working_from_index,
            "test.txt",
        );

        assert!(result.is_empty());
    }

    #[test]
    fn test_collect_canceled_simple_committed_cancellation() {
        // Line added in head (not from base) but removed before working
        let head_lines = vec!["base_line", "added_in_commit"];
        let index_lines = vec!["base_line"]; // added line removed in index
        let head_from_base = vec![Some(0), None]; // second line is new in head
        let index_from_head = vec![Some(0)]; // only first line survives
        let working_from_index = vec![Some(0)];

        let result = collect_canceled_simple(
            &head_lines,
            &index_lines,
            &head_from_base,
            &index_from_head,
            &working_from_index,
            "test.txt",
        );

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source, LineSource::CanceledCommitted);
        assert_eq!(result[0].content, "added_in_commit");
    }

    #[test]
    fn test_collect_canceled_simple_staged_cancellation() {
        // Line added in index (not from head) but removed in working
        let head_lines = vec!["line1"];
        let index_lines = vec!["line1", "staged_line"];
        let head_from_base = vec![Some(0)];
        let index_from_head = vec![Some(0), None]; // second line new in index
        let working_from_index = vec![Some(0)]; // staged line removed

        let result = collect_canceled_simple(
            &head_lines,
            &index_lines,
            &head_from_base,
            &index_from_head,
            &working_from_index,
            "test.txt",
        );

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source, LineSource::CanceledStaged);
        assert_eq!(result[0].content, "staged_line");
    }

    #[test]
    fn test_collect_canceled_simple_both_types() {
        // Both committed and staged cancellations
        let head_lines = vec!["base", "committed_add"];
        let index_lines = vec!["base", "staged_add"];
        let head_from_base = vec![Some(0), None];
        let index_from_head = vec![Some(0), None]; // committed_add removed, staged_add new
        let working_from_index = vec![Some(0)]; // staged_add also removed

        let result = collect_canceled_simple(
            &head_lines,
            &index_lines,
            &head_from_base,
            &index_from_head,
            &working_from_index,
            "test.txt",
        );

        assert_eq!(result.len(), 2);
        let sources: Vec<_> = result.iter().map(|l| l.source).collect();
        assert!(sources.contains(&LineSource::CanceledCommitted));
        assert!(sources.contains(&LineSource::CanceledStaged));
    }

    // ==================== collect_canceled_committed tests ====================

    #[test]
    fn test_collect_canceled_committed_none() {
        // Line from base survives - not canceled
        let head_lines = vec!["line1"];
        let head_from_base = vec![Some(0)]; // from base
        let index_from_head = vec![Some(0)];
        let working_from_index = vec![Some(0)];
        let head_index_mods = HashMap::new();
        let index_working_mods = HashMap::new();

        let result = collect_canceled_committed(
            &head_lines,
            &head_from_base,
            &index_from_head,
            &working_from_index,
            &head_index_mods,
            &index_working_mods,
        );

        assert!(result.is_empty());
    }

    #[test]
    fn test_collect_canceled_committed_via_provenance() {
        // Line added in head, survives via provenance chain
        let head_lines = vec!["new_line"];
        let head_from_base = vec![None]; // not from base
        let index_from_head = vec![Some(0)]; // survives to index
        let working_from_index = vec![Some(0)]; // survives to working
        let head_index_mods = HashMap::new();
        let index_working_mods = HashMap::new();

        let result = collect_canceled_committed(
            &head_lines,
            &head_from_base,
            &index_from_head,
            &working_from_index,
            &head_index_mods,
            &index_working_mods,
        );

        assert!(result.is_empty()); // Not canceled - it survived
    }

    #[test]
    fn test_collect_canceled_committed_detected() {
        // Line added in head but doesn't survive to working
        let head_lines = vec!["added_then_removed"];
        let head_from_base = vec![None]; // not from base
        let index_from_head = vec![Some(0)]; // survives to index
        let working_from_index: Vec<Option<usize>> = vec![]; // but not to working
        let head_index_mods = HashMap::new();
        let index_working_mods = HashMap::new();

        let result = collect_canceled_committed(
            &head_lines,
            &head_from_base,
            &index_from_head,
            &working_from_index,
            &head_index_mods,
            &index_working_mods,
        );

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], (0, "added_then_removed".to_string()));
    }

    #[test]
    fn test_collect_canceled_committed_survives_via_modification() {
        // Line added in head, modified in index->working, so it survives
        let head_lines = vec!["original"];
        let head_from_base = vec![None];
        let index_from_head = vec![Some(0)];
        let working_from_index = vec![None]; // doesn't survive via provenance

        let head_index_mods = HashMap::new();
        let mut index_working_mods = HashMap::new();
        // working line 0 is modified from index line 0
        index_working_mods.insert(0, (0, "modified"));

        let result = collect_canceled_committed(
            &head_lines,
            &head_from_base,
            &index_from_head,
            &working_from_index,
            &head_index_mods,
            &index_working_mods,
        );

        assert!(result.is_empty()); // Not canceled - survived via modification
    }

    // ==================== collect_canceled_staged tests ====================

    #[test]
    fn test_collect_canceled_staged_none() {
        // Line from head survives - not a staged addition
        let index_lines = vec!["line1"];
        let index_from_head = vec![Some(0)]; // from head, not new
        let working_from_index = vec![Some(0)];
        let index_working_mods = HashMap::new();

        let result = collect_canceled_staged(
            &index_lines,
            &index_from_head,
            &working_from_index,
            &index_working_mods,
        );

        assert!(result.is_empty());
    }

    #[test]
    fn test_collect_canceled_staged_detected() {
        // Line added in index but removed in working
        let index_lines = vec!["staged_line"];
        let index_from_head = vec![None]; // new in index
        let working_from_index: Vec<Option<usize>> = vec![]; // removed
        let index_working_mods = HashMap::new();

        let result = collect_canceled_staged(
            &index_lines,
            &index_from_head,
            &working_from_index,
            &index_working_mods,
        );

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], (0, "staged_line".to_string()));
    }

    #[test]
    fn test_collect_canceled_staged_survives_via_modification() {
        // Line added in index, modified in working - survives
        let index_lines = vec!["original"];
        let index_from_head = vec![None]; // new in index
        let working_from_index = vec![None]; // not via provenance

        let mut index_working_mods = HashMap::new();
        index_working_mods.insert(0, (0, "modified")); // but via modification

        let result = collect_canceled_staged(
            &index_lines,
            &index_from_head,
            &working_from_index,
            &index_working_mods,
        );

        assert!(result.is_empty()); // Not canceled - survived via modification
    }

    // ==================== insert_canceled_lines tests ====================

    #[test]
    fn test_insert_canceled_lines_empty() {
        let mut lines = vec![
            DiffLine::new(LineSource::Base, "line1".to_string(), ' ', Some(1)),
        ];
        let canceled: Vec<(usize, String)> = vec![];
        let mut positions = vec![Some(0)];

        insert_canceled_lines(
            &mut lines,
            canceled,
            LineSource::CanceledCommitted,
            "test.txt",
            &mut positions,
        );

        assert_eq!(lines.len(), 1);
        assert_eq!(positions.len(), 1);
    }

    #[test]
    fn test_insert_canceled_lines_single() {
        let mut lines = vec![
            DiffLine::new(LineSource::Base, "line0".to_string(), ' ', Some(1)),
            DiffLine::new(LineSource::Base, "line2".to_string(), ' ', Some(2)),
        ];
        let canceled = vec![(1, "canceled_line1".to_string())];
        let mut positions = vec![Some(0), Some(2)];

        insert_canceled_lines(
            &mut lines,
            canceled,
            LineSource::CanceledCommitted,
            "test.txt",
            &mut positions,
        );

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].source, LineSource::CanceledCommitted);
        assert_eq!(lines[1].content, "canceled_line1");
        assert_eq!(lines[1].prefix, '±');
        assert_eq!(positions, vec![Some(0), Some(1), Some(2)]);
    }

    #[test]
    fn test_insert_canceled_lines_multiple_preserves_order() {
        let mut lines = vec![
            DiffLine::new(LineSource::Base, "line0".to_string(), ' ', Some(1)),
            DiffLine::new(LineSource::Base, "line5".to_string(), ' ', Some(2)),
        ];
        // Insert canceled lines at positions 2 and 3 (in that order)
        let canceled = vec![
            (2, "canceled2".to_string()),
            (3, "canceled3".to_string()),
        ];
        let mut positions = vec![Some(0), Some(5)];

        insert_canceled_lines(
            &mut lines,
            canceled,
            LineSource::CanceledStaged,
            "test.txt",
            &mut positions,
        );

        assert_eq!(lines.len(), 4);
        // Both should be inserted between line0 and line5
        assert_eq!(lines[1].content, "canceled2");
        assert_eq!(lines[2].content, "canceled3");
    }

    #[test]
    fn test_insert_canceled_lines_sets_file_path() {
        let mut lines: Vec<DiffLine> = vec![];
        let canceled = vec![(0, "content".to_string())];
        let mut positions: Vec<Option<usize>> = vec![];

        insert_canceled_lines(
            &mut lines,
            canceled,
            LineSource::CanceledCommitted,
            "path/to/file.rs",
            &mut positions,
        );

        assert_eq!(lines[0].file_path, Some("path/to/file.rs".to_string()));
    }
}
