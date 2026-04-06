use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;

use super::{DiffLine, FileDiff, LineSource};

/// Maximum gap (in non-change lines) allowed within a single block.
const MAX_GAP: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Addition,
    Deletion,
    Canceled,
    Mixed,
}

#[derive(Debug, Clone)]
pub struct ChangeBlock {
    pub range: Range<usize>,
    pub kind: BlockKind,
    pub content_hash: u64,
    pub change_count: usize,
}

fn is_change_line(line: &DiffLine) -> bool {
    line.source.is_change() || line.change_source.is_some()
}

fn classify(lines: &[DiffLine], range: &Range<usize>) -> BlockKind {
    let mut has_add = false;
    let mut has_del = false;
    let mut has_canceled = false;

    for line in &lines[range.clone()] {
        let source = line.change_source.unwrap_or(line.source);
        if source.is_addition() {
            has_add = true;
        } else if source.is_deletion() {
            has_del = true;
        } else if matches!(source, LineSource::CanceledCommitted | LineSource::CanceledStaged) {
            has_canceled = true;
        }
    }

    if has_canceled && !has_add && !has_del {
        BlockKind::Canceled
    } else if has_add && has_del {
        BlockKind::Mixed
    } else if has_del {
        BlockKind::Deletion
    } else {
        BlockKind::Addition
    }
}

