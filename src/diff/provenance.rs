use imara_diff::{diff, intern::InternedInput, Algorithm};
use std::collections::HashMap;
use std::ops::Range;

use super::inline::compute_inline_diff_merged;
use super::LineSource;

/// Build a provenance map from old_lines to new_lines using histogram diff algorithm.
/// Returns a Vec where result[new_idx] = Some(old_idx) if new_lines[new_idx] came from old_lines[old_idx]
/// or None if it was inserted (not present in old_lines).
///
/// The histogram algorithm anchors on low-occurrence lines, producing better structural
/// alignment for files with repetitive patterns (HTML, XML, etc.).
pub fn build_provenance_map(old_lines: &[&str], new_lines: &[&str]) -> Vec<Option<usize>> {
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

    // Lines after all hunks are equal
    while new_idx < new_lines.len() {
        result[new_idx] = Some(old_idx);
        old_idx += 1;
        new_idx += 1;
    }

    result
}

/// Build a modification map from change hunks using histogram diff algorithm.
/// Returns: HashMap<new_idx, (old_idx, old_content)>
///
/// For each hunk (change region), matches deleted lines with inserted lines
/// based on content similarity for inline diff highlighting.
pub fn build_modification_map<'a>(
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
