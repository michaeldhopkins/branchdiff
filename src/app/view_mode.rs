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

    /// Compute visibility using a predicate to determine "interesting" lines.
    /// Lines within CONTEXT_LINES of interesting lines are shown; headers always show.
    fn compute_visibility_with_predicate(&self, is_interesting: impl Fn(&DiffLine) -> bool) -> Vec<bool> {
        const CONTEXT_LINES: usize = 5;

        let interesting: Vec<bool> = self
            .lines
            .iter()
            .map(|line| line.source.is_header() || is_interesting(line))
            .collect();

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

    /// Compute which original line indices are visible in context mode
    fn compute_context_visibility(&self) -> Vec<bool> {
        self.compute_visibility_with_predicate(|line| {
            line.old_content.is_some()
                || !line.inline_spans.is_empty()
                || line.source.is_change()
        })
    }

    /// Compute which original line indices are visible in commit-only mode (jj @).
    /// After the base visibility pass, suppress entire file sections where @
    /// made no changes — otherwise file headers (always "interesting") would
    /// pull in context lines for files touched only by earlier stack commits.
    fn compute_commit_only_visibility(&self) -> Vec<bool> {
        let mut show = self.compute_visibility_with_predicate(|line| line.is_current_commit());

        let mut file_start: Option<usize> = None;
        let mut has_commit_lines = false;

        for (i, line) in self.lines.iter().enumerate() {
            if line.source.is_header() {
                if let Some(start) = file_start
                    && !has_commit_lines
                {
                    for s in &mut show[start..i] {
                        *s = false;
                    }
                }
                file_start = Some(i);
                has_commit_lines = false;
            } else if line.is_current_commit() {
                has_commit_lines = true;
            }
        }
        if let Some(start) = file_start
            && !has_commit_lines
        {
            for s in &mut show[start..] {
                *s = false;
            }
        }

        show
    }

    /// Build filtered lines with elided markers for a visibility-based mode.
    /// Returns (filtered_lines, mapping from filtered index to original index)
    fn build_lines_with_mapping_from_visibility(&self, show: &[bool]) -> (Vec<DiffLine>, Vec<Option<usize>>) {
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

    /// Build filtered lines with elided markers for context mode
    pub fn build_context_lines_with_mapping(&self) -> (Vec<DiffLine>, Vec<Option<usize>>) {
        let show = self.compute_context_visibility();
        self.build_lines_with_mapping_from_visibility(&show)
    }

    /// Build filtered lines with elided markers for commit-only mode
    fn build_commit_only_lines_with_mapping(&self) -> (Vec<DiffLine>, Vec<Option<usize>>) {
        let show = self.compute_commit_only_visibility();
        self.build_lines_with_mapping_from_visibility(&show)
    }

    /// Build filtered lines with elided markers for bookmark-only mode
    fn build_bookmark_only_lines_with_mapping(&self) -> (Vec<DiffLine>, Vec<Option<usize>>) {
        let show = self.compute_bookmark_only_visibility();
        self.build_lines_with_mapping_from_visibility(&show)
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
            ViewMode::CommitOnly => self.compute_commit_only_items(),
            ViewMode::BookmarkOnly => self.compute_bookmark_only_items(),
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

    /// Build displayable items from a visibility array, inserting Elided markers for gaps
    fn build_items_from_visibility(&self, show: &[bool]) -> Vec<super::DisplayableItem> {
        use super::DisplayableItem;

        let mut result = Vec::new();
        let mut last_shown: Option<usize> = None;

        for (i, &is_shown) in show.iter().enumerate() {
            if is_shown {
                if let Some(last) = last_shown {
                    let gap = i - last - 1;
                    if gap > 0 {
                        result.push(DisplayableItem::Elided(gap));
                    }
                } else if i > 0 {
                    result.push(DisplayableItem::Elided(i));
                }
                result.push(DisplayableItem::Line(i));
                last_shown = Some(i);
            }
        }

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

    /// Context mode: show context around changes with Elided markers
    fn compute_context_items(&self) -> Vec<super::DisplayableItem> {
        let show = self.compute_context_visibility();
        self.build_items_from_visibility(&show)
    }

    /// Commit-only mode (jj): show only current commit changes with context
    fn compute_commit_only_items(&self) -> Vec<super::DisplayableItem> {
        let show = self.compute_commit_only_visibility();
        let items = self.build_items_from_visibility(&show);
        if items.is_empty() {
            return vec![super::DisplayableItem::Message(
                "No changes in current commit (@)",
            )];
        }
        items
    }

    /// Compute which line indices are visible in bookmark-only mode (jj).
    /// Similar to commit-only but uses `is_current_bookmark()` as the predicate.
    fn compute_bookmark_only_visibility(&self) -> Vec<bool> {
        let mut show = self.compute_visibility_with_predicate(|line| line.is_current_bookmark());

        let mut file_start: Option<usize> = None;
        let mut has_bookmark_lines = false;

        for (i, line) in self.lines.iter().enumerate() {
            if line.source.is_header() {
                if let Some(start) = file_start
                    && !has_bookmark_lines
                {
                    for s in &mut show[start..i] {
                        *s = false;
                    }
                }
                file_start = Some(i);
                has_bookmark_lines = false;
            } else if line.is_current_bookmark() {
                has_bookmark_lines = true;
            }
        }
        if let Some(start) = file_start
            && !has_bookmark_lines
        {
            for s in &mut show[start..] {
                *s = false;
            }
        }

        show
    }

    /// Bookmark-only mode (jj): show only current bookmark's changes with context
    fn compute_bookmark_only_items(&self) -> Vec<super::DisplayableItem> {
        let show = self.compute_bookmark_only_visibility();
        let items = self.build_items_from_visibility(&show);
        if items.is_empty() {
            return vec![super::DisplayableItem::Message(
                "No changes in current bookmark",
            )];
        }
        items
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
                DisplayableItem::Message(_) => {
                    result.push(item);
                }
            }
        }

        result
    }

    pub fn cycle_view_mode(&mut self) {
        use crate::vcs::VcsBackend;

        let is_jj = self.comparison.vcs_backend == VcsBackend::Jj;

        if self.lines.is_empty() {
            self.view.view_mode = match self.view.view_mode {
                ViewMode::Full => ViewMode::Context,
                ViewMode::Context => ViewMode::ChangesOnly,
                ViewMode::ChangesOnly if is_jj => ViewMode::CommitOnly,
                ViewMode::ChangesOnly => ViewMode::Full,
                ViewMode::CommitOnly if is_jj => ViewMode::BookmarkOnly,
                ViewMode::CommitOnly => ViewMode::Full,
                ViewMode::BookmarkOnly => ViewMode::Full,
            };
            self.view.needs_inline_spans = true;
            return;
        }

        let middle_offset = self.view.viewport_height / 2;
        let anchor_original_idx = self.get_original_index_at_offset(middle_offset);

        self.view.view_mode = match self.view.view_mode {
            ViewMode::Full => ViewMode::Context,
            ViewMode::Context => ViewMode::ChangesOnly,
            ViewMode::ChangesOnly if is_jj => ViewMode::CommitOnly,
            ViewMode::ChangesOnly => ViewMode::Full,
            ViewMode::CommitOnly if is_jj => ViewMode::BookmarkOnly,
            ViewMode::CommitOnly => ViewMode::Full,
            ViewMode::BookmarkOnly => ViewMode::Full,
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
            ViewMode::Context | ViewMode::CommitOnly | ViewMode::BookmarkOnly => {
                let (_, index_map) = match self.view.view_mode {
                    ViewMode::Context => self.build_context_lines_with_mapping(),
                    ViewMode::BookmarkOnly => self.build_bookmark_only_lines_with_mapping(),
                    _ => self.build_commit_only_lines_with_mapping(),
                };
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
            ViewMode::Context | ViewMode::CommitOnly | ViewMode::BookmarkOnly => {
                let (_, index_map) = match self.view.view_mode {
                    ViewMode::Context => self.build_context_lines_with_mapping(),
                    ViewMode::BookmarkOnly => self.build_bookmark_only_lines_with_mapping(),
                    _ => self.build_commit_only_lines_with_mapping(),
                };
                let visibility = match self.view.view_mode {
                    ViewMode::Context => self.compute_context_visibility(),
                    ViewMode::BookmarkOnly => self.compute_bookmark_only_visibility(),
                    _ => self.compute_commit_only_visibility(),
                };

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
    use crate::app::{DisplayableItem, ViewMode};
    use crate::diff::{DiffLine, LineSource};
    use crate::test_support::{base_line, TestAppBuilder};
    use crate::vcs::VcsBackend;

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

    // === CommitOnly view mode tests ===

    fn collect_visible_lines<'a>(app: &'a crate::app::App, items: &[DisplayableItem]) -> Vec<&'a DiffLine> {
        items.iter().filter_map(|item| match item {
            DisplayableItem::Line(idx) => Some(&app.lines[*idx]),
            _ => None,
        }).collect()
    }

    #[test]
    fn test_cycle_jj_includes_commit_only() {
        let lines = vec![base_line("ctx"), DiffLine::new(LineSource::Staged, "current".to_string(), '+', None)];
        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::Full;

        app.cycle_view_mode();
        assert_eq!(app.view.view_mode, ViewMode::Context);
        app.cycle_view_mode();
        assert_eq!(app.view.view_mode, ViewMode::ChangesOnly);
        app.cycle_view_mode();
        assert_eq!(app.view.view_mode, ViewMode::CommitOnly);
        app.cycle_view_mode();
        assert_eq!(app.view.view_mode, ViewMode::BookmarkOnly);
        app.cycle_view_mode();
        assert_eq!(app.view.view_mode, ViewMode::Full);
    }

    #[test]
    fn test_cycle_git_skips_commit_only() {
        let lines = vec![base_line("ctx"), DiffLine::new(LineSource::Staged, "staged".to_string(), '+', None)];
        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_vcs_backend(VcsBackend::Git)
            .build();
        app.view.view_mode = ViewMode::Full;

        app.cycle_view_mode();
        assert_eq!(app.view.view_mode, ViewMode::Context);
        app.cycle_view_mode();
        assert_eq!(app.view.view_mode, ViewMode::ChangesOnly);
        app.cycle_view_mode();
        assert_eq!(app.view.view_mode, ViewMode::Full);
    }

    #[test]
    fn test_cycle_jj_empty_lines_includes_commit_only() {
        let mut app = TestAppBuilder::new()
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::ChangesOnly;
        app.cycle_view_mode();
        assert_eq!(app.view.view_mode, ViewMode::CommitOnly);
        app.cycle_view_mode();
        assert_eq!(app.view.view_mode, ViewMode::BookmarkOnly);
        app.cycle_view_mode();
        assert_eq!(app.view.view_mode, ViewMode::Full);
    }

    #[test]
    fn test_commit_only_shows_staged_with_context() {
        // 20 base lines, then a Staged line (current commit), then 20 base lines
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(base_line(&format!("before{}", i)));
        }
        lines.push(DiffLine::new(LineSource::Staged, "current_commit_add".to_string(), '+', Some(21)));
        for i in 0..20 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::CommitOnly;

        let items = app.compute_displayable_items();
        let visible = collect_visible_lines(&app, &items);

        assert!(visible.iter().any(|l| l.content == "current_commit_add"),
            "Staged line should be visible in CommitOnly mode");

        // Context lines within ±5 should be visible
        assert!(visible.iter().any(|l| l.content == "before15"),
            "Context before should be visible");
        assert!(visible.iter().any(|l| l.content == "after4"),
            "Context after should be visible");

        // Lines far from the change should not be visible
        assert!(!visible.iter().any(|l| l.content == "before0"),
            "Far-away lines should be hidden");
    }

    #[test]
    fn test_commit_only_hides_committed_only_lines() {
        // Committed (earlier commits) line should NOT trigger visibility
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(base_line(&format!("before{}", i)));
        }
        lines.push(DiffLine::new(LineSource::Committed, "earlier_commit".to_string(), '+', Some(21)));
        for i in 0..20 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::CommitOnly;

        let items = app.compute_displayable_items();
        let visible = collect_visible_lines(&app, &items);

        // The Committed line should NOT be visible (it's not current commit)
        assert!(!visible.iter().any(|l| l.content == "earlier_commit"),
            "Committed (earlier commit) line should be hidden in CommitOnly mode");
    }

    #[test]
    fn test_commit_only_shows_deleted_committed_with_context() {
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(base_line(&format!("before{}", i)));
        }
        lines.push(DiffLine::new(LineSource::DeletedCommitted, "deleted_in_current".to_string(), '-', None));
        for i in 0..20 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::CommitOnly;

        let items = app.compute_displayable_items();
        let visible = collect_visible_lines(&app, &items);

        assert!(visible.iter().any(|l| l.content == "deleted_in_current"),
            "DeletedCommitted should be visible in CommitOnly mode");
    }

    #[test]
    fn test_commit_only_shows_base_with_staged_change_source() {
        // Base line with change_source=Staged (inline modification in current commit)
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(base_line(&format!("before{}", i)));
        }
        let mut modified = DiffLine::new(LineSource::Base, "modified_content".to_string(), ' ', Some(21));
        modified.change_source = Some(LineSource::Staged);
        modified.old_content = Some("old_content".to_string());
        lines.push(modified);
        for i in 0..20 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::CommitOnly;

        let items = app.compute_displayable_items();
        let visible = collect_visible_lines(&app, &items);

        assert!(visible.iter().any(|l| l.content == "modified_content"),
            "Base line with change_source=Staged should be visible in CommitOnly mode");
    }

    #[test]
    fn test_commit_only_context_preserves_other_commit_lines() {
        // A Committed line (earlier commit) within ±5 of a Staged line should be
        // visible as context, with its original source preserved
        let mut lines = Vec::new();
        lines.push(base_line("before"));
        lines.push(DiffLine::new(LineSource::Committed, "earlier_nearby".to_string(), '+', Some(2)));
        lines.push(base_line("between"));
        lines.push(DiffLine::new(LineSource::Staged, "current_commit".to_string(), '+', Some(4)));
        lines.push(base_line("after"));

        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::CommitOnly;

        let items = app.compute_displayable_items();
        let visible = collect_visible_lines(&app, &items);

        let earlier = visible.iter().find(|l| l.content == "earlier_nearby");
        assert!(earlier.is_some(), "Nearby Committed line should be visible as context");
        assert_eq!(earlier.unwrap().source, LineSource::Committed,
            "Committed line should retain its original source");
    }

    #[test]
    fn test_backend_switch_falls_back_from_commit_only() {
        let mut app = TestAppBuilder::new()
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::CommitOnly;

        // Simulate a refresh that switches backend to Git
        let result = crate::app::RefreshResult {
            files: vec![],
            lines: vec![],
            base_identifier: "abc".to_string(),
            base_label: None,
            current_branch: Some("main".to_string()),
            metrics: crate::limits::DiffMetrics::default(),
            file_links: std::collections::HashMap::new(),
            stack_position: None, bookmark_name: None,
            revision_id: None,
        };
        app.comparison.vcs_backend = VcsBackend::Git;
        app.apply_refresh_result(result);

        assert_eq!(app.view.view_mode, ViewMode::Context,
            "Should fall back from CommitOnly to Context when backend switches to Git");
    }

    #[test]
    fn test_commit_only_shows_canceled_staged() {
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(base_line(&format!("before{}", i)));
        }
        lines.push(DiffLine::new(LineSource::CanceledStaged, "canceled_in_child".to_string(), '±', None));
        for i in 0..20 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::CommitOnly;

        let items = app.compute_displayable_items();
        let visible = collect_visible_lines(&app, &items);

        assert!(visible.iter().any(|l| l.content == "canceled_in_child"),
            "CanceledStaged should be visible in CommitOnly mode");
        assert!(visible.iter().any(|l| l.content == "before15"),
            "Context before CanceledStaged should be visible");
    }

    #[test]
    fn test_commit_only_no_current_commit_lines_shows_message() {
        // Empty @ change: only earlier-commit and base lines, no current-commit lines
        let mut lines = Vec::new();
        lines.push(DiffLine::file_header("test.rs"));
        for i in 0..20 {
            lines.push(base_line(&format!("base{}", i)));
        }
        lines.push(DiffLine::new(LineSource::Committed, "earlier".to_string(), '+', Some(21)));
        for i in 0..10 {
            lines.push(base_line(&format!("more{}", i)));
        }

        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::CommitOnly;

        let items = app.compute_displayable_items();

        assert_eq!(items.len(), 1, "Should have exactly one item (the message)");
        assert_eq!(
            items[0],
            DisplayableItem::Message("No changes in current commit (@)"),
            "Should show empty-state message when @ has no changes"
        );
    }

    #[test]
    fn test_commit_only_produces_elided_markers() {
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(base_line(&format!("before{}", i)));
        }
        lines.push(DiffLine::new(LineSource::Staged, "current_add".to_string(), '+', Some(21)));
        for i in 0..20 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::CommitOnly;

        let items = app.compute_displayable_items();

        let elided_count = items.iter()
            .filter(|item| matches!(item, DisplayableItem::Elided(_)))
            .count();
        assert!(elided_count >= 1,
            "Should have at least one Elided marker for hidden lines, got {}", elided_count);

        // Verify the elided marker has a reasonable count
        let total_elided_lines: usize = items.iter()
            .filter_map(|item| match item {
                DisplayableItem::Elided(n) => Some(*n),
                _ => None,
            })
            .sum();
        // 41 total lines, ~11 visible (5 context before + staged + 5 context after),
        // so ~30 lines should be elided
        assert!(total_elided_lines > 20,
            "Elided markers should account for hidden lines, got {}", total_elided_lines);
    }

    #[test]
    fn test_commit_only_hides_files_with_no_current_commit_changes() {
        // Two files: file1 has Staged lines (current commit), file2 has only Committed lines
        let mut lines = Vec::new();

        // File 1: has current-commit changes
        lines.push(DiffLine::file_header("current.rs"));
        lines.push(base_line("unchanged"));
        lines.push(DiffLine::new(LineSource::Staged, "new_in_current".to_string(), '+', Some(2)));
        lines.push(base_line("more_unchanged"));

        // File 2: only earlier-commit changes (no current-commit lines)
        lines.push(DiffLine::file_header("earlier.rs"));
        for i in 0..10 {
            lines.push(base_line(&format!("base{}", i)));
        }
        lines.push(DiffLine::new(LineSource::Committed, "from_parent".to_string(), '+', Some(11)));

        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::CommitOnly;

        let items = app.compute_displayable_items();
        let visible = collect_visible_lines(&app, &items);

        assert!(visible.iter().any(|l| l.content == "current.rs"),
            "File with current-commit changes should be visible");
        assert!(visible.iter().any(|l| l.content == "new_in_current"),
            "Current-commit line should be visible");
        assert!(!visible.iter().any(|l| l.content == "earlier.rs"),
            "File with no current-commit changes should be hidden");
        assert!(!visible.iter().any(|l| l.content == "from_parent"),
            "Committed-only line should be hidden");
    }

    // === BookmarkOnly view mode tests ===

    fn make_bookmark_line(source: LineSource, content: &str, in_bookmark: bool) -> DiffLine {
        let prefix = match source {
            LineSource::Committed | LineSource::Staged | LineSource::Unstaged => '+',
            LineSource::DeletedBase | LineSource::DeletedCommitted | LineSource::DeletedStaged => '-',
            _ => ' ',
        };
        let mut line = DiffLine::new(source, content.to_string(), prefix, None);
        line.in_current_bookmark = Some(in_bookmark);
        line
    }

    #[test]
    fn test_bookmark_only_shows_current_bookmark_lines() {
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(base_line(&format!("before{}", i)));
        }
        lines.push(make_bookmark_line(LineSource::Committed, "in_bookmark", true));
        for i in 0..20 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::BookmarkOnly;

        let items = app.compute_displayable_items();
        let visible = collect_visible_lines(&app, &items);

        assert!(visible.iter().any(|l| l.content == "in_bookmark"),
            "Current bookmark line should be visible");
        assert!(visible.iter().any(|l| l.content == "before15"),
            "Context before should be visible");
    }

    #[test]
    fn test_bookmark_only_hides_earlier_bookmark_lines() {
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(base_line(&format!("before{}", i)));
        }
        lines.push(make_bookmark_line(LineSource::Committed, "earlier_bookmark", false));
        for i in 0..20 {
            lines.push(base_line(&format!("after{}", i)));
        }

        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::BookmarkOnly;

        let items = app.compute_displayable_items();
        let visible = collect_visible_lines(&app, &items);

        assert!(!visible.iter().any(|l| l.content == "earlier_bookmark"),
            "Earlier bookmark line should be hidden");
    }

    #[test]
    fn test_bookmark_only_shows_earlier_as_context() {
        let mut lines = Vec::new();
        lines.push(base_line("before"));
        lines.push(make_bookmark_line(LineSource::Committed, "earlier_nearby", false));
        lines.push(base_line("between"));
        lines.push(make_bookmark_line(LineSource::Staged, "current_bookmark", true));
        lines.push(base_line("after"));

        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::BookmarkOnly;

        let items = app.compute_displayable_items();
        let visible = collect_visible_lines(&app, &items);

        let earlier = visible.iter().find(|l| l.content == "earlier_nearby");
        assert!(earlier.is_some(), "Nearby earlier bookmark line should be visible as context");
        assert_eq!(earlier.unwrap().source, LineSource::Committed,
            "Earlier bookmark line should retain its original source");
    }

    #[test]
    fn test_bookmark_only_hides_files_with_no_bookmark_changes() {
        let mut lines = Vec::new();

        // File 1: has current-bookmark changes
        lines.push(DiffLine::file_header("current.rs"));
        lines.push(base_line("unchanged"));
        lines.push(make_bookmark_line(LineSource::Committed, "in_bookmark", true));

        // File 2: only earlier-bookmark changes
        let mut header = DiffLine::file_header("earlier.rs");
        header.in_current_bookmark = Some(false);
        lines.push(header);
        for i in 0..10 {
            lines.push(base_line(&format!("base{}", i)));
        }
        lines.push(make_bookmark_line(LineSource::Committed, "from_earlier", false));

        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::BookmarkOnly;

        let items = app.compute_displayable_items();
        let visible = collect_visible_lines(&app, &items);

        assert!(visible.iter().any(|l| l.content == "in_bookmark"),
            "Current bookmark line should be visible");
        assert!(!visible.iter().any(|l| l.content == "earlier.rs"),
            "File with no bookmark changes should be hidden");
    }

    #[test]
    fn test_bookmark_only_no_changes_shows_message() {
        let mut lines = Vec::new();
        lines.push(DiffLine::file_header("test.rs"));
        for i in 0..20 {
            lines.push(base_line(&format!("base{}", i)));
        }
        lines.push(make_bookmark_line(LineSource::Committed, "earlier", false));

        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::BookmarkOnly;

        let items = app.compute_displayable_items();

        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0],
            DisplayableItem::Message("No changes in current bookmark"),
        );
    }

    #[test]
    fn test_backend_switch_falls_back_from_bookmark_only() {
        let mut app = TestAppBuilder::new()
            .with_vcs_backend(VcsBackend::Jj)
            .build();
        app.view.view_mode = ViewMode::BookmarkOnly;

        let result = crate::app::RefreshResult {
            files: vec![],
            lines: vec![],
            base_identifier: "abc".to_string(),
            base_label: None,
            current_branch: Some("main".to_string()),
            metrics: crate::limits::DiffMetrics::default(),
            file_links: std::collections::HashMap::new(),
            stack_position: None, bookmark_name: None,
            revision_id: None,
        };
        app.comparison.vcs_backend = VcsBackend::Git;
        app.apply_refresh_result(result);

        assert_eq!(app.view.view_mode, ViewMode::Context,
            "Should fall back from BookmarkOnly to Context when backend switches to Git");
    }
}
