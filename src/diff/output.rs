use std::collections::HashMap;

use super::inline::compute_inline_diff_merged;
use super::{DiffLine, LineSource};

/// Determine where a base line was deleted (in commit, staging, or working)
pub fn determine_deletion_source(
    base_idx: usize,
    _base_lines: &[&str],
    _head_lines: &[&str],
    _index_lines: &[&str],
    head_from_base: &[Option<usize>],
    index_from_head: &[Option<usize>],
) -> LineSource {
    // Check if base line still exists in head (by provenance, not content)
    // A base line exists in head if some head line traces back to this base line
    let in_head = head_from_base.iter().any(|&opt| opt == Some(base_idx));

    if !in_head {
        return LineSource::DeletedBase;  // Deleted in commit
    }

    // Find which head line came from this base line
    let head_idx = head_from_base.iter().position(|&opt| opt == Some(base_idx));
    if let Some(head_idx) = head_idx {
        // Check if this head line still exists in index
        let in_index = index_from_head.iter().any(|&opt| opt == Some(head_idx));

        if !in_index {
            return LineSource::DeletedCommitted;  // Deleted in staging
        }
    }

    LineSource::DeletedStaged  // Deleted in working tree
}

/// Build the output line for a working line, handling modifications
pub fn build_working_line_output<F1, F2>(
    working_idx: usize,
    working_content: &str,
    source: LineSource,
    line_num: usize,
    path: &str,
    working_from_index: &[Option<usize>],
    index_from_head: &[Option<usize>],
    _head_from_base: &[Option<usize>],
    index_working_mods: &HashMap<usize, (usize, &str)>,
    base_head_mods: &HashMap<usize, (usize, &str)>,
    head_index_mods: &HashMap<usize, (usize, &str)>,
    _index_lines: &[&str],
    _head_lines: &[&str],
    trace_index_source: &F1,
    trace_head_source: &F2,
) -> DiffLine
where
    F1: Fn(usize) -> LineSource,
    F2: Fn(usize) -> LineSource,
{
    let content = working_content.to_string();

    // Default output: simple line with source
    let default_line = || {
        let prefix = if source == LineSource::Base { ' ' } else { '+' };
        DiffLine::new(source, content.clone(), prefix, Some(line_num)).with_file_path(path)
    };

    match source {
        LineSource::Unstaged => {
            // Check if this is a modification of an index line
            if let Some((index_idx, old_content)) = index_working_mods.get(&working_idx) {
                let original_source = trace_index_source(*index_idx);
                let inline_result = compute_inline_diff_merged(old_content, &content, LineSource::Unstaged);

                if inline_result.is_meaningful {
                    // Use spans directly - they already have correct source and is_deletion
                    return DiffLine::new(original_source, content, ' ', Some(line_num))
                        .with_file_path(path)
                        .with_inline_spans(inline_result.spans);
                }
            }
            default_line()
        }

        LineSource::Committed => {
            // Check if this is a modification of a base line
            if let Some(index_idx) = working_from_index.get(working_idx).copied().flatten() {
                if let Some(head_idx) = index_from_head.get(index_idx).copied().flatten() {
                    if let Some((_base_idx, old_content)) = base_head_mods.get(&head_idx) {
                        let inline_result = compute_inline_diff_merged(old_content, &content, LineSource::Committed);

                        if inline_result.is_meaningful {
                            // Use spans directly - they already have correct source and is_deletion
                            return DiffLine::new(LineSource::Base, content, ' ', Some(line_num))
                                .with_file_path(path)
                                .with_inline_spans(inline_result.spans);
                        }
                    }
                }
            }
            default_line()
        }

        LineSource::Staged => {
            // Check if this is a modification of a head line
            if let Some(index_idx) = working_from_index.get(working_idx).copied().flatten() {
                if let Some((_head_idx, old_content)) = head_index_mods.get(&index_idx) {
                    let inline_result = compute_inline_diff_merged(old_content, &content, LineSource::Staged);

                    if inline_result.is_meaningful {
                        let original_source = if let Some(head_idx) = index_from_head.get(index_idx).copied().flatten() {
                            trace_head_source(head_idx)
                        } else {
                            LineSource::Base
                        };

                        // Use spans directly - they already have correct source and is_deletion
                        return DiffLine::new(original_source, content, ' ', Some(line_num))
                            .with_file_path(path)
                            .with_inline_spans(inline_result.spans);
                    }
                }
            }
            default_line()
        }

        LineSource::Base => default_line(),

        _ => default_line(),
    }
}
