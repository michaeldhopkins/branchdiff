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
                // result[new_idx] stays None - this line was inserted
                new_idx += 1;
            }
        }
    }

    result
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
    struct Change<'b> {
        tag: ChangeTag,
        content: &'b str,
        old_idx: Option<usize>,
        new_idx: Option<usize>,
    }

    let mut old_idx = 0usize;
    let mut new_idx = 0usize;
    let changes: Vec<Change> = diff.iter_all_changes()
        .map(|c| {
            let change = Change {
                tag: c.tag(),
                content: c.value().trim_end(),
                old_idx: if c.tag() == ChangeTag::Delete { Some(old_idx) } else { None },
                new_idx: if c.tag() == ChangeTag::Insert { Some(new_idx) } else { None },
            };
            match c.tag() {
                ChangeTag::Equal => { old_idx += 1; new_idx += 1; }
                ChangeTag::Delete => { old_idx += 1; }
                ChangeTag::Insert => { new_idx += 1; }
            }
            change
        })
        .collect();

    // Find blocks of deletions followed by insertions, and pair them intelligently
    // We need to match deletions with insertions based on content similarity,
    // not just positional adjacency in the change stream.
    let mut i = 0;
    while i < changes.len() {
        if changes[i].tag == ChangeTag::Delete {
            // Collect consecutive deletions
            let mut deletions: Vec<&Change> = vec![&changes[i]];
            let mut j = i + 1;
            while j < changes.len() && changes[j].tag == ChangeTag::Delete {
                deletions.push(&changes[j]);
                j += 1;
            }

            // Skip empty inserts
            while j < changes.len() && changes[j].tag == ChangeTag::Insert && changes[j].content.trim().is_empty() {
                j += 1;
            }

            // Collect consecutive insertions
            let mut insertions: Vec<&Change> = Vec::new();
            while j < changes.len() && changes[j].tag == ChangeTag::Insert {
                insertions.push(&changes[j]);
                j += 1;
            }

            // Match deletions with insertions based on content similarity
            // Track which insertions have been paired
            let mut paired_inserts: std::collections::HashSet<usize> = std::collections::HashSet::new();

            for deletion in &deletions {
                let old_i = deletion.old_idx.unwrap();
                let old_content = deletion.content;

                // Find the best matching insert (highest similarity that qualifies as meaningful)
                let mut best_match: Option<(usize, usize)> = None; // (insert_idx_in_vec, new_i)

                for (ins_idx, insertion) in insertions.iter().enumerate() {
                    if paired_inserts.contains(&ins_idx) {
                        continue;
                    }
                    let new_i = insertion.new_idx.unwrap();
                    let new_content = insertion.content;

                    let inline_result = compute_inline_diff_merged(old_content, new_content, LineSource::Unstaged);
                    if inline_result.is_meaningful {
                        // Use this match - first meaningful match wins for simplicity
                        best_match = Some((ins_idx, new_i));
                        break;
                    }
                }

                if let Some((ins_idx, new_i)) = best_match {
                    paired_inserts.insert(ins_idx);
                    result.insert(new_i, (old_i, old_lines[old_i]));
                }
            }

            i = j;
        } else {
            i += 1;
        }
    }

    result
}
