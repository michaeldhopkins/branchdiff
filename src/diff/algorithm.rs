//! 4-way diff algorithm: base→head→index→working.
//!
//! Computes a unified diff showing changes across all four file versions,
//! using provenance maps to track where each line originated.

use super::cancellation::{
    collect_canceled_committed, collect_canceled_simple, collect_canceled_staged,
    insert_canceled_lines,
};
use super::output::{build_working_line_output, determine_deletion_source};
use super::provenance::{build_modification_map, build_provenance_map};
use super::{DiffLine, FileDiff, LineSource};

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
pub fn compute_four_way_diff(
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
