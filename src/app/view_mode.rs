use crate::diff::DiffLine;

use super::{App, ViewMode};

impl App {
    pub fn changed_line_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| line.is_change())
            .count()
    }

    pub fn additions_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| line.source.is_addition())
            .count()
    }

    pub fn deletions_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| line.source.is_deletion())
            .count()
    }

    /// Compute which original line indices are visible in context mode
    fn compute_context_visibility(&self) -> Vec<bool> {
        const CONTEXT_LINES: usize = 5;

        // First pass: mark which lines are "interesting" (changes or headers)
        let interesting: Vec<bool> = self
            .lines
            .iter()
            .map(|line| {
                // Lines with modifications are always interesting
                line.old_content.is_some()
                    || !line.inline_spans.is_empty()
                    || line.source.is_change()
                    || line.source.is_header()
            })
            .collect();

        // Second pass: mark lines within CONTEXT_LINES of interesting lines
        let mut show = vec![false; self.lines.len()];
        for (i, &is_interesting) in interesting.iter().enumerate() {
            if is_interesting {
                let start = i.saturating_sub(CONTEXT_LINES);
                let end = (i + CONTEXT_LINES + 1).min(self.lines.len());
                for item in show.iter_mut().take(end).skip(start) {
                    *item = true;
                }
            }
        }
        show
    }

    /// Build filtered lines with elided markers for context-only mode
    /// Returns (filtered_lines, mapping from filtered index to original index)
    pub fn build_context_lines_with_mapping(&self) -> (Vec<DiffLine>, Vec<Option<usize>>) {
        let show = self.compute_context_visibility();

        // Build result with elided markers between gaps
        let mut result = Vec::new();
        let mut index_map = Vec::new(); // Maps filtered index -> original index (None for elided)
        let mut last_shown: Option<usize> = None;

        for (i, line) in self.lines.iter().enumerate() {
            if show[i] {
                // Check if there's a gap since last shown line
                if let Some(last) = last_shown {
                    let gap = i - last - 1;
                    if gap > 0 {
                        result.push(DiffLine::elided(gap));
                        index_map.push(None); // Elided marker has no original index
                    }
                }
                result.push(line.clone());
                index_map.push(Some(i));
                last_shown = Some(i);
            }
        }

        // Handle trailing gap - count elided lines after last shown line
        if let Some(last) = last_shown {
            let trailing = self.lines.len().saturating_sub(last + 1);
            let trailing_hidden: usize = (last + 1..self.lines.len())
                .filter(|&i| !show[i])
                .count();
            if trailing_hidden > 0 && trailing > 0 {
                result.push(DiffLine::elided(trailing_hidden));
                index_map.push(None);
            }
        }

        (result, index_map)
    }

    pub(super) fn build_changes_only_lines(&self) -> Vec<DiffLine> {
        self.lines
            .iter()
            .filter(|line| line.source.is_change() || line.source.is_header())
            .cloned()
            .collect()
    }

    /// Compute displayable items as indices (more efficient than cloning lines)
    pub fn compute_displayable_items(&self) -> Vec<super::DisplayableItem> {
        let items = match self.view.view_mode {
            ViewMode::Full => self.compute_full_items(),
            ViewMode::Context => self.compute_context_items(),
            ViewMode::ChangesOnly => self.compute_changes_only_items(),
        };
        self.filter_collapsed_items(items)
    }

    /// Full mode: all lines as indices
    fn compute_full_items(&self) -> Vec<super::DisplayableItem> {
        (0..self.lines.len())
            .map(super::DisplayableItem::Line)
            .collect()
    }

    /// Changes-only mode: filter to just change lines (including modified base lines)
    fn compute_changes_only_items(&self) -> Vec<super::DisplayableItem> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, line)| {
                line.source.is_change()
                    || line.source.is_header()
                    || line.old_content.is_some()  // Include modified base lines
            })
            .map(|(i, _)| super::DisplayableItem::Line(i))
            .collect()
    }

    /// Context mode: show context around changes with Elided markers
    fn compute_context_items(&self) -> Vec<super::DisplayableItem> {
        use super::DisplayableItem;

        let show = self.compute_context_visibility();

        let mut result = Vec::new();
        let mut last_shown: Option<usize> = None;

        for (i, &is_shown) in show.iter().enumerate() {
            if is_shown {
                // Check if there's a gap since last shown line
                if let Some(last) = last_shown {
                    let gap = i - last - 1;
                    if gap > 0 {
                        result.push(DisplayableItem::Elided(gap));
                    }
                }
                result.push(DisplayableItem::Line(i));
                last_shown = Some(i);
            }
        }

        // Handle trailing gap
        if let Some(last) = last_shown {
            let trailing_hidden: usize = (last + 1..self.lines.len())
                .filter(|&i| !show[i])
                .count();
            if trailing_hidden > 0 {
                result.push(DisplayableItem::Elided(trailing_hidden));
            }
        }

        result
    }

    /// Filter out items belonging to collapsed files (keep headers)
    fn filter_collapsed_items(&self, items: Vec<super::DisplayableItem>) -> Vec<super::DisplayableItem> {
        use super::DisplayableItem;

        if self.view.collapsed_files.is_empty() {
            return items;
        }

        let mut current_file: Option<String> = None;
        let mut result = Vec::new();

        for item in items {
            match item {
                DisplayableItem::Line(idx) => {
                    let line = &self.lines[idx];

                    // Update current file when we see a file header
                    if line.source.is_header() {
                        current_file = line.file_path.clone();
                        result.push(item); // Always show file headers
                        continue;
                    }

                    // Use line's file_path if available, otherwise use tracked current_file
                    let file_path = line.file_path.as_ref().or(current_file.as_ref());

                    // Hide lines from collapsed files
                    let should_show = if let Some(path) = file_path {
                        !self.view.collapsed_files.contains(path)
                    } else {
                        true
                    };

                    if should_show {
                        result.push(item);
                    }
                }
                DisplayableItem::Elided(_) => {
                    // Elided markers don't have file_path, use current_file
                    let should_show = if let Some(ref path) = current_file {
                        !self.view.collapsed_files.contains(path)
                    } else {
                        true
                    };

                    if should_show {
                        result.push(item);
                    }
                }
            }
        }

        result
    }

    pub fn cycle_view_mode(&mut self) {
        if self.lines.is_empty() {
            self.view.view_mode = match self.view.view_mode {
                ViewMode::Full => ViewMode::Context,
                ViewMode::Context => ViewMode::ChangesOnly,
                ViewMode::ChangesOnly => ViewMode::Full,
            };
            self.view.needs_inline_spans = true;
            return;
        }

        let middle_offset = self.view.viewport_height / 2;
        let anchor_original_idx = self.get_original_index_at_offset(middle_offset);

        self.view.view_mode = match self.view.view_mode {
            ViewMode::Full => ViewMode::Context,
            ViewMode::Context => ViewMode::ChangesOnly,
            ViewMode::ChangesOnly => ViewMode::Full,
        };

        if let Some(anchor_idx) = anchor_original_idx {
            let new_position = self.find_position_for_original_index(anchor_idx);
            self.view.scroll_offset = new_position.saturating_sub(middle_offset);
        }

        self.clamp_scroll();
        self.view.needs_inline_spans = true;
    }

    fn get_original_index_at_offset(&self, offset: usize) -> Option<usize> {
        let target_pos = self.view.scroll_offset + offset;

        match self.view.view_mode {
            ViewMode::Full => {
                if target_pos < self.lines.len() {
                    Some(target_pos)
                } else if !self.lines.is_empty() {
                    Some(self.lines.len() - 1)
                } else {
                    None
                }
            }
            ViewMode::Context => {
                let (_, index_map) = self.build_context_lines_with_mapping();
                if target_pos < index_map.len() {
                    if let Some(idx) = index_map[target_pos] {
                        return Some(idx);
                    }
                    for delta in 1..index_map.len() {
                        if target_pos >= delta
                            && let Some(Some(idx)) = index_map.get(target_pos - delta)
                        {
                            return Some(*idx);
                        }
                        if let Some(Some(idx)) = index_map.get(target_pos + delta) {
                            return Some(*idx);
                        }
                    }
                }
                index_map.iter().rev().find_map(|x| *x)
            }
            ViewMode::ChangesOnly => {
                let displayed = self.build_changes_only_lines();
                if target_pos < displayed.len() {
                    let target_line = &displayed[target_pos];
                    self.lines.iter().position(|l| {
                        l.source == target_line.source
                            && l.content == target_line.content
                            && l.line_number == target_line.line_number
                    })
                } else if !displayed.is_empty() {
                    Some(self.lines.len().saturating_sub(1))
                } else {
                    None
                }
            }
        }
    }

    pub fn find_position_for_original_index(&self, original_idx: usize) -> usize {
        match self.view.view_mode {
            ViewMode::Full => original_idx.min(self.lines.len().saturating_sub(1)),
            ViewMode::Context => {
                let (_, index_map) = self.build_context_lines_with_mapping();
                let visibility = self.compute_context_visibility();

                if original_idx < visibility.len() && visibility[original_idx] {
                    for (pos, mapped_idx) in index_map.iter().enumerate() {
                        if *mapped_idx == Some(original_idx) {
                            return pos;
                        }
                    }
                }

                let mut best_pos = 0;
                let mut best_distance = usize::MAX;

                for (pos, mapped_idx) in index_map.iter().enumerate() {
                    if let Some(idx) = mapped_idx {
                        let distance = (*idx).abs_diff(original_idx);
                        if distance < best_distance {
                            best_distance = distance;
                            best_pos = pos;
                        }
                    }
                }

                best_pos
            }
            ViewMode::ChangesOnly => {
                let displayed = self.build_changes_only_lines();
                if original_idx < self.lines.len() {
                    let target = &self.lines[original_idx];
                    for (pos, line) in displayed.iter().enumerate() {
                        if line.source == target.source
                            && line.content == target.content
                            && line.line_number == target.line_number
                        {
                            return pos;
                        }
                    }
                }
                0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::diff::{DiffLine, LineSource};
    use crate::test_support::TestAppBuilder;

    #[test]
    fn test_additions_count() {
        let lines = vec![
            DiffLine::new(LineSource::Committed, "+new".to_string(), '+', None),
            DiffLine::new(LineSource::Staged, "+staged".to_string(), '+', None),
            DiffLine::new(LineSource::Unstaged, "+unstaged".to_string(), '+', None),
            DiffLine::new(LineSource::Base, " context".to_string(), ' ', None),
        ];
        let app = TestAppBuilder::new().with_lines(lines).build();
        assert_eq!(app.additions_count(), 3);
    }

    #[test]
    fn test_deletions_count() {
        let lines = vec![
            DiffLine::new(LineSource::DeletedBase, "-old".to_string(), '-', None),
            DiffLine::new(LineSource::DeletedCommitted, "-old2".to_string(), '-', None),
            DiffLine::new(LineSource::DeletedStaged, "-old3".to_string(), '-', None),
            DiffLine::new(LineSource::Base, " context".to_string(), ' ', None),
        ];
        let app = TestAppBuilder::new().with_lines(lines).build();
        assert_eq!(app.deletions_count(), 3);
    }

    #[test]
    fn test_canceled_lines_excluded_from_counts() {
        let lines = vec![
            DiffLine::new(LineSource::Committed, "+new".to_string(), '+', None),
            DiffLine::new(LineSource::CanceledCommitted, "+canceled".to_string(), '+', None),
            DiffLine::new(LineSource::CanceledStaged, "+also_canceled".to_string(), '+', None),
            DiffLine::new(LineSource::DeletedBase, "-deleted".to_string(), '-', None),
        ];
        let app = TestAppBuilder::new().with_lines(lines).build();
        assert_eq!(app.additions_count(), 1);
        assert_eq!(app.deletions_count(), 1);
    }
}
