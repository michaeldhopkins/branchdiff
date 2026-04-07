use imara_diff::{diff, intern::InternedInput, Algorithm};
use std::collections::HashMap;
use std::ops::Range;

use super::inline::compute_inline_diff_merged;
use super::LineSource;

/// Score a hunk boundary position. Higher is better.
/// Prefers boundaries that land after a blank line (between logical blocks).
fn boundary_score(line: &str) -> u32 {
    if line.trim().is_empty() {
        3 // blank line — ideal boundary (between functions/blocks)
    } else if !line.starts_with(' ') && !line.starts_with('\t') {
        2 // unindented line — likely a top-level declaration
    } else {
        0
    }
}

/// Slide hunk boundaries to prefer landing on blank lines.
///
/// When a hunk can be shifted (the line leaving the top equals the line
/// entering the bottom), try all equivalent positions and pick the one
/// where the context line just before the hunk is a blank line.
/// This produces cleaner diffs for function-oriented code.
fn slide_hunks(
    hunks: &mut [(Range<u32>, Range<u32>)],
    old_lines: &[&str],
    new_lines: &[&str],
) {
    let old_len = old_lines.len() as u32;
    let new_len = new_lines.len() as u32;

    for hunk in hunks.iter_mut() {
        let (ref mut before, ref mut after) = *hunk;

        // Only slide pure deletions or pure insertions, not mixed hunks.
        // For mixed hunks, sliding is constrained by both sides needing
        // identical boundary lines, which is rarely productive.
        let before_len = before.end - before.start;
        let after_len = after.end - after.start;
        if before_len > 0 && after_len > 0 {
            continue;
        }

        let mut best_score = 0u32;
        let mut best_offset: i32 = 0;

        // Try sliding down: line at before.start must equal line at before.end
        let mut offset: i32 = 0;
        loop {
            let new_before_end = before.end as i32 + offset;
            let new_after_end = after.end as i32 + offset;
            let new_before_start = before.start as i32 + offset;

            if new_before_end >= old_len as i32 || new_after_end >= new_len as i32 {
                break;
            }
            if new_before_start < 0 {
                break;
            }

            // Check the line just before the hunk start.
            // For deletions, score using old_lines; for insertions, use new_lines.
            let ctx_score = if before_len > 0 {
                let ctx_idx = new_before_start - 1;
                if ctx_idx >= 0 { boundary_score(old_lines[ctx_idx as usize]) } else { 0 }
            } else {
                let new_after_start = after.start as i32 + offset;
                let ctx_idx = new_after_start - 1;
                if ctx_idx >= 0 { boundary_score(new_lines[ctx_idx as usize]) } else { 0 }
            };
            if ctx_score > best_score {
                best_score = ctx_score;
                best_offset = offset;
            }

            // Can we slide further? The line exiting the top must equal the line entering the bottom.
            if before_len > 0 {
                // Pure deletion: check old lines
                if old_lines[new_before_start as usize] == old_lines[new_before_end as usize] {
                    offset += 1;
                } else {
                    break;
                }
            } else {
                // Pure insertion: check new lines
                let new_after_start = after.start as i32 + offset;
                if new_after_start >= 0
                    && (new_after_end as usize) < new_lines.len()
                    && new_lines[new_after_start as usize] == new_lines[new_after_end as usize]
                {
                    offset += 1;
                } else {
                    break;
                }
            }
        }

        // Check final position after loop (the last valid slide)
        let final_score = if before_len > 0 {
            let ctx = before.start as i32 + offset - 1;
            if ctx >= 0 { boundary_score(old_lines[ctx as usize]) } else { 0 }
        } else {
            let ctx = after.start as i32 + offset - 1;
            if ctx >= 0 { boundary_score(new_lines[ctx as usize]) } else { 0 }
        };
        if final_score > best_score {
            best_score = final_score;
            best_offset = offset;
        }

        // Apply the best offset
        if best_offset != 0 && best_score > 0 {
            before.start = (before.start as i32 + best_offset) as u32;
            before.end = (before.end as i32 + best_offset) as u32;
            after.start = (after.start as i32 + best_offset) as u32;
            after.end = (after.end as i32 + best_offset) as u32;
        }
    }
}

