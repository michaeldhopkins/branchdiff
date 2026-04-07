//! Diff computation module for branchdiff
//!
//! This module computes 4-way diffs showing changes across:
//! - base (merge-base with main/master)
//! - head (committed on branch)
//! - index (staged)
//! - working (working tree)

mod algorithm;
pub mod block;
mod cancellation;
mod inline;
mod line_builder;
mod output;
mod provenance;

pub use algorithm::{compute_four_way_diff, DiffInput};
pub use block::{BlockKind, BlockMatch, ChangeBlock};
pub use inline::InlineSpan;

pub(crate) use inline::compute_inline_diff_merged;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineSource {
    Base,
    Committed,
    Staged,
    Unstaged,
    DeletedBase,
    DeletedCommitted,
    DeletedStaged,
    CanceledCommitted,
    CanceledStaged,
    FileHeader,
    Elided,
}

impl LineSource {
    /// True for any line representing a change (addition, deletion, or canceled)
    pub fn is_change(self) -> bool {
        matches!(
            self,
            Self::Committed
                | Self::Staged
                | Self::Unstaged
                | Self::DeletedBase
                | Self::DeletedCommitted
                | Self::DeletedStaged
                | Self::CanceledCommitted
                | Self::CanceledStaged
        )
    }

    /// True for additions (committed, staged, or unstaged)
    pub fn is_addition(self) -> bool {
        matches!(self, Self::Committed | Self::Staged | Self::Unstaged)
    }

    /// True for deletions
    pub fn is_deletion(self) -> bool {
        matches!(
            self,
            Self::DeletedBase | Self::DeletedCommitted | Self::DeletedStaged
        )
    }

    /// True for unstaged changes (working tree modifications)
    pub fn is_unstaged(self) -> bool {
        matches!(self, Self::Unstaged)
    }

    /// True for file/section headers
    pub fn is_header(self) -> bool {
        matches!(self, Self::FileHeader)
    }

    /// True for lines belonging to jj's current commit (@).
    /// In jj, Staged = current commit additions, DeletedCommitted = current commit deletions,
    /// CanceledStaged = added in current commit then removed in child.
    pub fn is_current_commit(self) -> bool {
        matches!(
            self,
            Self::Staged | Self::DeletedCommitted | Self::CanceledStaged
        )
    }
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub source: LineSource,
    pub content: String,
    pub prefix: char,
    pub line_number: Option<usize>,
    pub file_path: Option<String>,
    pub inline_spans: Vec<InlineSpan>,
    pub old_content: Option<String>,
    pub change_source: Option<LineSource>,
    /// Whether this line belongs to the current jj bookmark's scope.
    /// `None` when bookmark boundary info is unavailable.
    pub in_current_bookmark: Option<bool>,
    /// Index into the parent FileDiff's `blocks` vec, if this line is part of a change block.
    pub block_idx: Option<usize>,
    /// If this line is part of a moved block, the file path of the matching block.
    pub move_target: Option<String>,
}

impl DiffLine {
    pub fn new(source: LineSource, content: String, prefix: char, line_number: Option<usize>) -> Self {
        Self {
            source,
            content,
            prefix,
            line_number,
            file_path: None,
            inline_spans: Vec::new(),
            old_content: None,
            change_source: None,
            in_current_bookmark: None,
            block_idx: None,
            move_target: None,
        }
    }

    pub fn with_old_content(mut self, old: &str) -> Self {
        self.old_content = Some(old.to_string());
        self
    }

    pub fn with_change_source(mut self, change_source: LineSource) -> Self {
        self.change_source = Some(change_source);
        self
    }

    pub fn is_change(&self) -> bool {
        self.source.is_change() || self.change_source.is_some_and(|cs| cs.is_change())
    }

    /// True if this line belongs to jj's current commit (@).
    /// Catches both direct current-commit lines and Base lines with inline modifications from @.
    pub fn is_current_commit(&self) -> bool {
        self.source.is_current_commit()
            || self.change_source.is_some_and(|cs| cs.is_current_commit())
    }

    /// True if this line belongs to the current jj bookmark's scope.
    pub fn is_current_bookmark(&self) -> bool {
        self.in_current_bookmark == Some(true)
    }

    pub fn ensure_inline_spans(&mut self) {
        if self.inline_spans.is_empty()
            && let Some(ref old) = self.old_content
        {
            let source = self.change_source.unwrap_or(self.source);
            let result = compute_inline_diff_merged(old, &self.content, source);
            self.inline_spans = result.spans;
        }
    }

    pub fn with_file_path(mut self, path: &str) -> Self {
        self.file_path = Some(path.to_string());
        self
    }

    /// Check if this line is an image marker (for UI rendering)
    pub fn is_image_marker(&self) -> bool {
        self.content == "[image]" && self.file_path.is_some()
    }
}
// Builder methods (file_header, deleted_file_header, renamed_file_header,
// image_marker, elided) are in line_builder.rs

// Cancellation detection functions (index_line_in_working, collect_canceled_*,
// find_insertion_position, insert_canceled_lines) are in cancellation.rs

// Algorithm functions (build_deletion_diff, check_file_deletion, compute_four_way_diff)
// are in algorithm.rs

#[derive(Debug)]
pub struct FileDiff {
    pub lines: Vec<DiffLine>,
    pub blocks: Vec<ChangeBlock>,
    /// Hash of all change line content, for detecting when a file's diff changes.
    pub content_hash: u64,
}

impl FileDiff {
    pub fn new(mut lines: Vec<DiffLine>) -> Self {
        let blocks = block::extract_blocks(&mut lines);
        let content_hash = Self::compute_content_hash(&lines);
        Self { lines, blocks, content_hash }
    }

    fn compute_content_hash(lines: &[DiffLine]) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        for line in lines {
            if line.source.is_change() || line.change_source.is_some() {
                let trimmed = line.content.trim();
                if !trimmed.is_empty() {
                    trimmed.hash(&mut hasher);
                }
            }
        }
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // LineSource classification tests
    #[test]
    fn test_line_source_is_change() {
        // These should be changes
        assert!(LineSource::Committed.is_change());
        assert!(LineSource::Staged.is_change());
        assert!(LineSource::Unstaged.is_change());
        assert!(LineSource::DeletedBase.is_change());
        assert!(LineSource::DeletedCommitted.is_change());
        assert!(LineSource::DeletedStaged.is_change());
        assert!(LineSource::CanceledCommitted.is_change());
        assert!(LineSource::CanceledStaged.is_change());

        // These should NOT be changes
        assert!(!LineSource::Base.is_change());
        assert!(!LineSource::FileHeader.is_change());
        assert!(!LineSource::Elided.is_change());
    }

    #[test]
    fn test_line_source_is_addition() {
        assert!(LineSource::Committed.is_addition());
        assert!(LineSource::Staged.is_addition());
        assert!(LineSource::Unstaged.is_addition());

        assert!(!LineSource::Base.is_addition());
        assert!(!LineSource::DeletedBase.is_addition());
        assert!(!LineSource::CanceledCommitted.is_addition());
    }

    #[test]
    fn test_line_source_is_deletion() {
        assert!(LineSource::DeletedBase.is_deletion());
        assert!(LineSource::DeletedCommitted.is_deletion());
        assert!(LineSource::DeletedStaged.is_deletion());

        assert!(!LineSource::Base.is_deletion());
        assert!(!LineSource::Committed.is_deletion());
        assert!(!LineSource::CanceledCommitted.is_deletion());
    }

    #[test]
    fn test_line_source_is_unstaged() {
        assert!(LineSource::Unstaged.is_unstaged());

        assert!(!LineSource::Base.is_unstaged());
        assert!(!LineSource::Committed.is_unstaged());
        assert!(!LineSource::Staged.is_unstaged());
    }