fn content_hash(lines: &[DiffLine], range: &Range<usize>) -> u64 {
    let mut hasher = DefaultHasher::new();
    for line in &lines[range.clone()] {
        if is_change_line(line) {
            let trimmed = line.content.trim();
            if !trimmed.is_empty() {
                trimmed.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Extract change blocks from a file's diff lines.
///
/// A change block is a maximal contiguous run of change lines,
/// tolerating up to `MAX_GAP` non-change lines within the run.
/// Returns blocks and annotates each line with its block index.
pub fn extract_blocks(lines: &mut [DiffLine]) -> Vec<ChangeBlock> {
    let mut blocks = Vec::new();
    let mut block_start: Option<usize> = None;
    let mut gap = 0;

    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];

        if line.source == LineSource::FileHeader || line.source == LineSource::Elided {
            if let Some(start) = block_start {
                finalize_block(lines, start, i - gap, &mut blocks);
                block_start = None;
            }
            gap = 0;
            i += 1;
            continue;
        }

        if is_change_line(line) {
            if block_start.is_none() {
                block_start = Some(i);
            }
            gap = 0;
        } else if block_start.is_some() {
            gap += 1;
            if gap > MAX_GAP {
                let start = block_start.expect("checked above");
                // End at the last change line + 1 (exclusive), excluding the gap
                finalize_block(lines, start, i - gap + 1, &mut blocks);
                block_start = None;
                gap = 0;
            }
        }

        i += 1;
    }

    if let Some(start) = block_start {
        let trailing_gap = gap.min(lines.len() - start);
        finalize_block(lines, start, lines.len() - trailing_gap, &mut blocks);
    }

    blocks
}

fn finalize_block(
    lines: &mut [DiffLine],
    start: usize,
    end: usize,
    blocks: &mut Vec<ChangeBlock>,
) {
    if start >= end {
        return;
    }
    // Trim blank/whitespace-only change lines from block edges so that
    // matched blocks on both sides of a move contain the same content.
    let mut trimmed_start = start;
    let mut trimmed_end = end;
    while trimmed_start < trimmed_end && lines[trimmed_start].content.trim().is_empty() {
        trimmed_start += 1;
    }
    while trimmed_end > trimmed_start && lines[trimmed_end - 1].content.trim().is_empty() {
        trimmed_end -= 1;
    }
    if trimmed_start >= trimmed_end {
        return;
    }
    let range = trimmed_start..trimmed_end;
    let change_count = lines[range.clone()]
        .iter()
        .filter(|l| is_change_line(l))
        .count();
    if change_count == 0 {
        return;
    }

    let kind = classify(lines, &range);
    let hash = content_hash(lines, &range);
    let block_idx = blocks.len();

    for line in &mut lines[range.clone()] {
        if is_change_line(line) || line.source == LineSource::Base {
            line.block_idx = Some(block_idx);
        }
    }

    blocks.push(ChangeBlock {
        range,
        kind,
        content_hash: hash,
        change_count,
    });
}

/// A matched pair of blocks: code deleted in one place and added in another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockMatch {
    /// (file index, block index) of the deletion block
    pub deletion: (usize, usize),
    /// (file index, block index) of the addition block
    pub addition: (usize, usize),
}

/// Match deletion blocks against addition blocks across all files.
///
/// Uses exact content hash matching. Only unambiguous 1:1 matches are recorded.
/// Mixed blocks are excluded (they contain both adds and deletes — inline modifications).
/// Annotates matched lines with `move_target` pointing to the other file.
pub fn match_blocks(files: &mut [FileDiff]) -> Vec<BlockMatch> {
    // Index all Addition blocks by content hash
    let mut add_index: HashMap<u64, Vec<(usize, usize)>> = HashMap::new();
    for (file_idx, file) in files.iter().enumerate() {
        for (block_idx, block) in file.blocks.iter().enumerate() {
            if block.kind == BlockKind::Addition {
                add_index
                    .entry(block.content_hash)
                    .or_default()
                    .push((file_idx, block_idx));
            }
        }
    }

    // Match each Deletion block against the addition index
    let mut matches = Vec::new();
    let mut consumed: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();

    // Collect deletion block info first to avoid borrow conflicts
    let deletions: Vec<(usize, usize, u64)> = files
        .iter()
        .enumerate()
        .flat_map(|(fi, file)| {
            file.blocks
                .iter()
                .enumerate()
                .filter(|(_, b)| b.kind == BlockKind::Deletion)
                .map(move |(bi, b)| (fi, bi, b.content_hash))
        })
        .collect();

    for (del_file, del_block, hash) in deletions {
        let Some(candidates) = add_index.get(&hash) else {
            continue;
        };
        // Filter to unconsumed candidates
        let available: Vec<(usize, usize)> = candidates
            .iter()
            .copied()
            .filter(|c| !consumed.contains(c))
            .collect();

        if available.len() != 1 {
            continue; // ambiguous or no match
        }

        let (add_file, add_block) = available[0];
        consumed.insert((add_file, add_block));
        matches.push(BlockMatch {
            deletion: (del_file, del_block),
            addition: (add_file, add_block),
        });
    }

    // Annotate lines with move_target
    for m in &matches {
        let add_path = file_path_of(files, m.addition.0);
        let del_path = file_path_of(files, m.deletion.0);

        let del_range = files[m.deletion.0].blocks[m.deletion.1].range.clone();
        for line in &mut files[m.deletion.0].lines[del_range] {
            line.move_target = Some(add_path.clone());
        }

        let add_range = files[m.addition.0].blocks[m.addition.1].range.clone();
        for line in &mut files[m.addition.0].lines[add_range] {
            line.move_target = Some(del_path.clone());
        }
    }

    matches
}

fn file_path_of(files: &[FileDiff], file_idx: usize) -> String {
    files[file_idx]
        .lines
        .first()
        .and_then(|l| l.file_path.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add(n: usize) -> DiffLine {
        DiffLine::new(LineSource::Committed, format!("add {n}"), '+', Some(n))
    }

    fn del(n: usize) -> DiffLine {
        DiffLine::new(LineSource::DeletedBase, format!("del {n}"), '-', Some(n))
    }

    fn base(n: usize) -> DiffLine {
        DiffLine::new(LineSource::Base, format!("base {n}"), ' ', Some(n))
    }

    #[test]
    fn test_empty_input() {
        let mut lines: Vec<DiffLine> = vec![];
        let blocks = extract_blocks(&mut lines);
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_all_context() {
        let mut lines = vec![base(1), base(2), base(3)];
        let blocks = extract_blocks(&mut lines);
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_single_addition_block() {
        let mut lines = vec![add(1), add(2), add(3)];
        let blocks = extract_blocks(&mut lines);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, BlockKind::Addition);
        assert_eq!(blocks[0].range, 0..3);
        assert_eq!(blocks[0].change_count, 3);
    }

    #[test]
    fn test_single_deletion_block() {
        let mut lines = vec![del(1), del(2), del(3)];
        let blocks = extract_blocks(&mut lines);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, BlockKind::Deletion);
        assert_eq!(blocks[0].range, 0..3);
    }

    #[test]
    fn test_gap_merging_within_tolerance() {
        let mut lines = vec![add(1), base(2), base(3), add(4)];
        let blocks = extract_blocks(&mut lines);
        assert_eq!(blocks.len(), 1, "gap of 2 should merge into one block");
        assert_eq!(blocks[0].range, 0..4);
        assert_eq!(blocks[0].change_count, 2);
    }

    #[test]
    fn test_gap_too_large_splits_blocks() {
        let mut lines = vec![add(1), base(2), base(3), base(4), add(5)];
        let blocks = extract_blocks(&mut lines);
        assert_eq!(blocks.len(), 2, "gap of 3 should split into two blocks");
        assert_eq!(blocks[0].range, 0..1);
        assert_eq!(blocks[1].range, 4..5);
    }

    #[test]
    fn test_mixed_block() {
        let mut lines = vec![del(1), del(2), add(3), add(4)];
        let blocks = extract_blocks(&mut lines);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, BlockKind::Mixed);
    }

    #[test]
    fn test_canceled_block() {
        let mut lines = vec![
            DiffLine::new(LineSource::CanceledCommitted, "x".into(), '±', Some(1)),
            DiffLine::new(LineSource::CanceledCommitted, "y".into(), '±', Some(2)),
        ];
        let blocks = extract_blocks(&mut lines);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, BlockKind::Canceled);
    }

    #[test]
    fn test_change_source_counts_as_change() {
        let mut line = base(1);
        line.change_source = Some(LineSource::Staged);
        let mut lines = vec![line];
        let blocks = extract_blocks(&mut lines);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].change_count, 1);
    }

    #[test]
    fn test_content_hash_stability() {
        let mut lines1 = vec![add(1), add(2)];
        let mut lines2 = vec![add(1), add(2)];
        let blocks1 = extract_blocks(&mut lines1);
        let blocks2 = extract_blocks(&mut lines2);
        assert_eq!(blocks1[0].content_hash, blocks2[0].content_hash);
    }

    #[test]
    fn test_content_hash_ignores_leading_whitespace() {
        let mut lines1 = vec![
            DiffLine::new(LineSource::Committed, "  fn foo()".into(), '+', Some(1)),
        ];
        let mut lines2 = vec![
            DiffLine::new(LineSource::Committed, "    fn foo()".into(), '+', Some(1)),
        ];
        let blocks1 = extract_blocks(&mut lines1);
        let blocks2 = extract_blocks(&mut lines2);
        assert_eq!(blocks1[0].content_hash, blocks2[0].content_hash);
    }

    #[test]
    fn test_line_annotation() {
        let mut lines = vec![base(1), add(2), add(3), base(4)];
        let blocks = extract_blocks(&mut lines);
        assert_eq!(blocks.len(), 1);
        assert_eq!(lines[0].block_idx, None, "context before block");
        assert_eq!(lines[1].block_idx, Some(0));
        assert_eq!(lines[2].block_idx, Some(0));
        assert_eq!(lines[3].block_idx, None, "context after block");
    }

    #[test]
    fn test_file_header_breaks_block() {
        let mut lines = vec![add(1), DiffLine::file_header("test.rs"), add(2)];
        let blocks = extract_blocks(&mut lines);
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn test_multiple_separate_blocks() {
        let mut lines = vec![
            add(1), add(2),
            base(3), base(4), base(5), base(6),
            del(7), del(8),
        ];
        let blocks = extract_blocks(&mut lines);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].kind, BlockKind::Addition);
        assert_eq!(blocks[1].kind, BlockKind::Deletion);
    }

    #[test]
    fn test_gap_context_lines_get_block_idx() {
        let mut lines = vec![add(1), base(2), add(3)];
        let blocks = extract_blocks(&mut lines);
        assert_eq!(blocks.len(), 1);
        assert_eq!(lines[1].block_idx, Some(0), "gap context line should be in block");
    }

    // --- Block matching tests ---

    fn make_file(path: &str, lines: Vec<DiffLine>) -> FileDiff {
        let mut file_lines = vec![DiffLine::file_header(path)];
        file_lines.extend(lines);
        FileDiff::new(file_lines)
    }

    /// Create a deletion line with specific content (for matching tests)
    fn del_content(content: &str, n: usize) -> DiffLine {
        DiffLine::new(LineSource::DeletedBase, content.into(), '-', Some(n))
    }

    /// Create an addition line with specific content (for matching tests)
    fn add_content(content: &str, n: usize) -> DiffLine {
        DiffLine::new(LineSource::Committed, content.into(), '+', Some(n))
    }

    #[test]
    fn test_match_no_blocks() {
        let mut files = vec![
            make_file("a.rs", vec![base(1), base(2)]),
        ];
        let matches = match_blocks(&mut files);
        assert!(matches.is_empty());
    }

    #[test]
    fn test_match_within_file() {
        let mut files = vec![make_file("a.rs", vec![
            del_content("fn foo() {}", 1), del_content("  return 42;", 2),
            base(10), base(11), base(12), base(13),
            add_content("fn foo() {}", 20), add_content("  return 42;", 21),
        ])];
        let matches = match_blocks(&mut files);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].deletion.0, 0, "same file");
        assert_eq!(matches[0].addition.0, 0, "same file");
    }

    #[test]
    fn test_match_cross_file() {
        let mut files = vec![
            make_file("a.rs", vec![del_content("fn moved()", 1), del_content("  body", 2)]),
            make_file("b.rs", vec![add_content("fn moved()", 1), add_content("  body", 2)]),
        ];
        let matches = match_blocks(&mut files);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].deletion.0, 0, "deletion in file a");
        assert_eq!(matches[0].addition.0, 1, "addition in file b");
    }

    #[test]
    fn test_match_no_match_for_unique_blocks() {
        let mut files = vec![
            make_file("a.rs", vec![del_content("old code", 1)]),
            make_file("b.rs", vec![add_content("new code", 1)]),
        ];
        let matches = match_blocks(&mut files);
        assert!(matches.is_empty(), "different content should not match");
    }

    #[test]
    fn test_match_ambiguous_skipped() {
        let mut files = vec![
            make_file("a.rs", vec![del_content("fn dup()", 1)]),
            make_file("b.rs", vec![add_content("fn dup()", 1)]),
            make_file("c.rs", vec![add_content("fn dup()", 1)]),
        ];
        let matches = match_blocks(&mut files);
        assert!(matches.is_empty(), "ambiguous match should be skipped");
    }

    #[test]
    fn test_match_consumed() {
        let mut files = vec![
            make_file("a.rs", vec![del_content("fn shared()", 1)]),
            make_file("b.rs", vec![del_content("fn shared()", 1)]),
            make_file("c.rs", vec![add_content("fn shared()", 1)]),
        ];
        let matches = match_blocks(&mut files);
        assert_eq!(matches.len(), 1, "first deletion wins, second has no match");
    }

    #[test]
    fn test_match_mixed_blocks_excluded() {
        let mut files = vec![
            make_file("a.rs", vec![del_content("fn x()", 1), add_content("fn x()", 2)]),
            make_file("b.rs", vec![add_content("fn x()", 1)]),
        ];
        let matches = match_blocks(&mut files);
        // The mixed block in a.rs won't match; only the pure addition in b.rs exists
        // but there's no pure deletion to match it
        assert!(matches.is_empty());
    }

    #[test]
    fn test_match_annotates_lines() {
        let mut files = vec![
            make_file("a.rs", vec![del_content("fn moved()", 1), del_content("  body", 2)]),
            make_file("b.rs", vec![add_content("fn moved()", 1), add_content("  body", 2)]),
        ];
        let matches = match_blocks(&mut files);
        assert_eq!(matches.len(), 1);

        let del_range = files[0].blocks[matches[0].deletion.1].range.clone();
        for line in &files[0].lines[del_range] {
            assert_eq!(line.move_target.as_deref(), Some("b.rs"));
        }

        let add_range = files[1].blocks[matches[0].addition.1].range.clone();
        for line in &files[1].lines[add_range] {
            assert_eq!(line.move_target.as_deref(), Some("a.rs"));
        }
    }

    #[test]
    fn test_match_indentation_tolerant() {
        let mut files = vec![
            make_file("a.rs", vec![
                DiffLine::new(LineSource::DeletedBase, "  fn foo()".into(), '-', Some(1)),
            ]),
            make_file("b.rs", vec![
                DiffLine::new(LineSource::Committed, "    fn foo()".into(), '+', Some(1)),
            ]),
        ];
        let matches = match_blocks(&mut files);
        assert_eq!(matches.len(), 1, "indentation difference should still match");
    }

    #[test]
    fn test_moved_blocks_show_same_lines_on_both_sides() {
        // Simulates: function deleted from a.rs (with trailing blank),
        // added to b.rs (with leading blank). Both sides should mark
        // exactly the same content lines as moved — blank lines at
        // block edges that don't appear on the other side should NOT
        // be marked as moved.
        let mut files = vec![
            make_file("a.rs", vec![
                del_content("fn process_data() {", 1),
                del_content("    let x = 1;", 2),
                del_content("}", 3),
                // trailing blank line — only on deletion side
                DiffLine::new(LineSource::DeletedBase, "".into(), '-', None),
            ]),
            make_file("b.rs", vec![
                // leading blank line — only on addition side
                DiffLine::new(LineSource::Committed, "".into(), '+', None),
                add_content("fn process_data() {", 5),
                add_content("    let x = 1;", 6),
                add_content("}", 7),
            ]),
        ];
        let matches = match_blocks(&mut files);
        assert_eq!(matches.len(), 1, "should detect the move");

        // Count moved lines on each side
        let del_moved: Vec<&str> = files[0].lines.iter()
            .filter(|l| l.move_target.is_some())
            .map(|l| l.content.as_str())
            .collect();
        let add_moved: Vec<&str> = files[1].lines.iter()
            .filter(|l| l.move_target.is_some())
            .map(|l| l.content.as_str())
            .collect();

        assert_eq!(del_moved.len(), add_moved.len(),
            "both sides should have the same number of moved lines\n  del: {del_moved:?}\n  add: {add_moved:?}");

        // Only the 3 content lines should be moved, not the blank lines
        assert_eq!(del_moved.len(), 3,
            "should mark 3 content lines, not blank edges: {del_moved:?}");
        assert!(!del_moved.contains(&""),
            "blank lines should not be marked as moved: {del_moved:?}");
        assert!(!add_moved.contains(&""),
            "blank lines should not be marked as moved: {add_moved:?}");
    }
}
