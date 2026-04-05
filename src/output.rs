use crate::app::App;
use crate::diff::{DiffLine, LineSource};

/// A file prepared for non-interactive output, with view mode filtering
/// and inline spans already computed.
pub struct OutputFile {
    pub path: String,
    pub lines: Vec<DiffLine>,
    pub additions: usize,
    pub deletions: usize,
    /// Whether this file should be collapsed by default (e.g. lock files)
    pub collapsed: bool,
}

/// Everything a non-interactive renderer needs, independent of output format.
pub struct OutputData {
    pub repo_name: String,
    pub to_label: String,
    pub from_label: String,
    pub files: Vec<OutputFile>,
    pub total_additions: usize,
    pub total_deletions: usize,
}

const CONTEXT_LINES: usize = 5;

/// Compute context visibility for a slice of lines.
/// Returns a bool-per-line indicating whether it should be shown.
fn context_visibility(lines: &[DiffLine]) -> Vec<bool> {
    let interesting: Vec<bool> = lines
        .iter()
        .map(|line| {
            line.source.is_header()
                || line.old_content.is_some()
                || !line.inline_spans.is_empty()
                || line.source.is_change()
        })
        .collect();

    let mut show = vec![false; lines.len()];
    for (i, &is_int) in interesting.iter().enumerate() {
        if is_int {
            let start = i.saturating_sub(CONTEXT_LINES);
            let end = (i + CONTEXT_LINES + 1).min(lines.len());
            for item in show.iter_mut().take(end).skip(start) {
                *item = true;
            }
        }
    }
    show
}

/// Filter lines by visibility, inserting elided markers for gaps.
fn filter_with_elided(lines: &[DiffLine], show: &[bool]) -> Vec<DiffLine> {
    let mut result = Vec::new();
    let mut last_shown: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        if show[i] {
            let gap = match last_shown {
                Some(last) => i - last - 1,
                None => i, // leading hidden lines
            };
            if gap > 0 {
                result.push(DiffLine::elided(gap));
            }
            result.push(line.clone());
            last_shown = Some(i);
        }
    }

    if let Some(last) = last_shown {
        let trailing_hidden = (last + 1..lines.len())
            .filter(|&i| !show[i])
            .count();
        if trailing_hidden > 0 {
            result.push(DiffLine::elided(trailing_hidden));
        }
    }

    result
}

/// Prepare app data for non-interactive output.
///
/// This is the shared engine for print, HTML, and any future output formats.
/// It computes inline spans, applies the current view mode, and returns
/// file-grouped lines ready for rendering.
pub fn prepare(app: &mut App) -> OutputData {
    // Compute inline spans on the canonical file-level lines
    for file in &mut app.files {
        for line in &mut file.lines {
            if line.old_content.is_some() {
                line.ensure_inline_spans();
            }
        }
    }

    let files: Vec<OutputFile> = app
        .files
        .iter()
        .filter_map(|file| {
            let path = file
                .lines
                .first()
                .filter(|l| l.source == LineSource::FileHeader)
                .map(|l| l.content.clone())
                .unwrap_or_default();

            let has_content = file.lines.iter().any(|l| l.source != LineSource::FileHeader);
            if !has_content {
                return None;
            }

            let filtered = apply_view_mode(&file.lines, &app.view.view_mode);

            let additions = filtered.iter().filter(|l| l.source.is_addition()).count();
            let deletions = filtered.iter().filter(|l| l.source.is_deletion()).count();

            let collapsed = app.view.collapsed_files.contains(&path);

            Some(OutputFile {
                path,
                lines: filtered,
                additions,
                deletions,
                collapsed,
            })
        })
        .collect();

    let total_additions = files.iter().map(|f| f.additions).sum();
    let total_deletions = files.iter().map(|f| f.deletions).sum();

    let repo_name = app
        .repo_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo")
        .to_string();

    OutputData {
        repo_name,
        to_label: app.comparison.to_label.clone(),
        from_label: app.comparison.from_label.clone(),
        files,
        total_additions,
        total_deletions,
    }
}