    #[test]
    fn test_line_source_is_header() {
        assert!(LineSource::FileHeader.is_header());

        assert!(!LineSource::Base.is_header());
        assert!(!LineSource::Committed.is_header());
        assert!(!LineSource::Elided.is_header());
    }

    #[test]
    fn test_line_source_is_current_commit() {
        assert!(LineSource::Staged.is_current_commit());
        assert!(LineSource::DeletedCommitted.is_current_commit());
        assert!(LineSource::CanceledStaged.is_current_commit());

        assert!(!LineSource::Base.is_current_commit());
        assert!(!LineSource::Committed.is_current_commit());
        assert!(!LineSource::Unstaged.is_current_commit());
        assert!(!LineSource::DeletedBase.is_current_commit());
        assert!(!LineSource::DeletedStaged.is_current_commit());
        assert!(!LineSource::CanceledCommitted.is_current_commit());
        assert!(!LineSource::FileHeader.is_current_commit());
        assert!(!LineSource::Elided.is_current_commit());
    }

    #[test]
    fn test_diff_line_is_current_commit_via_source() {
        let staged = DiffLine::new(LineSource::Staged, "added".to_string(), '+', None);
        assert!(staged.is_current_commit());

        let del = DiffLine::new(LineSource::DeletedCommitted, "removed".to_string(), '-', None);
        assert!(del.is_current_commit());

        let base = DiffLine::new(LineSource::Base, "context".to_string(), ' ', None);
        assert!(!base.is_current_commit());

        let committed = DiffLine::new(LineSource::Committed, "earlier".to_string(), '+', None);
        assert!(!committed.is_current_commit());
    }

    #[test]
    fn test_diff_line_is_current_commit_via_change_source() {
        let mut base_with_staged_mod = DiffLine::new(LineSource::Base, "modified".to_string(), ' ', Some(1));
        base_with_staged_mod.change_source = Some(LineSource::Staged);
        assert!(base_with_staged_mod.is_current_commit());

        let mut base_with_committed_mod = DiffLine::new(LineSource::Base, "modified".to_string(), ' ', Some(1));
        base_with_committed_mod.change_source = Some(LineSource::Committed);
        assert!(!base_with_committed_mod.is_current_commit());

        let mut base_with_unstaged_mod = DiffLine::new(LineSource::Base, "modified".to_string(), ' ', Some(1));
        base_with_unstaged_mod.change_source = Some(LineSource::Unstaged);
        assert!(!base_with_unstaged_mod.is_current_commit());
    }

    #[test]
    fn test_is_current_bookmark() {
        let mut line = DiffLine::new(LineSource::Committed, "test".to_string(), '+', None);
        assert!(!line.is_current_bookmark(), "None should be false");

        line.in_current_bookmark = Some(true);
        assert!(line.is_current_bookmark(), "Some(true) should be true");

        line.in_current_bookmark = Some(false);
        assert!(!line.is_current_bookmark(), "Some(false) should be false");
    }

    fn compute_diff_with_inline(
        path: &str,
        base: Option<&str>,
        head: Option<&str>,
        index: Option<&str>,
        working: Option<&str>,
    ) -> FileDiff {
        let mut diff = compute_four_way_diff(DiffInput {
            path,
            base,
            head,
            index,
            working,
            old_path: None,
        });
        for line in &mut diff.lines {
            line.ensure_inline_spans();
        }
        diff
    }

    fn content_lines(diff: &FileDiff) -> Vec<&DiffLine> {
        diff.lines.iter().filter(|l| !l.source.is_header()).collect()
    }

    #[test]
    fn test_compute_file_diff_with_rename() {
        let content = "line1\nline2";
        let diff = compute_four_way_diff(DiffInput {
            path: "new/path.rs",
            base: Some(content),
            head: Some(content),
            index: Some(content),
            working: Some(content),
            old_path: Some("old/path.rs"),
        });
        assert_eq!(diff.lines[0].source, LineSource::FileHeader);
        assert_eq!(diff.lines[0].content, "old/path.rs → new/path.rs");
    }

    #[test]
    fn test_renamed_file_with_content_change() {
        // Simulates: base == head == index, but working has a change
        // This is the case for an unstaged rename with modifications
        let original = "line 1\nline 2\nline 3\nline 4\nline 5";
        let modified = "line 1\nline 2 modified\nline 3\nline 4\nline 5";

        let diff = compute_four_way_diff(DiffInput {
            path: "renamed.txt",
            base: Some(original),    // original content
            head: Some(original),    // same as base (rename not committed)
            index: Some(original),   // same as head (rename not staged)
            working: Some(modified), // modified content
            old_path: Some("original.txt"),
        });

        // Modifications have source=Base but change_source=Unstaged and old_content set
        let modified_lines: Vec<_> = diff
            .lines
            .iter()
            .filter(|l| l.old_content.is_some() || l.change_source.is_some())
            .collect();

        assert!(
            !modified_lines.is_empty(),
            "Expected at least one modified line"
        );

        // Verify the modification is tracked correctly
        let mod_line = modified_lines
            .iter()
            .find(|l| l.content.contains("line 2 modified"))
            .expect("Should have modification for line 2");

        assert_eq!(
            mod_line.old_content.as_deref(),
            Some("line 2"),
            "Should track original content"
        );
        assert_eq!(
            mod_line.change_source,
            Some(LineSource::Unstaged),
            "Should mark as unstaged modification"
        );
    }

    #[test]
    fn test_committed_rename_with_content_change() {
        // Simulates: rename committed with modification
        // base has old content, head/index/working have new content
        let original = "line 1\nline 2\nline 3";
        let modified = "line 1\nline 2 modified\nline 3";

        let diff = compute_four_way_diff(DiffInput {
            path: "renamed.txt",
            base: Some(original),    // original at old path
            head: Some(modified),    // modified (rename+change committed)
            index: Some(modified),   // same as head
            working: Some(modified), // same as head
            old_path: Some("original.txt"),
        });

        // Should show modification as Committed (happened in commit)
        let modified_lines: Vec<_> = diff
            .lines
            .iter()
            .filter(|l| l.old_content.is_some() || l.change_source.is_some())
            .collect();

        assert!(
            !modified_lines.is_empty(),
            "Expected modification to be tracked"
        );

        let mod_line = modified_lines
            .iter()
            .find(|l| l.content.contains("line 2 modified"))
            .expect("Should have modification for line 2");

        assert_eq!(
            mod_line.change_source,
            Some(LineSource::Committed),
            "Should mark as committed modification"
        );
    }

    #[test]
    fn test_staged_rename_with_content_change() {
        // Simulates: rename staged with modification
        // base/head have old content, index/working have new content
        let original = "line 1\nline 2\nline 3";
        let modified = "line 1\nline 2 modified\nline 3";

        let diff = compute_four_way_diff(DiffInput {
            path: "renamed.txt",
            base: Some(original),    // original at old path
            head: Some(original),    // still at old path (not committed)
            index: Some(modified),   // modified (rename+change staged)
            working: Some(modified), // same as index
            old_path: Some("original.txt"),
        });

        // Should show modification as Staged
        let modified_lines: Vec<_> = diff
            .lines
            .iter()
            .filter(|l| l.old_content.is_some() || l.change_source.is_some())
            .collect();

        assert!(
            !modified_lines.is_empty(),
            "Expected modification to be tracked"
        );

        let mod_line = modified_lines
            .iter()
            .find(|l| l.content.contains("line 2 modified"))
            .expect("Should have modification for line 2");

        assert_eq!(
            mod_line.change_source,
            Some(LineSource::Staged),
            "Should mark as staged modification"
        );
    }