/// Build a provenance map from old_lines to new_lines using histogram diff algorithm.
/// Returns a Vec where result[new_idx] = Some(old_idx) if new_lines[new_idx] came from old_lines[old_idx]
/// or None if it was inserted (not present in old_lines).
///
/// The histogram algorithm anchors on low-occurrence lines, producing better structural
/// alignment for files with repetitive patterns (HTML, XML, etc.).
pub(super) fn build_provenance_map(old_lines: &[&str], new_lines: &[&str]) -> Vec<Option<usize>> {
    if old_lines.is_empty() {
        return vec![None; new_lines.len()];
    }
    if new_lines.is_empty() {
        return Vec::new();
    }

    // Join lines for imara-diff (it tokenizes by newlines internally)
    let old_text = old_lines.join("\n");
    let new_text = new_lines.join("\n");

    let input = InternedInput::new(old_text.as_str(), new_text.as_str());

    // Collect hunks first
    let mut hunks: Vec<(Range<u32>, Range<u32>)> = Vec::new();
    diff(
        Algorithm::Histogram,
        &input,
        |before: Range<u32>, after: Range<u32>| {
            hunks.push((before, after));
        },
    );

    slide_hunks(&mut hunks, old_lines, new_lines);

    let mut result = vec![None; new_lines.len()];
    let mut old_idx = 0usize;
    let mut new_idx = 0usize;

    for (before, after) in hunks {
        let hunk_new_start = after.start as usize;

        // Lines before this hunk are equal
        while new_idx < hunk_new_start {
            result[new_idx] = Some(old_idx);
            old_idx += 1;
            new_idx += 1;
        }

        // Skip the changed region
        old_idx = before.end as usize;
        new_idx = after.end as usize;
    }

    // Lines after all hunks are equal - but only if both sides have remaining lines
    while new_idx < new_lines.len() && old_idx < old_lines.len() {
        result[new_idx] = Some(old_idx);
        old_idx += 1;
        new_idx += 1;
    }
    // Any remaining new_lines beyond old_lines.len() are insertions (already None)

    result
}

/// Build a modification map from change hunks using histogram diff algorithm.
/// Returns: HashMap<new_idx, (old_idx, old_content)>
///
/// For each hunk (change region), matches deleted lines with inserted lines
/// based on content similarity for inline diff highlighting.
pub(super) fn build_modification_map<'a>(
    old_lines: &[&'a str],
    new_lines: &[&'a str],
    _change_source: LineSource,
) -> HashMap<usize, (usize, &'a str)> {
    let mut result = HashMap::new();

    if old_lines.is_empty() || new_lines.is_empty() {
        return result;
    }

    // Join lines for imara-diff (it tokenizes by newlines internally)
    let old_text = old_lines.join("\n");
    let new_text = new_lines.join("\n");

    let input = InternedInput::new(old_text.as_str(), new_text.as_str());

    // Collect hunks first
    let mut hunks: Vec<(Range<u32>, Range<u32>)> = Vec::new();
    diff(
        Algorithm::Histogram,
        &input,
        |before: Range<u32>, after: Range<u32>| {
            hunks.push((before, after));
        },
    );

    slide_hunks(&mut hunks, old_lines, new_lines);

    for (before, after) in hunks {
        let old_start = before.start as usize;
        let old_end = before.end as usize;
        let new_start = after.start as usize;
        let new_end = after.end as usize;

        // Collect deletions (lines from old that are being removed)
        let deletions: Vec<(&str, usize)> = (old_start..old_end)
            .map(|i| (old_lines[i].trim_end(), i))
            .collect();

        // Collect insertions (lines in new that are being added), skip empty lines
        let insertions: Vec<(&str, usize)> = (new_start..new_end)
            .map(|i| (new_lines[i].trim_end(), i))
            .filter(|(content, _)| !content.trim().is_empty())
            .collect();

        // Match deletions with insertions based on content similarity
        let mut paired_inserts: std::collections::HashSet<usize> = std::collections::HashSet::new();

        for (old_content, old_i) in &deletions {
            for (ins_idx, (new_content, new_i)) in insertions.iter().enumerate() {
                if paired_inserts.contains(&ins_idx) {
                    continue;
                }

                let inline_result =
                    compute_inline_diff_merged(old_content, new_content, LineSource::Unstaged);
                if inline_result.is_meaningful {
                    paired_inserts.insert(ins_idx);
                    result.insert(*new_i, (*old_i, old_lines[*old_i]));
                    break;
                }
            }
        }
    }

    result
}

/// Check if `target_idx` appears anywhere in `provenance` (i.e., survives to the next stage)
pub(super) fn survives_in(provenance: &[Option<usize>], target_idx: usize) -> bool {
    provenance.contains(&Some(target_idx))
}

/// Find all indices in `provenance` that point to `target_idx`
pub(super) fn find_sources(
    provenance: &[Option<usize>],
    target_idx: usize,
) -> impl Iterator<Item = usize> + '_ {
    provenance
        .iter()
        .enumerate()
        .filter_map(move |(idx, &prov)| {
            if prov == Some(target_idx) {
                Some(idx)
            } else {
                None
            }
        })
}