fn apply_view_mode(lines: &[DiffLine], view_mode: &crate::app::ViewMode) -> Vec<DiffLine> {
    use crate::app::ViewMode;
    match view_mode {
        ViewMode::Full => lines.to_vec(),
        ViewMode::Context => {
            let show = context_visibility(lines);
            filter_with_elided(lines, &show)
        }
        ViewMode::ChangesOnly => lines
            .iter()
            .filter(|l| l.source.is_change() || l.source.is_header())
            .cloned()
            .collect(),
        // CommitOnly and BookmarkOnly fall back to Context for non-interactive output
        ViewMode::CommitOnly | ViewMode::BookmarkOnly => {
            let show = context_visibility(lines);
            filter_with_elided(lines, &show)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::DiffLine;

    #[test]
    fn test_context_visibility_shows_changes_and_context() {
        // No header — pure content lines to test context window in isolation
        let mut lines = Vec::new();
        // 10 base lines (indices 0-9), then a change (index 10), then 10 base lines (11-20)
        for i in 0..10 {
            lines.push(DiffLine::new(LineSource::Base, format!("line {i}"), ' ', Some(i + 1)));
        }
        lines.push(DiffLine::new(LineSource::Committed, "added".into(), '+', Some(11)));
        for i in 11..21 {
            lines.push(DiffLine::new(LineSource::Base, format!("line {i}"), ' ', Some(i + 1)));
        }

        let show = context_visibility(&lines);

        // Lines 0-4 should be hidden (indices 0-4, more than 5 away from change at 10)
        assert!(!show[0], "line 0 should be hidden");
        assert!(!show[4], "line 4 should be hidden");
        // Lines 5-9 should be visible (within 5 of change at index 10)
        assert!(show[5], "line 5 should be visible (5 before change)");
        assert!(show[9], "line 9 should be visible (1 before change)");
        // The change itself
        assert!(show[10], "change should be visible");
        // Lines 11-15 should be visible (within 5 after change)
        assert!(show[11], "line 11 should be visible");
        assert!(show[15], "line 15 should be visible (5 after change)");
        // Lines 16+ should be hidden
        assert!(!show[16], "line 16 should be hidden");
    }

    #[test]
    fn test_filter_with_elided_inserts_markers() {
        let lines = vec![
            DiffLine::new(LineSource::Base, "a".into(), ' ', Some(1)),
            DiffLine::new(LineSource::Base, "b".into(), ' ', Some(2)),
            DiffLine::new(LineSource::Base, "c".into(), ' ', Some(3)),
            DiffLine::new(LineSource::Committed, "d".into(), '+', Some(4)),
            DiffLine::new(LineSource::Base, "e".into(), ' ', Some(5)),
            DiffLine::new(LineSource::Base, "f".into(), ' ', Some(6)),
        ];
        let show = vec![false, false, false, true, false, false];

        let filtered = filter_with_elided(&lines, &show);

        assert_eq!(filtered.len(), 3); // elided + change + elided
        assert_eq!(filtered[0].source, LineSource::Elided);
        assert_eq!(filtered[0].content, "3 lines"); // 3 hidden lines before
        assert_eq!(filtered[1].content, "d");
        assert_eq!(filtered[2].source, LineSource::Elided);
        assert_eq!(filtered[2].content, "2 lines"); // 2 hidden lines after
    }

    #[test]
    fn test_apply_view_mode_full_returns_all() {
        let lines = vec![
            DiffLine::file_header("test.rs"),
            DiffLine::new(LineSource::Base, "a".into(), ' ', Some(1)),
            DiffLine::new(LineSource::Committed, "b".into(), '+', Some(2)),
        ];
        let result = apply_view_mode(&lines, &crate::app::ViewMode::Full);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_apply_view_mode_changes_only() {
        let lines = vec![
            DiffLine::file_header("test.rs"),
            DiffLine::new(LineSource::Base, "a".into(), ' ', Some(1)),
            DiffLine::new(LineSource::Committed, "b".into(), '+', Some(2)),
            DiffLine::new(LineSource::DeletedBase, "c".into(), '-', Some(3)),
        ];
        let result = apply_view_mode(&lines, &crate::app::ViewMode::ChangesOnly);
        assert_eq!(result.len(), 3); // header + 2 changes, no base
        assert_eq!(result[0].source, LineSource::FileHeader);
        assert_eq!(result[1].source, LineSource::Committed);
        assert_eq!(result[2].source, LineSource::DeletedBase);
    }
}
