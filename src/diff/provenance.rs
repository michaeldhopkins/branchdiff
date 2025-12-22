use similar::{ChangeTag, TextDiff};
use std::collections::HashMap;

use super::inline::compute_inline_diff_merged;
use super::LineSource;

/// Build a provenance map from old_lines to new_lines
/// Returns a Vec where result[new_idx] = Some(old_idx) if new_lines[new_idx] came from old_lines[old_idx]
/// or None if it was inserted (not present in old_lines)
pub fn build_provenance_map(old_lines: &[&str], new_lines: &[&str]) -> Vec<Option<usize>> {
    let diff = TextDiff::from_slices(old_lines, new_lines);
    let mut result = vec![None; new_lines.len()];

    let mut old_idx = 0usize;
    let mut new_idx = 0usize;

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                result[new_idx] = Some(old_idx);
                old_idx += 1;
                new_idx += 1;
            }
            ChangeTag::Delete => {
                old_idx += 1;
            }
            ChangeTag::Insert => {
                new_idx += 1;
            }
        }
    }

    result
}

/// Represents a single change in a diff, with type-safe access to indices.
/// Each variant contains exactly the fields that are valid for that change type.
enum DiffChange<'a> {
    Equal,
    Delete { content: &'a str, old_idx: usize },
    Insert { content: &'a str, new_idx: usize },
}

/// Build a modification map from adjacent delete-insert pairs
/// Returns: HashMap<new_idx, (old_idx, old_content)>
pub fn build_modification_map<'a>(
    old_lines: &[&'a str],
    new_lines: &[&'a str],
    _change_source: LineSource,
) -> HashMap<usize, (usize, &'a str)> {
    let mut result = HashMap::new();
    let diff = TextDiff::from_slices(old_lines, new_lines);

    // Collect all changes with indices
    let mut old_idx = 0usize;
    let mut new_idx = 0usize;
    let changes: Vec<DiffChange> = diff.iter_all_changes()
        .map(|c| {
            let change = match c.tag() {
                ChangeTag::Equal => {
                    old_idx += 1;
                    new_idx += 1;
                    DiffChange::Equal
                }
                ChangeTag::Delete => {
                    let ch = DiffChange::Delete {
                        content: c.value().trim_end(),
                        old_idx,
                    };
                    old_idx += 1;
                    ch
                }
                ChangeTag::Insert => {
                    let ch = DiffChange::Insert {
                        content: c.value().trim_end(),
                        new_idx,
                    };
                    new_idx += 1;
                    ch
                }
            };
            change
        })
        .collect();

    // Find blocks of deletions followed by insertions, and pair them intelligently
    // We need to match deletions with insertions based on content similarity,
    // not just positional adjacency in the change stream.
    let mut i = 0;
    while i < changes.len() {
        if let DiffChange::Delete { .. } = changes[i] {
            // Collect consecutive deletions
            let mut deletions: Vec<(&str, usize)> = Vec::new();
            let mut j = i;
            while j < changes.len() {
                if let DiffChange::Delete { content, old_idx } = &changes[j] {
                    deletions.push((content, *old_idx));
                    j += 1;
                } else {
                    break;
                }
            }

            // Skip empty inserts
            while j < changes.len() {
                if let DiffChange::Insert { content, .. } = &changes[j] {
                    if content.trim().is_empty() {
                        j += 1;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }

            // Collect consecutive insertions
            let mut insertions: Vec<(&str, usize)> = Vec::new();
            while j < changes.len() {
                if let DiffChange::Insert { content, new_idx } = &changes[j] {
                    insertions.push((content, *new_idx));
                    j += 1;
                } else {
                    break;
                }
            }

            // Match deletions with insertions based on content similarity
            // Track which insertions have been paired
            let mut paired_inserts: std::collections::HashSet<usize> = std::collections::HashSet::new();

            for (old_content, old_i) in &deletions {
                // Find the best matching insert (highest similarity that qualifies as meaningful)
                let mut best_match: Option<(usize, usize)> = None; // (insert_idx_in_vec, new_i)

                for (ins_idx, (new_content, new_i)) in insertions.iter().enumerate() {
                    if paired_inserts.contains(&ins_idx) {
                        continue;
                    }

                    let inline_result = compute_inline_diff_merged(old_content, new_content, LineSource::Unstaged);
                    if inline_result.is_meaningful {
                        // Use this match - first meaningful match wins for simplicity
                        best_match = Some((ins_idx, *new_i));
                        break;
                    }
                }

                if let Some((ins_idx, new_i)) = best_match {
                    paired_inserts.insert(ins_idx);
                    result.insert(new_i, (*old_i, old_lines[*old_i]));
                }
            }

            i = j;
        } else {
            i += 1;
        }
    }

    result
}