/// Check if `source_idx` survives through an intermediate stage to the final stage (chained lookup)
pub(super) fn survives_chain(
    source_idx: usize,
    intermediate_from_source: &[Option<usize>],
    final_from_intermediate: &[Option<usize>],
) -> bool {
    find_sources(intermediate_from_source, source_idx)
        .any(|intermediate_idx| survives_in(final_from_intermediate, intermediate_idx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provenance_map_new_has_more_lines_than_old() {
        // Regression test: ensure we don't produce out-of-bounds indices
        // when new_lines has more lines than old_lines after all hunks
        let old_lines = &["line1", "line2"];
        let new_lines = &["line1", "line2", "line3", "line4", "line5"];

        let provenance = build_provenance_map(old_lines, new_lines);

        // Should have 5 entries (one per new line)
        assert_eq!(provenance.len(), 5);

        // First two lines came from old
        assert_eq!(provenance[0], Some(0));
        assert_eq!(provenance[1], Some(1));

        // Lines 3-5 are insertions (not present in old)
        assert_eq!(provenance[2], None);
        assert_eq!(provenance[3], None);
        assert_eq!(provenance[4], None);

        // All indices in the provenance should be valid for old_lines
        for prov in &provenance {
            if let Some(idx) = prov {
                assert!(
                    *idx < old_lines.len(),
                    "provenance index {} is out of bounds for old_lines (len {})",
                    idx,
                    old_lines.len()
                );
            }
        }
    }

    #[test]
    fn test_provenance_map_old_has_more_lines_than_new() {
        // The inverse case: old has more lines than new
        let old_lines = &["line1", "line2", "line3", "line4", "line5"];
        let new_lines = &["line1", "line2"];

        let provenance = build_provenance_map(old_lines, new_lines);

        // Should have 2 entries (one per new line)
        assert_eq!(provenance.len(), 2);

        // Both lines came from old
        assert_eq!(provenance[0], Some(0));
        assert_eq!(provenance[1], Some(1));

        // All indices should be valid
        for prov in &provenance {
            if let Some(idx) = prov {
                assert!(*idx < old_lines.len());
            }
        }
    }

    #[test]
    fn test_survives_in_found() {
        let provenance = vec![Some(0), Some(1), None, Some(2)];
        assert!(survives_in(&provenance, 0));
        assert!(survives_in(&provenance, 1));
        assert!(survives_in(&provenance, 2));
    }

    #[test]
    fn test_survives_in_not_found() {
        let provenance = vec![Some(0), Some(1), None, Some(2)];
        assert!(!survives_in(&provenance, 3));
        assert!(!survives_in(&provenance, 99));
    }

    #[test]
    fn test_survives_in_empty() {
        let provenance: Vec<Option<usize>> = vec![];
        assert!(!survives_in(&provenance, 0));
    }

    #[test]
    fn test_find_sources_single_match() {
        let provenance = vec![Some(0), Some(1), Some(2)];
        let sources: Vec<_> = find_sources(&provenance, 1).collect();
        assert_eq!(sources, vec![1]);
    }

    #[test]
    fn test_find_sources_multiple_matches() {
        // Multiple indices can point to the same source (e.g., duplicated lines)
        let provenance = vec![Some(0), Some(1), Some(0), Some(1)];
        let sources: Vec<_> = find_sources(&provenance, 0).collect();
        assert_eq!(sources, vec![0, 2]);
    }

    #[test]
    fn test_find_sources_no_match() {
        let provenance = vec![Some(0), Some(1), Some(2)];
        let sources: Vec<_> = find_sources(&provenance, 99).collect();
        assert!(sources.is_empty());
    }

    #[test]
    fn test_survives_chain_direct_path() {
        // source 0 -> intermediate 1 -> final 2
        let intermediate_from_source = vec![None, Some(0), None];
        let final_from_intermediate = vec![None, None, Some(1)];
        assert!(survives_chain(0, &intermediate_from_source, &final_from_intermediate));
    }

    #[test]
    fn test_survives_chain_no_path() {
        // source 0 -> intermediate 1, but intermediate 1 doesn't survive to final
        let intermediate_from_source = vec![None, Some(0), None];
        let final_from_intermediate = vec![Some(99), None, None];
        assert!(!survives_chain(0, &intermediate_from_source, &final_from_intermediate));
    }

    #[test]
    fn test_survives_chain_source_not_in_intermediate() {
        let intermediate_from_source = vec![Some(1), Some(2)];
        let final_from_intermediate = vec![Some(0), Some(1)];
        assert!(!survives_chain(99, &intermediate_from_source, &final_from_intermediate));
    }

    #[test]
    fn test_hunk_boundary_multi_deletion_provenance() {
        // Delete fn two and fn four from 5 functions.
        // fn three's "}" must map to old fn three's "}", NOT old fn four's "}".
        let old: Vec<&str> = "fn one() {\n    println!(\"one\");\n}\n\n\
fn two() {\n    println!(\"two\");\n}\n\n\
fn three() {\n    println!(\"three\");\n}\n\n\
fn four() {\n    println!(\"four\");\n    println!(\"more\");\n}\n\n\
fn five() {\n    println!(\"five\");\n}".lines().collect();

        let new: Vec<&str> = "fn one() {\n    println!(\"one\");\n}\n\n\
fn three() {\n    println!(\"three\");\n}\n\n\
fn five() {\n    println!(\"five\");\n}".lines().collect();

        let prov = build_provenance_map(&old, &new);

        // new[6] is fn three's "}" — should map to old[10] (fn three's "}"),
        // NOT old[15] (fn four's "}")
        assert_eq!(prov[6], Some(10),
            "fn three's '}}' (new[6]) should map to old[10], not old[{}]",
            prov[6].unwrap_or(999));
    }

    #[test]
    fn test_hunk_boundary_prefers_blank_line() {
        // Provenance-level test: confirms histogram diff handles function
        // deletion boundaries correctly at the line-matching level.
        let old: &[&str] = &[
            "fn three() {",             // 0
            "    println!(\"three\");",  // 1
            "}",                         // 2
            "",                          // 3
            "fn four() {",               // 4
            "    println!(\"four\");",   // 5
            "}",                         // 6
            "",                          // 7
            "fn five() {",               // 8
            "    println!(\"five\");",   // 9
            "}",                         // 10
        ];
        let new: &[&str] = &[
            "fn three() {",             // 0
            "    println!(\"three\");",  // 1
            "}",                         // 2
            "",                          // 3
            "fn five() {",               // 4
            "    println!(\"five\");",   // 5
            "}",                         // 6
        ];

        let prov = build_provenance_map(old, new);

        assert_eq!(prov[0], Some(0), "fn three");
        assert_eq!(prov[1], Some(1));
        assert_eq!(prov[2], Some(2), "three's }}");
        assert_eq!(prov[3], Some(3), "blank line between three and five");
        assert_eq!(prov[4], Some(8), "fn five should map to old[8]");
        assert_eq!(prov[5], Some(9));
        assert_eq!(prov[6], Some(10));
    }
}