    #[test]
    fn test_pure_rename_no_content_change() {
        // Pure rename: same content everywhere, just different path
        let content = "line 1\nline 2\nline 3";

        let diff = compute_four_way_diff(DiffInput {
            path: "renamed.txt",
            base: Some(content),
            head: Some(content),
            index: Some(content),
            working: Some(content),
            old_path: Some("original.txt"),
        });

        // Should have header and unchanged lines only
        let modified_lines: Vec<_> = diff
            .lines
            .iter()
            .filter(|l| l.old_content.is_some() || l.change_source.is_some())
            .collect();

        assert!(
            modified_lines.is_empty(),
            "Pure rename should have no modifications, got {:?}",
            modified_lines
        );

        // Verify the header shows the rename
        assert_eq!(diff.lines[0].source, LineSource::FileHeader);
        assert_eq!(diff.lines[0].content, "original.txt → renamed.txt");
    }

    #[test]
    fn test_canceled_committed_line() {
        let base = "line1\nline2";
        let head = "line1\nline2\ncommitted_line";
        let working = "line1\nline2";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(head), Some(working));

        let canceled_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::CanceledCommitted)
            .collect();

        assert_eq!(canceled_lines.len(), 1);
        assert_eq!(canceled_lines[0].content, "committed_line");
        assert_eq!(canceled_lines[0].prefix, '±');
    }

    #[test]
    fn test_canceled_staged_line() {
        let base = "line1\nline2";
        let index = "line1\nline2\nstaged_line";
        let working = "line1\nline2";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(index), Some(working));

        let canceled_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::CanceledStaged)
            .collect();

        assert_eq!(canceled_lines.len(), 1);
        assert_eq!(canceled_lines[0].content, "staged_line");
        assert_eq!(canceled_lines[0].prefix, '±');
    }

    #[test]
    fn test_committed_then_modified_not_canceled() {
        let base = "line1\nline2";
        let head = "line1\nline2\nversion1";
        let working = "line1\nline2\nversion2";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(head), Some(working));

        let canceled_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::CanceledCommitted)
            .collect();

        assert_eq!(canceled_lines.len(), 0, "modified line should not be canceled");
    }

    #[test]
    fn test_staged_then_modified_not_canceled() {
        let base = "line1\nline2";
        let index = "line1\nline2\nversion1";
        let working = "line1\nline2\nversion2";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(index), Some(working));

        let canceled_lines: Vec<_> = diff.lines.iter()
            .filter(|l| l.source == LineSource::CanceledStaged)
            .collect();

        assert_eq!(canceled_lines.len(), 0, "modified line should not be canceled");
    }

    #[test]
    fn test_modified_line_shows_merged_with_inline_spans() {
        let base = "line1\nold content\nline3";
        let working = "line1\nnew content\nline3";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();

        assert_eq!(with_spans.len(), 1);
        assert_eq!(with_spans[0].content, "new content");
        assert!(with_spans[0].prefix == ' ');
    }

    #[test]
    fn test_modified_line_position_preserved() {
        let base = "before\nprocess_data(input)\nafter";
        let working = "before\nprocess_data(input, options)\nafter";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let contents: Vec<_> = lines.iter().map(|l| l.content.as_str()).collect();
        assert_eq!(contents, vec!["before", "process_data(input, options)", "after"]);

        let modified = lines.iter().find(|l| l.content == "process_data(input, options)").unwrap();
        assert!(!modified.inline_spans.is_empty());
    }

    #[test]
    fn test_multiple_modifications_show_merged() {
        let base = "line1\nprocess_item(data1)\nline3\nprocess_item(data2)\nline5";
        let working = "line1\nprocess_item(data1, options)\nline3\nprocess_item(data2, options)\nline5";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();

        assert_eq!(with_spans.len(), 2);
        assert_eq!(with_spans[0].content, "process_item(data1, options)");
        assert_eq!(with_spans[1].content, "process_item(data2, options)");
    }

    #[test]
    fn test_committed_modification_shows_merged() {
        let base = "line1\nfunction getData()\nline3";
        let head = "line1\nfunction getData(params)\nline3";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(with_spans.len(), 1);
        assert_eq!(with_spans[0].content, "function getData(params)");

        let changed: Vec<_> = with_spans[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Committed))
            .collect();
        assert!(!changed.is_empty());
    }

    #[test]
    fn test_committed_modification_with_additions_before() {
        // Simulates: new lines added at top, AND existing lines modified
        // This is the "workon" bug scenario - realistic file structure
        let base = r#"layout {
    pane size=1 borderless=true {
        plugin location="tab-bar"
    }
    pane split_direction="vertical" {
        pane split_direction="horizontal" size="50%" {
            pane size="70%" {
                command "claude"
            }
            pane size="30%" {
            }
        }
        pane size="50%" {
            command "branchdiff"
        }
    }
    pane size=1 borderless=true {
        plugin location="status-bar"
    }
}"#;

        let head = r#"keybinds {
    unbind "Alt f"
}

layout {
    pane size=1 borderless=true {
        plugin location="tab-bar"
    }
    pane split_direction="vertical" {
        pane split_direction="horizontal" size="50%" {
            pane size="80%" {
                command "claude"
            }
            pane size="20%" {
            }
        }
        pane size="50%" {
            command "branchdiff"
        }
    }
    pane size=1 borderless=true {
        plugin location="status-bar"
    }
}"#;

        let diff = compute_diff_with_inline("workon.kdl", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        // The "keybinds" lines should be Committed additions
        let additions: Vec<_> = lines.iter()
            .filter(|l| l.source == LineSource::Committed && l.prefix == '+')
            .collect();
        assert!(additions.iter().any(|l| l.content.contains("keybinds")),
            "Should have committed addition for 'keybinds', got additions: {:?}",
            additions.iter().map(|l| &l.content).collect::<Vec<_>>());

        // The modified line (70% -> 80%) should show as modified with inline spans
        // It should NOT be shown as Base
        let modified_line = lines.iter()
            .find(|l| l.content.contains("80%"));
        assert!(modified_line.is_some(), "Should have a line containing '80%'");

        let modified = modified_line.unwrap();
        // Modified lines show as Base (gray context) with inline highlighting
        assert_eq!(modified.source, LineSource::Base,
            "Modified line '{}' should be Base (with inline highlighting), not {:?}",
            modified.content, modified.source);
        // Must have old_content set for inline diff computation
        assert!(modified.old_content.is_some(),
            "Modified line should have old_content set");
        assert!(!modified.inline_spans.is_empty(),
            "Modified line '{}' should have inline spans showing the change from 70% to 80%",
            modified.content);

        // Also check the 30% -> 20% modification
        let modified_line_2 = lines.iter()
            .find(|l| l.content.contains("20%"));
        assert!(modified_line_2.is_some(), "Should have a line containing '20%'");

        let modified2 = modified_line_2.unwrap();
        assert_eq!(modified2.source, LineSource::Base,
            "Modified line '{}' should be Base (with inline highlighting), not {:?}",
            modified2.content, modified2.source);
        assert!(modified2.old_content.is_some(),
            "Modified line should have old_content set");
        assert!(!modified2.inline_spans.is_empty(),
            "Modified line '{}' should have inline spans showing the change from 30% to 20%",
            modified2.content);
    }

    #[test]
    fn test_staged_modification_shows_merged() {
        let base = "line1\nfunction getData()\nline3";
        let index = "line1\nfunction getData(params)\nline3";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(index), Some(index));
        let lines = content_lines(&diff);

        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(with_spans.len(), 1);
        assert_eq!(with_spans[0].content, "function getData(params)");

        let changed: Vec<_> = with_spans[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Staged))
            .collect();
        assert!(!changed.is_empty());
    }

    #[test]
    fn test_context_lines_preserved() {
        let base = "line1\nline2\nline3\nline4\nline5";
        let working = "line1\nline2\nmodified\nline4\nline5";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let pure_context: Vec<_> = lines.iter()
            .filter(|l| l.source == LineSource::Base && l.inline_spans.is_empty())
            .collect();

        assert_eq!(pure_context.len(), 4);
        assert!(pure_context.iter().all(|l| l.prefix == ' '));
    }

    #[test]
    fn test_line_numbers_correct_after_deletion() {
        let base = "line1\nto_delete\nline3";
        let working = "line1\nline3";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        assert!(deleted.iter().all(|l| l.line_number.is_none()));

        let with_numbers: Vec<_> = lines.iter()
            .filter(|l| l.line_number.is_some())
            .collect();

        assert_eq!(with_numbers.len(), 2);
        assert_eq!(with_numbers[0].line_number, Some(1));
        assert_eq!(with_numbers[1].line_number, Some(2));
    }

    #[test]
    fn test_line_numbers_correct_after_addition() {
        let base = "line1\nline2";
        let working = "line1\nnew_line\nline2";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let with_numbers: Vec<_> = lines.iter()
            .filter(|l| l.line_number.is_some())
            .map(|l| (l.content.as_str(), l.line_number.unwrap()))
            .collect();

        assert_eq!(with_numbers, vec![
            ("line1", 1),
            ("new_line", 2),
            ("line2", 3),
        ]);
    }

    #[test]
    fn test_modify_committed_line_in_working_tree() {
        let base = "line1\n";
        let head = "line1\ncommitted line\n";
        let working = "line1\ncommitted line # with comment\n";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(head), Some(working));
        let lines = content_lines(&diff);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].content, "committed line # with comment");

        let unchanged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() && !s.is_deletion)
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        assert!(!unchanged_spans.is_empty());
        assert!(!unstaged_spans.is_empty());
    }

    #[test]
    fn test_modify_staged_line_in_working_tree() {
        let base = "line1\n";
        let head = "line1\n";
        let index = "line1\nstaged line\n";
        let working = "line1\nstaged line modified\n";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].content, "staged line modified");

        let unchanged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() && !s.is_deletion)
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        assert!(!unchanged_spans.is_empty());
        assert!(!unstaged_spans.is_empty());
    }

    #[test]
    fn test_modify_base_line_in_commit() {
        let base = "do_thing(data)\n";
        let head = "do_thing(data, params)\n";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(with_spans.len(), 1);
        assert_eq!(with_spans[0].content, "do_thing(data, params)");

        let changed: Vec<_> = with_spans[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Committed))
            .collect();
        assert!(!changed.is_empty());
    }

    #[test]
    fn test_chain_of_modifications() {
        let base = "original\n";
        let head = "committed version\n";
        let index = "staged version\n";
        let working = "working version\n";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let with_spans: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(with_spans.len(), 1);
        assert_eq!(with_spans[0].content, "working version");

        let changed: Vec<_> = with_spans[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();
        assert!(!changed.is_empty());
    }

    #[test]
    fn test_committed_line_unchanged_through_stages() {
        let base = "line1\n";
        let head = "line1\ncommitted line\n";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        let added: Vec<_> = lines.iter().filter(|l| l.prefix == '+').collect();

        assert_eq!(added.len(), 1);
        assert_eq!(added[0].content, "committed line");
        assert_eq!(added[0].source, LineSource::Committed);
    }

    #[test]
    fn test_staged_line_unchanged_in_working() {
        let base = "line1\n";
        let head = "line1\n";
        let index = "line1\nstaged line\n";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(index), Some(index));
        let lines = content_lines(&diff);

        let added: Vec<_> = lines.iter().filter(|l| l.prefix == '+').collect();

        assert_eq!(added.len(), 1);
        assert_eq!(added[0].content, "staged line");
        assert_eq!(added[0].source, LineSource::Staged);
    }

    #[test]
    fn test_inline_diff_not_meaningful_falls_back_to_pair() {
        let base = "abcdefgh\n";
        let working = "xyz12345\n";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        let added: Vec<_> = lines.iter().filter(|l| l.prefix == '+').collect();

        assert_eq!(deleted.len(), 1);
        assert_eq!(added.len(), 1);
        assert_eq!(deleted[0].content, "abcdefgh");
        assert_eq!(added[0].content, "xyz12345");
    }

    #[test]
    fn test_block_of_changes_no_inline_merge() {
        let base = "context\nalpha: aaa,\nbeta: bbb,\ngamma: ccc,\nend";
        let working = "context\nxray: xxx,\nyankee: yyy,\nzulu: zzz,\nend";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        let added: Vec<_> = lines.iter().filter(|l| l.prefix == '+').collect();
        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();

        assert_eq!(deleted.len(), 3);
        assert_eq!(added.len(), 3);
        assert_eq!(merged.len(), 0);

        for line in &added {
            assert!(line.inline_spans.is_empty());
        }
    }

    #[test]
    fn test_single_line_modification_with_context_shows_inline() {
        let base = "before\ndescribed_class.new(bond).execute\nafter";
        let working = "before\ndescribed_class.new(bond).execute # and add some color commentary\nafter";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        assert_eq!(deleted.len(), 0);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);

        assert!(merged[0].content.contains("# and add some color commentary"));

        let unchanged: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none())
            .collect();
        let changed: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_some())
            .collect();

        assert!(!unchanged.is_empty());
        assert!(!changed.is_empty());

        let unchanged_text: String = unchanged.iter().map(|s| s.text.as_str()).collect();
        assert!(unchanged_text.contains("described_class.new(bond).execute"));
    }

    #[test]
    fn test_single_line_committed_modification_shows_inline() {
        let base = "before\ndescribed_class.new(bond).execute\nafter";
        let head = "before\ndescribed_class.new(bond).execute # and add some color commentary\nafter";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        assert_eq!(deleted.len(), 0);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);

        assert!(merged[0].content.contains("# and add some color commentary"));

        let unchanged: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none())
            .collect();
        let changed: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_some())
            .collect();

        assert!(!unchanged.is_empty());
        assert!(!changed.is_empty());

        let committed_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Committed))
            .collect();
        assert!(!committed_spans.is_empty());

        let unchanged_text: String = unchanged.iter().map(|s| s.text.as_str()).collect();
        assert!(unchanged_text.contains("described_class.new(bond).execute"));
    }

    #[test]
    fn test_modification_with_adjacent_empty_line_inserts_shows_inline() {
        let base = "before\ndescribed_class.new(bond).execute\nafter";
        let head = "before\n\ndescribed_class.new(bond).execute # comment\n\nafter";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);

        assert!(merged[0].content.contains("# comment"));

        let unchanged: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none())
            .collect();
        let changed: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_some())
            .collect();

        assert!(!unchanged.is_empty());
        assert!(!changed.is_empty());
    }

    #[test]
    fn test_unstaged_modification_of_committed_line_shows_inline() {
        let base = "before\nafter";
        let head = "before\ndescribed_class.new(bond).execute\nafter";
        let index = "before\ndescribed_class.new(bond).execute\nafter";
        let working = "before\ndescribed_class.new(bond).execute # and add some color commentary\nafter";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);

        assert_eq!(merged[0].content, "described_class.new(bond).execute # and add some color commentary");

        let unchanged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() && !s.is_deletion)
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        assert!(!unchanged_spans.is_empty());
        let unchanged_text: String = unchanged_spans.iter().map(|s| s.text.as_str()).collect();
        assert!(unchanged_text.contains("described_class.new(bond).execute"));

        assert!(!unstaged_spans.is_empty());
        let unstaged_text: String = unstaged_spans.iter().map(|s| s.text.as_str()).collect();
        assert!(unstaged_text.contains("# and add some color commentary"));
    }

    #[test]
    fn test_unstaged_modification_of_base_line_shows_gray_and_yellow() {
        let base = "before\noriginal_code()\nafter";
        let head = "before\noriginal_code()\nafter";
        let index = "before\noriginal_code()\nafter";
        let working = "before\noriginal_code() # added comment\nafter";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);

        let base_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() || s.source == Some(LineSource::Base))
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        let committed_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Committed))
            .collect();
        assert!(committed_spans.is_empty(), "line from master shouldn't be Committed");

        assert!(!base_spans.is_empty());
        assert!(!unstaged_spans.is_empty());

        let unstaged_text: String = unstaged_spans.iter().map(|s| s.text.as_str()).collect();
        assert!(unstaged_text.contains("# added comment"));
    }

    #[test]
    fn test_duplicate_lines_correct_source_attribution() {
        let base = "context 'first' do\n  it 'test' do\n  end\nend\n";
        let head = "context 'first' do\n  it 'test' do\n  end\n  it 'new test' do\n  end\nend\n";
        let index = head;
        let working = "context 'first' do\n  it 'test' do\n  end\n  it 'new test' do\n  end # added comment\nend\n";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);

        let unchanged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() && !s.is_deletion)
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        assert!(!unchanged_spans.is_empty());
        assert!(!unstaged_spans.is_empty());
    }

    #[test]
    fn test_duplicate_lines_earlier_base_line_doesnt_affect_committed_line() {
        let base = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end
";
        let head = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end

context 'new' do
  it 'new test' do
    described_class.new(bond).execute
  end
end
";
        let index = head;
        // Modify the SECOND "described_class.new(bond).execute" (the one added in commit)
        let working = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end

context 'new' do
  it 'new test' do
    described_class.new(bond).execute # added comment
  end
end
";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let merged: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();
        assert_eq!(merged.len(), 1);

        assert!(merged[0].content.contains("described_class.new(bond).execute # added comment"));

        let unchanged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source.is_none() && !s.is_deletion)
            .collect();
        let unstaged_spans: Vec<_> = merged[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();

        assert!(!unchanged_spans.is_empty());

        assert!(!unstaged_spans.is_empty());
        let unstaged_text: String = unstaged_spans.iter().map(|s| s.text.as_str()).collect();
        assert!(unstaged_text.contains("# added comment"));
    }

    #[test]
    fn test_last_test_in_committed_block_shows_committed_not_base() {
        let base = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end
";
        let head = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end

context 'first new' do
  it 'first new test' do
    described_class.new(bond).execute
  end
end

context 'second new' do
  it 'second new test' do
    described_class.new(bond).execute
  end
end

context 'third new' do
  it 'third new test' do
    described_class.new(bond).execute
  end
end
";
        let index = head;
        let working = head;

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let execute_lines: Vec<_> = lines.iter()
            .filter(|l| l.content == "    described_class.new(bond).execute")
            .collect();

        assert_eq!(execute_lines.len(), 4);

        assert_eq!(execute_lines[0].source, LineSource::Base);

        for line in execute_lines.iter().skip(1) {
            assert_eq!(line.source, LineSource::Committed);
        }
    }

    #[test]
    fn test_committed_block_with_shared_end_line() {
        let base = "context 'existing' do
  it 'test' do
    described_class.new(bond).execute
  end
end
";
        let head = "context 'existing' do
  it 'test' do
    described_class.new(bond).execute
  end
end

context 'new' do
  it 'uses bond data' do
    expected_address = bond.principal_mailing_address

    described_class.new(bond).execute

    notice = Commercial::PremiumDueNotice.new(bond)
    expect(notice.pdf_fields_hash[:address]).to eq(expected_address)
  end
end
";
        let index = head;
        let working = head;

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let new_test_lines: Vec<_> = lines.iter()
            .enumerate()
            .filter(|(_, l)| {
                l.content.contains("uses bond data") ||
                l.content.contains("expected_address") ||
                l.content.contains("notice = Commercial") ||
                l.content.contains("expect(notice")
            })
            .collect();

        for (_, line) in &new_test_lines {
            assert_eq!(line.source, LineSource::Committed);
        }

        let execute_lines: Vec<_> = lines.iter()
            .enumerate()
            .filter(|(_, l)| l.content.trim() == "described_class.new(bond).execute")
            .collect();

        assert_eq!(execute_lines.len(), 2);
        assert_eq!(execute_lines[0].1.source, LineSource::Base);
        assert_eq!(execute_lines[1].1.source, LineSource::Committed);
    }

    #[test]
    fn test_blank_line_in_committed_block_shows_committed() {
        let base = "context 'existing' do
  it 'test' do
    existing_code

    described_class.new(bond).execute
  end
end
";
        let head = "context 'existing' do
  it 'test' do
    existing_code

    described_class.new(bond).execute
  end
end

context 'new' do
  it 'new test' do
    expected_address = bond.principal_mailing_address

    described_class.new(bond).execute

    notice = Commercial::PremiumDueNotice.new(bond)
  end
end
";
        let index = head;
        let working = head;

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let new_context_idx = lines.iter().position(|l| l.content.contains("context 'new'"));
        assert!(new_context_idx.is_some());

        let new_context_idx = new_context_idx.unwrap();

        for (i, line) in lines.iter().enumerate().skip(new_context_idx) {
            if line.content.trim().is_empty() {
                assert_eq!(line.source, LineSource::Committed,
                    "Blank line at {} should be Committed", i);
            }
        }
    }

    #[test]
    fn test_third_test_in_block_of_three_shows_committed() {
        let base = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end
";
        let head = "context 'existing' do
  it 'existing test' do
    described_class.new(bond).execute
  end
end

context 'first new' do
  it 'first test' do
    expected = bond.first_attribute

    described_class.new(bond).execute

    expect(result).to eq(expected)
  end
end

context 'second new' do
  it 'second test' do
    expected = bond.second_attribute

    described_class.new(bond).execute

    expect(result).to eq(expected)
  end
end

context 'third new' do
  it 'third test' do
    expected = bond.third_attribute

    described_class.new(bond).execute

    expect(result).to eq(expected)
  end
end
";
        let index = head;
        let working = head;

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let third_context_idx = lines.iter().position(|l| l.content.contains("context 'third new'"));
        assert!(third_context_idx.is_some());
        let third_context_idx = third_context_idx.unwrap();

        for (_, line) in lines.iter().enumerate().skip(third_context_idx) {
            if line.content.contains("third test") ||
               line.content.contains("third_attribute") ||
               line.content.contains("described_class") ||
               line.content.contains("expect(result)") {
                assert_eq!(line.source, LineSource::Committed);
            }
        }

        let execute_lines: Vec<_> = lines.iter().enumerate()
            .filter(|(_, l)| l.content.trim() == "described_class.new(bond).execute")
            .collect();

        assert_eq!(execute_lines.len(), 4);
        assert_eq!(execute_lines[0].1.source, LineSource::Base);
        assert_eq!(execute_lines[1].1.source, LineSource::Committed);
        assert_eq!(execute_lines[2].1.source, LineSource::Committed);
        assert_eq!(execute_lines[3].1.source, LineSource::Committed);
    }

    #[test]
    fn test_modified_line_shows_as_single_merged_line() {
        let base = "do_thing(data)\n";
        let working = "do_thing(data, parameters)\n";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        let modified: Vec<_> = lines.iter().filter(|l| !l.inline_spans.is_empty()).collect();

        assert_eq!(deleted.len(), 0);
        assert_eq!(modified.len(), 1);
        assert_eq!(modified[0].content, "do_thing(data, parameters)");

        let changed: Vec<_> = modified[0].inline_spans.iter()
            .filter(|s| s.source == Some(LineSource::Unstaged))
            .collect();
        assert!(!changed.is_empty());

        let changed_text: String = changed.iter().map(|s| s.text.as_str()).collect();
        assert!(changed_text.contains(", parameters"));
    }

    #[test]
    fn test_new_line_addition_no_inline_spans() {
        let base = "line1\n";
        let working = "line1\nnew line\n";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let added: Vec<_> = lines.iter().filter(|l| l.prefix == '+').collect();
        assert_eq!(added.len(), 1);
        assert!(added[0].inline_spans.is_empty());
    }

    #[test]
    fn test_pure_deletion_still_shows_minus() {
        let base = "line1\nto_delete\nline3\n";
        let working = "line1\nline3\n";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let deleted: Vec<_> = lines.iter().filter(|l| l.prefix == '-').collect();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].content, "to_delete");
    }

    #[test]
    fn test_two_adjacent_committed_modifications() {
        let base = r#"            principal_zip: "00000",
            effective_date: "2022-08-30",
            expiration_date: "2024-08-30",
"#;
        let head = r#"            principal_zip: "00000",
            effective_date: "2023-08-30",
            expiration_date: "2025-08-30",
"#;

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        let effective_lines: Vec<_> = lines.iter()
            .filter(|l| l.content.contains("effective_date"))
            .collect();
        let expiration_lines: Vec<_> = lines.iter()
            .filter(|l| l.content.contains("expiration_date"))
            .collect();

        assert_eq!(effective_lines.len(), 1);
        assert!(effective_lines[0].content.contains("2023"));
        assert!(!effective_lines[0].inline_spans.is_empty());

        assert_eq!(expiration_lines.len(), 1);
        assert!(expiration_lines[0].content.contains("2025"));
        assert!(!expiration_lines[0].inline_spans.is_empty());

        let effective_has_committed = effective_lines[0].inline_spans.iter()
            .any(|s| s.source == Some(LineSource::Committed));
        let expiration_has_committed = expiration_lines[0].inline_spans.iter()
            .any(|s| s.source == Some(LineSource::Committed));

        assert!(effective_has_committed);
        assert!(expiration_has_committed);
    }

    #[test]
    fn test_deletion_positioned_correctly_with_insertions_before() {
        let base = "line1\nline2\nline3\nto_delete\nline5";
        let working = "line1\nline2\nNEW_LINE\nline3\nline5";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let line_contents: Vec<&str> = lines.iter().map(|l| l.content.as_str()).collect();

        let new_line_pos = line_contents.iter().position(|&c| c == "NEW_LINE").unwrap();
        let to_delete_pos = line_contents.iter().position(|&c| c == "to_delete").unwrap();
        let line3_pos = line_contents.iter().position(|&c| c == "line3").unwrap();

        assert!(to_delete_pos > line3_pos);
        assert!(to_delete_pos > new_line_pos);

        let deleted_line = &lines[to_delete_pos];
        assert_eq!(deleted_line.prefix, '-');
    }

    #[test]
    fn test_deletion_before_insertion_at_same_position() {
        let base = "def principal_mailing_address\n  commercial_renewal.principal_mailing_address\nend";
        let working = "def principal_mailing_address\n  \"new content\"\nend";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let line_contents: Vec<&str> = lines.iter().map(|l| l.content.as_str()).collect();
        let prefixes: Vec<char> = lines.iter().map(|l| l.prefix).collect();

        let deleted_pos = line_contents.iter().position(|&c| c.contains("commercial_renewal")).unwrap();
        let inserted_pos = line_contents.iter().position(|&c| c.contains("new content")).unwrap();

        assert!(deleted_pos < inserted_pos);
        assert_eq!(prefixes[deleted_pos], '-');
        assert_eq!(prefixes[inserted_pos], '+');
    }

    #[test]
    fn test_deletion_appears_after_preceding_context_line() {
        let base = "def foo\n  body_line\nend";
        let working = "def foo\n  \"new body\"\nend";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let line_contents: Vec<&str> = lines.iter().map(|l| l.content.as_str()).collect();

        let def_pos = line_contents.iter().position(|&c| c.contains("def foo")).unwrap();
        let deleted_pos = line_contents.iter().position(|&c| c.contains("body_line")).unwrap();
        let inserted_pos = line_contents.iter().position(|&c| c.contains("new body")).unwrap();
        let end_pos = line_contents.iter().position(|&c| c == "end").unwrap();

        assert!(deleted_pos > def_pos);
        assert!(deleted_pos < inserted_pos);
        assert!(deleted_pos < end_pos);

        assert_eq!(def_pos, 0);
        assert_eq!(deleted_pos, 1);
        assert_eq!(inserted_pos, 2);
        assert_eq!(end_pos, 3);
    }

    #[test]
    fn test_deletion_after_modified_line() {
        let base = "def principal_mailing_address\n  commercial_renewal.principal_mailing_address\nend";
        let working = "def pribond_descripal_mailtiong_address\n  \"new content\"\nend";

        let diff = compute_diff_with_inline("test.txt", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        let line_contents: Vec<&str> = lines.iter().map(|l| l.content.as_str()).collect();
        let prefixes: Vec<char> = lines.iter().map(|l| l.prefix).collect();

        let deleted_principal_pos = line_contents.iter().position(|&c| c.contains("principal_mailing_address")).unwrap();
        let inserted_pribond_pos = line_contents.iter().position(|&c| c.contains("pribond")).unwrap();
        let deleted_commercial_pos = line_contents.iter().position(|&c| c.contains("commercial_renewal")).unwrap();
        let inserted_new_content_pos = line_contents.iter().position(|&c| c.contains("new content")).unwrap();
        let end_pos = line_contents.iter().position(|&c| c == "end").unwrap();

        assert_eq!(prefixes[deleted_principal_pos], '-', "principal should be deleted");
        assert_eq!(prefixes[inserted_pribond_pos], '+', "pribond should be inserted");
        assert_eq!(prefixes[deleted_commercial_pos], '-', "commercial_renewal should be deleted");
        assert_eq!(prefixes[inserted_new_content_pos], '+', "new content should be inserted");
        assert_eq!(prefixes[end_pos], ' ', "end should be unchanged context");

        assert!(deleted_principal_pos < deleted_commercial_pos, "both deletions should come together");
        assert!(deleted_commercial_pos < inserted_pribond_pos, "deletions should come before insertions");
        assert!(inserted_pribond_pos < inserted_new_content_pos, "both insertions should come together");
    }

    #[test]
    fn test_unstaged_modification_of_newly_committed_method_appears_in_correct_position() {
        let base = "def abeyance_required?
  abeyance? || active?
end

def from_domino?
  legacy_unid.present?
end
";
        let head = "def abeyance_required?
  abeyance? || active?
end

def can_request_letter_of_bondability?
  !commercial? && (active? || abeyance?)
end

def from_domino?
  legacy_unid.present?
end
";
        let index = head;
        let working = "def abeyance_required?
  abeyance? || active?
end

def can_request_letter_of_bondability?
  !inactive? && status != \"Destroy\"
end

def from_domino?
  legacy_unid.present?
end
";

        let diff = compute_diff_with_inline("principal.rb", Some(base), Some(head), Some(index), Some(working));
        let lines = content_lines(&diff);

        let abeyance_pos = lines.iter().position(|l| l.content.contains("abeyance_required")).unwrap();
        let can_request_pos = lines.iter().position(|l| l.content.contains("can_request_letter_of_bondability")).unwrap();
        let from_domino_pos = lines.iter().position(|l| l.content.contains("from_domino")).unwrap();

        let commercial_line_pos = lines.iter().position(|l| l.content.contains("!commercial?"));
        let inactive_line_pos = lines.iter().position(|l| l.content.contains("!inactive?"));

        assert!(can_request_pos > abeyance_pos, "can_request method should come after abeyance_required");
        assert!(can_request_pos < from_domino_pos, "can_request method should come before from_domino");

        if let Some(commercial_pos) = commercial_line_pos {
            assert!(
                commercial_pos > abeyance_pos && commercial_pos < from_domino_pos,
                "!commercial? line should appear between abeyance_required and from_domino, not at pos {} (abeyance={}, from_domino={})",
                commercial_pos, abeyance_pos, from_domino_pos
            );
        }

        if let Some(inactive_pos) = inactive_line_pos {
            assert!(
                inactive_pos > abeyance_pos && inactive_pos < from_domino_pos,
                "!inactive? line should appear between abeyance_required and from_domino, not at pos {} (abeyance={}, from_domino={})",
                inactive_pos, abeyance_pos, from_domino_pos
            );
        }
    }

    #[test]
    fn test_trailing_context_after_addition() {
        let base = "def foo\nend\nend";
        let working = "def foo\nnew_line\nend\nend";

        let diff = compute_diff_with_inline("test.rb", Some(base), Some(base), Some(base), Some(working));
        let lines = content_lines(&diff);

        assert_eq!(lines.len(), 4);

        assert_eq!(lines[0].content, "def foo");
        assert_eq!(lines[0].source, LineSource::Base);

        assert_eq!(lines[1].content, "new_line");
        assert_eq!(lines[1].prefix, '+');

        assert_eq!(lines[2].content, "end");
        assert_eq!(lines[2].source, LineSource::Base);

        assert_eq!(lines[3].content, "end");
        assert_eq!(lines[3].source, LineSource::Base);
    }

    #[test]
    fn test_trailing_context_after_committed_addition() {
        let base = "def foo\nend\nend";
        let head = "def foo\nnew_line\nend\nend";

        let diff = compute_diff_with_inline("test.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);
        assert_eq!(lines.len(), 4);

        assert_eq!(lines[0].content, "def foo");
        assert_eq!(lines[0].source, LineSource::Base);

        assert_eq!(lines[1].content, "new_line");
        assert_eq!(lines[1].source, LineSource::Committed);
        assert_eq!(lines[1].prefix, '+');

        assert_eq!(lines[2].content, "end");
        assert_eq!(lines[2].source, LineSource::Base);

        assert_eq!(lines[3].content, "end");
        assert_eq!(lines[3].source, LineSource::Base);
    }

    #[test]
    fn test_addition_at_end_of_file_with_trailing_context() {
        let base = "class Foo\n  def bar\n  end\nend";
        let head = "class Foo\n  def bar\n    new_line\n  end\nend";

        let diff = compute_diff_with_inline("test.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        assert_eq!(lines.len(), 5);

        assert_eq!(lines[0].content, "class Foo");
        assert_eq!(lines[1].content, "  def bar");
        assert_eq!(lines[2].content, "    new_line");
        assert_eq!(lines[2].source, LineSource::Committed);
        assert_eq!(lines[3].content, "  end");
        assert_eq!(lines[3].source, LineSource::Base);
        assert_eq!(lines[4].content, "end");
        assert_eq!(lines[4].source, LineSource::Base);
    }

    #[test]
    fn test_addition_before_two_trailing_ends() {
        let base = "do\n  body\nend\nend";
        let head = "do\n  body\n  new_end\nend\nend";

        let diff = compute_diff_with_inline("test.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        assert_eq!(lines.len(), 5);

        assert_eq!(lines[0].content, "do");
        assert_eq!(lines[0].source, LineSource::Base);

        assert_eq!(lines[1].content, "  body");
        assert_eq!(lines[1].source, LineSource::Base);

        assert_eq!(lines[2].content, "  new_end");
        assert_eq!(lines[2].source, LineSource::Committed);
        assert_eq!(lines[2].prefix, '+');

        assert_eq!(lines[3].content, "end");
        assert_eq!(lines[3].source, LineSource::Base);

        assert_eq!(lines[4].content, "end");
        assert_eq!(lines[4].source, LineSource::Base);
    }

    #[test]
    fn test_final_file_ends_with_addition() {
        let base = "do\n  body\nend";
        let head = "do\n  body\nend\n  extra";

        let diff = compute_diff_with_inline("test.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        assert_eq!(lines.len(), 4);

        assert_eq!(lines[0].content, "do");
        assert_eq!(lines[0].source, LineSource::Base);

        assert_eq!(lines[1].content, "  body");
        assert_eq!(lines[1].source, LineSource::Base);

        assert_eq!(lines[2].content, "end");
        assert_eq!(lines[2].source, LineSource::Base);

        assert_eq!(lines[3].content, "  extra");
        assert_eq!(lines[3].source, LineSource::Committed);
        assert_eq!(lines[3].prefix, '+');
    }

    #[test]
    fn test_file_without_trailing_newline() {
        let base = "line1\nline2\nend\nend\nend";
        let head = "line1\nline2\nnew_line\nend\nend\nend";

        let diff = compute_diff_with_inline("test.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0].content, "line1");
        assert_eq!(lines[1].content, "line2");
        assert_eq!(lines[2].content, "new_line");
        assert_eq!(lines[2].source, LineSource::Committed);
        assert_eq!(lines[3].content, "end");
        assert_eq!(lines[3].source, LineSource::Base);
        assert_eq!(lines[4].content, "end");
        assert_eq!(lines[4].source, LineSource::Base);
        assert_eq!(lines[5].content, "end");
        assert_eq!(lines[5].source, LineSource::Base);
    }

    #[test]
    fn test_exact_scenario_from_bug_report() {
        // EXACT scenario from the bug report:
        // Base file ends with:
        //   ...some tests...
        //   end  <- end of describe block
        //   end  <- end of RSpec.describe
        //
        // Head adds a new test BEFORE the final two ends:
        //   ...some tests...
        //   (empty line)
        //   it "new test" do
        //     expect(...)
        //   end   <- end of new test (ADDED)
        //   end   <- end of describe block (BASE)
        //   end   <- end of RSpec.describe (BASE)

        let base = r##"  describe "#method" do
    it "test 1" do
      expect(true).to be(true)
    end
  end
end"##;

        let head = r##"  describe "#method" do
    it "test 1" do
      expect(true).to be(true)
    end

    it "new test" do
      expect(false).to be(false)
    end
  end
end"##;

        let diff = compute_diff_with_inline("spec.rb", Some(base), Some(head), Some(head), Some(head));
        let lines = content_lines(&diff);

        // Expected:
        // [0] describe "#method" do       (base)
        // [1]   it "test 1" do            (base)
        // [2]     expect(true)...         (base)
        // [3]   end                       (base)
        // [4] (empty line)                (committed +)
        // [5]   it "new test" do          (committed +)
        // [6]     expect(false)...        (committed +)
        // [7]   end                       (committed +)
        // [8] end                         (base) <-- trailing
        // [9] end                         (base) <-- trailing

        assert!(lines.len() >= 10, "Should have at least 10 lines, got {}", lines.len());

        // Check the trailing base lines exist
        let last_two: Vec<_> = lines.iter().rev().take(2).collect();
        assert_eq!(last_two[0].content, "end", "Last line should be 'end'");
        assert_eq!(last_two[0].source, LineSource::Base, "Last line should be Base");
        // Second to last is "  end" (with indent)
        assert_eq!(last_two[1].content, "  end", "Second to last should be '  end'");
        assert_eq!(last_two[1].source, LineSource::Base, "Second to last should be Base");

        // Check that the added "end" (end of new test) is Committed
        let added_end = lines.iter().find(|l| l.content == "    end" && l.source == LineSource::Committed);
        assert!(added_end.is_some(), "Should have a committed '    end' line");

        // CRITICAL: All 10 lines should be present
        assert_eq!(lines.len(), 10, "All 10 lines should be in the diff output");
    }

    #[test]
    fn test_staging_changes_line_source_from_unstaged_to_staged() {
        let base = "line1\nline2";
        let modified = "line1\nline2\nline3";

        // Before staging: change is only in working tree
        let diff_before = compute_four_way_diff(DiffInput {
            path: "test.txt",
            base: Some(base),
            head: Some(base),
            index: Some(base),      // index same as base
            working: Some(modified), // working has the change
            old_path: None,
        });

        let unstaged_lines: Vec<_> = diff_before.lines.iter()
            .filter(|l| l.source == LineSource::Unstaged && l.content == "line3")
            .collect();
        assert_eq!(unstaged_lines.len(), 1, "line3 should be Unstaged before staging");

        // After staging: change is in index and working tree
        let diff_after = compute_four_way_diff(DiffInput {
            path: "test.txt",
            base: Some(base),
            head: Some(base),
            index: Some(modified),   // index now has the change
            working: Some(modified), // working same as index
            old_path: None,
        });

        let staged_lines: Vec<_> = diff_after.lines.iter()
            .filter(|l| l.source == LineSource::Staged && l.content == "line3")
            .collect();
        assert_eq!(staged_lines.len(), 1, "line3 should be Staged after staging");

        // Verify line3 is NOT unstaged after staging
        let still_unstaged: Vec<_> = diff_after.lines.iter()
            .filter(|l| l.source == LineSource::Unstaged && l.content == "line3")
            .collect();
        assert_eq!(still_unstaged.len(), 0, "line3 should NOT be Unstaged after staging");
    }

    #[test]
    fn test_unstaged_import_modification_shows_inline() {
        // Reproduces the bug: modifying an import line in working tree
        // should show as modified with inline highlighting, not as gray context
        let base = r#"use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};"#;

        let working = r#"use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};"#;

        let diff = compute_diff_with_inline(
            "test.rs",
            Some(base),
            Some(base),
            Some(base),
            Some(working),
        );
        let lines = content_lines(&diff);

        // Find the modified line
        let modified_line = lines.iter()
            .find(|l| l.content.contains("Clear"));
        assert!(modified_line.is_some(), "Should have a line containing 'Clear'");

        let modified = modified_line.unwrap();

        // Key assertions: the line should have old_content and inline_spans
        assert!(modified.old_content.is_some(),
            "Modified import line should have old_content set, but it was None. \
            Source is {:?}, change_source is {:?}",
            modified.source, modified.change_source);

        assert!(!modified.inline_spans.is_empty(),
            "Modified import line should have inline spans showing 'Clear, ' insertion. \
            Source is {:?}, change_source is {:?}",
            modified.source, modified.change_source);

        // The change should be marked as Unstaged
        assert_eq!(modified.change_source, Some(LineSource::Unstaged),
            "Modification should be marked as Unstaged");
    }

    #[test]
    fn test_unstaged_modification_plus_addition() {
        // More realistic: modification early in file + addition later
        // This matches the actual bug scenario where line 12 (import modification)
        // shows as gray but line 158 (addition) shows as yellow
        let base = r#"line 1
line 2
widgets::{Block, Borders, Paragraph},
line 4
line 5
line 6
line 7
line 8
render_widget(paragraph)"#;

        let working = r#"line 1
line 2
widgets::{Block, Borders, Clear, Paragraph},
line 4
line 5
line 6
line 7
line 8
render_widget(Clear);
render_widget(paragraph)"#;

        let diff = compute_diff_with_inline(
            "test.rs",
            Some(base),
            Some(base),
            Some(base),
            Some(working),
        );
        let lines = content_lines(&diff);

        // Check the modification (line 3)
        let modified_line = lines.iter()
            .find(|l| l.content.contains("Clear, Paragraph"));
        assert!(modified_line.is_some(), "Should have modified line with 'Clear, Paragraph'");

        let modified = modified_line.unwrap();
        assert!(modified.old_content.is_some(),
            "Modified line should have old_content set. Source: {:?}, change_source: {:?}",
            modified.source, modified.change_source);
        assert_eq!(modified.change_source, Some(LineSource::Unstaged),
            "Modification should be marked as Unstaged");

        // Check the addition (new line)
        let added_line = lines.iter()
            .find(|l| l.content == "render_widget(Clear);");
        assert!(added_line.is_some(), "Should have added line 'render_widget(Clear);'");

        let added = added_line.unwrap();
        assert_eq!(added.source, LineSource::Unstaged,
            "Pure addition should have source Unstaged");
        assert_eq!(added.prefix, '+', "Addition should have + prefix");
    }

    #[test]
    fn test_exact_diff_view_scenario() {
        // Exact reproduction of the diff_view.rs scenario
        // Line 12: widgets::{Block, Borders, Paragraph}, -> widgets::{Block, Borders, Clear, Paragraph},
        // Line 158: addition of frame.render_widget(Clear, self.area);
        let base = r#"//! Diff view rendering with pure data model separation.
//!
//! The DiffViewModel provides a pure view model for rendering, enabling
//! easier unit testing without requiring a full App instance.

use std::collections::HashSet;

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::{App, DisplayableItem, FrameContext, Selection};

// ... 140 lines of code ...

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let paragraph = Paragraph::new(all_lines).block(block);
        frame.render_widget(paragraph, self.area);"#;

        let working = r#"//! Diff view rendering with pure data model separation.
//!
//! The DiffViewModel provides a pure view model for rendering, enabling
//! easier unit testing without requiring a full App instance.

use std::collections::HashSet;

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::{App, DisplayableItem, FrameContext, Selection};

// ... 140 lines of code ...

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        frame.render_widget(Clear, self.area);
        let paragraph = Paragraph::new(all_lines).block(block);
        frame.render_widget(paragraph, self.area);"#;

        let diff = compute_diff_with_inline(
            "diff_view.rs",
            Some(base),
            Some(base),
            Some(base),
            Some(working),
        );
        let lines = content_lines(&diff);

        // Debug: print all lines with their sources
        for (i, line) in lines.iter().enumerate() {
            if line.content.contains("Clear") || line.content.contains("widgets") {
                eprintln!("Line {}: source={:?}, change_source={:?}, old_content={}, content='{}'",
                    i, line.source, line.change_source, line.old_content.is_some(), line.content);
            }
        }

        // Check the import modification (line 12 in real file)
        let modified_import = lines.iter()
            .find(|l| l.content.contains("Clear, Paragraph"));
        assert!(modified_import.is_some(), "Should have modified import line");

        let modified = modified_import.unwrap();
        assert!(modified.old_content.is_some(),
            "Modified import should have old_content. Source: {:?}, change_source: {:?}, prefix: '{}'",
            modified.source, modified.change_source, modified.prefix);

        // Check the addition (line 158 in real file)
        let added_line = lines.iter()
            .find(|l| l.content.contains("render_widget(Clear"));
        assert!(added_line.is_some(), "Should have added render_widget line");

        let added = added_line.unwrap();
        assert_eq!(added.prefix, '+', "Addition should have + prefix");
    }
}
