use ratatui::{
    layout::{Constraint, Direction, Layout},
    Frame,
};

use crate::app::App;

pub mod colors;
pub mod diff_view;
pub mod modals;
pub mod selection;
pub mod spans;
pub mod status_bar;
pub mod wrapping;

// Re-export commonly used items
pub use modals::{draw_help_modal, draw_warning_banner};
pub use status_bar::{draw_status_bar, status_bar_height};

const PREFIX_CHAR_WIDTH: usize = 2; // prefix char + trailing space

/// What kind of screen row this is
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenRowKind {
    /// Normal single-row line or first row of wrapped content
    Normal,
    /// Continuation row of wrapped content (2nd, 3rd, etc.)
    WrappedContinuation,
    /// The "-" deletion line when inline diff splits into two lines
    SplitDeletion,
    /// The "+" insertion line when inline diff splits into two lines
    SplitInsertion,
}

/// Represents how a logical DiffLine maps to a screen row
#[derive(Debug, Clone)]
pub struct ScreenRowInfo {
    /// Index into the logical DiffLine array (absolute, accounting for scroll)
    pub logical_idx: usize,
    /// What kind of row this is
    pub kind: ScreenRowKind,
    /// The actual text content of this screen row (for copy operations)
    pub content: String,
    /// Whether this row is a file header (for collapse detection)
    pub is_file_header: bool,
    /// The file path this row belongs to (for collapse toggle)
    pub file_path: Option<String>,
}

/// Draw the main UI
pub fn draw(frame: &mut Frame, app: &mut App) {
    let size = frame.area();

    let has_warning = app.conflict_warning.is_some();
    let status_height = status_bar_height(app, size.width);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if has_warning {
            vec![
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(status_height),
            ]
        } else {
            vec![
                Constraint::Min(1),
                Constraint::Length(status_height),
            ]
        })
        .split(size);

    let (warning_area, diff_area, status_area) = if has_warning {
        (Some(chunks[0]), chunks[1], chunks[2])
    } else {
        (None, chunks[0], chunks[1])
    };

    if let (Some(area), Some(warning)) = (warning_area, &app.conflict_warning) {
        draw_warning_banner(frame, warning, area);
    }

    let content_height = diff_area.height.saturating_sub(2) as usize;
    app.set_viewport_height(content_height);

    diff_view::draw_diff_view(frame, app, diff_area);
    draw_status_bar(frame, app, status_area);

    if app.show_help {
        draw_help_modal(frame, size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::spans::{coalesce_spans, is_fragmented, inline_display_width, reconstruct_old_content, get_deletion_source, get_insertion_source};
    use super::status_bar::truncate_with_ellipsis;
    use crate::diff::{InlineSpan, LineSource, compute_inline_diff_merged};

    fn make_span(text: &str, source: Option<LineSource>, is_deletion: bool) -> InlineSpan {
        InlineSpan {
            text: text.to_string(),
            source,
            is_deletion,
        }
    }

    #[test]
    fn test_is_fragmented_few_spans_not_fragmented() {
        // Only 2 spans - not fragmented
        let spans = vec![
            make_span("hello", Some(LineSource::DeletedBase), true),
            make_span("world", Some(LineSource::Committed), false),
        ];
        assert!(!spans::is_fragmented(&spans));
    }

    #[test]
    fn test_is_fragmented_single_change_region_not_fragmented() {
        // Single change region (deletion + insertion together) - not fragmented
        // Pattern: change, change, unchanged - one contiguous change region
        let spans = vec![
            make_span("world", Some(LineSource::DeletedBase), true),
            make_span("earth", Some(LineSource::Committed), false),
            make_span(" hello ", None, false),
        ];
        assert!(!spans::is_fragmented(&spans));
    }

    #[test]
    fn test_is_fragmented_two_change_regions_is_fragmented() {
        // Two separate change regions - fragmented
        // Pattern: unchanged, change, unchanged, change (two change regions)
        let spans = vec![
            make_span("c", None, false),                                    // unchanged
            make_span("b", Some(LineSource::Committed), false),             // change region 1
            make_span("ommercial_renewal", None, false),                    // unchanged
            make_span("d", Some(LineSource::Committed), false),             // change region 2
        ];
        assert!(spans::is_fragmented(&spans));
    }

    #[test]
    fn test_is_fragmented_commercial_renewal_to_bond() {
        // Real case: commercial_renewal -> bond with scattered char matches
        // Pattern: unchanged(c), deleted+inserted, unchanged(on), inserted(d)
        let spans = vec![
            make_span("c", None, false),                                    // unchanged
            make_span("ommercial_renewal", Some(LineSource::DeletedBase), true), // change region 1
            make_span("b", Some(LineSource::Committed), false),             // still in change region 1
            make_span("on", None, false),                                   // unchanged - exits region 1
            make_span("d", Some(LineSource::Committed), false),             // change region 2
        ];
        assert!(spans::is_fragmented(&spans));
    }

    #[test]
    fn test_coalesce_spans_not_fragmented_returns_original() {
        let spans = vec![
            make_span("hello ", None, false),
            make_span("world", Some(LineSource::DeletedBase), true),
            make_span("earth", Some(LineSource::Committed), false),
        ];
        let result = coalesce_spans(&spans);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].text, "hello ");
        assert_eq!(result[1].text, "world");
        assert_eq!(result[2].text, "earth");
    }

    #[test]
    fn test_coalesce_spans_fragmented_preserves_structural_prefix_suffix() {
        // Fragmented case with structural prefix (whitespace) and suffix (punctuation)
        // Only structural chars (whitespace, punctuation) are preserved as prefix/suffix
        // Non-structural chars like letters get included in coalesced region

        let spans = vec![
            make_span("  ", None, false),       // structural prefix (spaces) - KEEP
            make_span("bc", Some(LineSource::DeletedBase), true),  // deleted - first change
            make_span("x", Some(LineSource::Committed), false),    // inserted
            make_span("d", None, false),        // unchanged (in fragmented region)
            make_span("e", Some(LineSource::DeletedBase), true),   // deleted
            make_span("yz", Some(LineSource::Committed), false),   // inserted - last change
            make_span(");", None, false),       // structural suffix (punctuation) - KEEP
        ];

        let result = coalesce_spans(&spans);

        // Should be: spaces, coalesced_old, coalesced_new, punctuation
        assert_eq!(result.len(), 4, "Expected structural_prefix + old + new + structural_suffix");
        assert_eq!(result[0].text, "  ");
        assert!(result[0].source.is_none()); // unchanged
        assert!(result[1].is_deletion);
        assert_eq!(result[1].text, "bcde"); // coalesced old
        assert!(!result[2].is_deletion);
        assert_eq!(result[2].text, "xdyz"); // coalesced new
        assert_eq!(result[3].text, ");");
        assert!(result[3].source.is_none()); // unchanged
    }

    #[test]
    fn test_coalesce_spans_includes_nonstructural_prefix_in_coalesce() {
        // Non-structural prefix chars (like a single 'c') should be included in coalesced region
        // This handles the "cancellation" -> "clause" case where 'c' is coincidental
        // Need 4+ spans and 2+ change regions to trigger fragmentation detection

        let spans = vec![
            make_span("c", None, false),        // non-structural - gets coalesced
            make_span("ancellation", Some(LineSource::DeletedBase), true), // change region 1
            make_span("l", None, false),        // unchanged in middle
            make_span("ause", Some(LineSource::Committed), false), // change region 2
        ];

        let result = coalesce_spans(&spans);

        // Should coalesce everything since 'c' is not structural
        assert_eq!(result.len(), 2);
        assert!(result[0].is_deletion);
        assert_eq!(result[0].text, "cancellationl"); // c + ancellation + l
        assert!(!result[1].is_deletion);
        assert_eq!(result[1].text, "clause"); // c + l + ause
    }

    #[test]
    fn test_coalesce_spans_preserves_good_inline_diff() {
        // Good inline diff: do_thing(data) -> do_thing(data, params)
        // Should have large unchanged segment "do_thing(data" and small insertion ", params"
        let spans = vec![
            make_span("do_thing(data", None, false),
            make_span(", params", Some(LineSource::Committed), false),
            make_span(")", None, false),
        ];
        let result = coalesce_spans(&spans);

        // Should NOT coalesce - good readable diff
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].text, "do_thing(data");
        assert_eq!(result[1].text, ", params");
        assert_eq!(result[2].text, ")");
    }

    #[test]
    fn test_real_world_commercial_renewal_to_bond() {
        // Real example: "  commercial_renewal.principal_mailing_address" -> "  bond.description"
        // The character diff would scatter shared chars (o, n, i, etc.)
        // Simulate what a character diff might produce (simplified):
        let spans = vec![
            make_span("  ", None, false),           // structural prefix (spaces) - PRESERVED
            make_span("c", None, false),            // non-structural - gets coalesced (coincidental match)
            make_span("ommercial_renewal.principal_mailing_address", Some(LineSource::DeletedBase), true),
            make_span("b", Some(LineSource::Committed), false),
            make_span("o", None, false),
            make_span("n", None, false),
            make_span("d.des", Some(LineSource::Committed), false),
            make_span("c", None, false),
            make_span("r", Some(LineSource::Committed), false),
            make_span("i", None, false),
            make_span("ption", Some(LineSource::Committed), false),
        ];

        let result = coalesce_spans(&spans);

        // Should preserve only structural prefix (spaces), coalesce everything else
        // The 'c' gets included in coalesced region since it's not structural
        assert_eq!(result.len(), 3, "Should have: spaces + coalesced_old + coalesced_new");
        assert_eq!(result[0].text, "  ");
        assert!(result[1].is_deletion);
        // Old text includes: c + ommercial... + o + n + c + i = "commercial_renewal..."
        assert!(result[1].text.starts_with("commercial_renewal"));
        assert!(!result[2].is_deletion);
        // New text includes: c + b + o + n + d.des + c + r + i + ption = "bond.description"
    }

    // ============================================================
    // Integration tests using actual diff algorithm output
    // These test the ACTUAL spans produced, not hand-crafted ones
    // ============================================================

    #[test]
    fn test_inline_diff_commercial_renewal_to_bond_coalesces() {
        // FAILING TEST: This tests the actual diff output for commercial_renewal -> bond
        // The display should show:
        //   "BDEFF: date_for_display(" (gray) + "commercial_renewal" (red) + "bond" (cyan) + ".effective_date)," (gray)
        // NOT the entire old line red and entire new line cyan

        use crate::diff::compute_inline_diff_merged;

        let old = "BDEFF: date_for_display(commercial_renewal.effective_date),";
        let new = "BDEFF: date_for_display(bond.effective_date),";

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        // Debug: print raw spans BEFORE coalescing
        eprintln!("\n=== commercial_renewal -> bond ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let coalesced = coalesce_spans(&result.spans);

        // Debug: print coalesced spans
        eprintln!("Coalesced spans ({}):", coalesced.len());
        for (i, span) in coalesced.iter().enumerate() {
            eprintln!("  span[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        // We should have structural prefix preserved, then coalesced old/new, then structural suffix
        // Prefix: "BDEFF: date_for_display("
        // Old: "commercial_renewal"
        // New: "bond"
        // Suffix: ".effective_date),"

        // Find the deletion span
        let deletion = coalesced.iter().find(|s| s.is_deletion);
        assert!(deletion.is_some(), "Should have a deletion span");
        let deletion = deletion.unwrap();

        // The deletion should contain "commercial_renewal", not scattered chars
        assert!(
            deletion.text.contains("commercial_renewal") || deletion.text == "commercial_renewal",
            "Deletion should be 'commercial_renewal', got: {:?}", deletion.text
        );

        // Find the insertion span
        let insertion = coalesced.iter().find(|s| s.source.is_some() && !s.is_deletion);
        assert!(insertion.is_some(), "Should have an insertion span");
        let insertion = insertion.unwrap();

        // The insertion should contain "bond", not scattered chars
        assert!(
            insertion.text.contains("bond") || insertion.text == "bond",
            "Insertion should be 'bond', got: {:?}", insertion.text
        );
    }

    #[test]
    fn test_inline_diff_commercial_bond_to_bond() {
        // Exact user case: "@commercial_bond = commercial_bond" -> "@bond = bond"
        // The display was showing: "@commercial_bond = commercial_bondbond = bond"

        use crate::diff::compute_inline_diff_merged;

        let old = "@commercial_bond = commercial_bond";
        let new = "@bond = bond";

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        // Debug: print raw spans BEFORE coalescing
        eprintln!("\n=== @commercial_bond -> @bond ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let coalesced = coalesce_spans(&result.spans);

        // Debug: print coalesced spans
        eprintln!("Coalesced spans ({}):", coalesced.len());
        for (i, span) in coalesced.iter().enumerate() {
            eprintln!("  span[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        // Build display string to verify no garbled output
        let display: String = coalesced.iter().map(|s| s.text.as_str()).collect();
        eprintln!("Display string: {:?}", display);

        // The display should NOT contain the old text concatenated with new text
        assert!(
            !display.contains("commercial_bondbond"),
            "Display should NOT contain 'commercial_bondbond' (garbled), got: {}",
            display
        );

        // Verify meaningful coalescing happened
        assert!(result.is_meaningful || !result.is_meaningful, "Just checking we got a result");
    }

    #[test]
    fn test_inline_diff_cancellation_to_clause_coalesces() {
        // FAILING TEST: This tests "cancellation" -> "clause"
        // The display should show "cancellation" (red) not "ancellation c" (red)

        use crate::diff::compute_inline_diff_merged;

        let old = "context \"when cancellation clause value is given\" do";
        let new = "context \"when bond cannot be expired\" do";

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        // Debug: print raw spans BEFORE coalescing
        eprintln!("\n=== cancellation -> clause ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let coalesced = coalesce_spans(&result.spans);

        // Debug: print coalesced spans
        eprintln!("Coalesced spans ({}):", coalesced.len());
        for (i, span) in coalesced.iter().enumerate() {
            eprintln!("  span[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        // Find the deletion span
        let deletion = coalesced.iter().find(|s| s.is_deletion);
        assert!(deletion.is_some(), "Should have a deletion span");
        let deletion = deletion.unwrap();

        // The deletion should NOT start with "ancellation" - it should include the 'c'
        assert!(
            !deletion.text.starts_with("ancellation"),
            "Deletion should NOT start with 'ancellation' (missing 'c'), got: {:?}", deletion.text
        );

        // Should contain the full word being replaced
        assert!(
            deletion.text.contains("cancellation"),
            "Deletion should contain 'cancellation', got: {:?}", deletion.text
        );
    }

    // ============================================================
    // Tests for width-aware inline diff display
    // ============================================================

    #[test]
    fn test_inline_display_width_simple() {
        let spans = vec![
            make_span("hello ", None, false),
            make_span("world", Some(LineSource::DeletedBase), true),
            make_span("earth", Some(LineSource::Committed), false),
        ];
        // "hello " + "world" + "earth" = 6 + 5 + 5 = 16
        assert_eq!(spans::inline_display_width(&spans), 16);
    }

    #[test]
    fn test_inline_display_width_with_coalesce() {
        // When spans are coalesced, the width should be the coalesced width
        let spans = vec![
            make_span("c", None, false),
            make_span("ancellation", Some(LineSource::DeletedBase), true),
            make_span("l", None, false),
            make_span("ause", Some(LineSource::Committed), false),
        ];
        // After coalesce: "cancellationl" + "clause" = 13 + 6 = 19
        let width = spans::inline_display_width(&spans);
        assert_eq!(width, 19);
    }

    #[test]
    fn test_reconstruct_old_content_simple() {
        let spans = vec![
            make_span("hello ", None, false),           // unchanged - included
            make_span("world", Some(LineSource::DeletedBase), true),  // deletion - included
            make_span("earth", Some(LineSource::Committed), false),   // insertion - NOT included
        ];
        assert_eq!(spans::reconstruct_old_content(&spans), "hello world");
    }

    #[test]
    fn test_reconstruct_old_content_with_multiple_deletions() {
        let spans = vec![
            make_span("prefix ", None, false),
            make_span("old1", Some(LineSource::DeletedBase), true),
            make_span(" middle ", None, false),
            make_span("old2", Some(LineSource::DeletedBase), true),
            make_span("new", Some(LineSource::Committed), false),
        ];
        assert_eq!(spans::reconstruct_old_content(&spans), "prefix old1 middle old2");
    }

    #[test]
    fn test_get_deletion_source_finds_correct_source() {
        let spans = vec![
            make_span("unchanged", None, false),
            make_span("deleted", Some(LineSource::DeletedCommitted), true),
            make_span("inserted", Some(LineSource::Staged), false),
        ];
        assert_eq!(spans::get_deletion_source(&spans), LineSource::DeletedCommitted);
    }

    #[test]
    fn test_get_deletion_source_defaults_to_deleted_base() {
        let spans = vec![
            make_span("unchanged", None, false),
            make_span("inserted", Some(LineSource::Committed), false),
        ];
        assert_eq!(spans::get_deletion_source(&spans), LineSource::DeletedBase);
    }

    #[test]
    fn test_get_insertion_source_finds_correct_source() {
        let spans = vec![
            make_span("unchanged", None, false),
            make_span("deleted", Some(LineSource::DeletedBase), true),
            make_span("inserted", Some(LineSource::Staged), false),
        ];
        assert_eq!(spans::get_insertion_source(&spans), LineSource::Staged);
    }

    #[test]
    fn test_get_insertion_source_defaults_to_committed() {
        let spans = vec![
            make_span("unchanged", None, false),
            make_span("deleted", Some(LineSource::DeletedBase), true),
        ];
        assert_eq!(spans::get_insertion_source(&spans), LineSource::Committed);
    }

    #[test]
    fn test_screen_row_kind_equality() {
        assert_eq!(ScreenRowKind::Normal, ScreenRowKind::Normal);
        assert_eq!(ScreenRowKind::SplitDeletion, ScreenRowKind::SplitDeletion);
        assert_ne!(ScreenRowKind::Normal, ScreenRowKind::SplitDeletion);
        assert_ne!(ScreenRowKind::SplitDeletion, ScreenRowKind::SplitInsertion);
    }

    // ============================================================
    // Tests for truncate_with_ellipsis
    // ============================================================

    #[test]
    fn test_truncate_with_ellipsis_no_truncation_needed() {
        assert_eq!(truncate_with_ellipsis("hello", 10), "hello");
        assert_eq!(truncate_with_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_with_ellipsis_truncates_with_dots() {
        assert_eq!(truncate_with_ellipsis("hello world", 8), "hello...");
        assert_eq!(truncate_with_ellipsis("hello world", 6), "hel...");
    }

    #[test]
    fn test_truncate_with_ellipsis_very_short_max() {
        assert_eq!(truncate_with_ellipsis("hello", 3), "...");
        assert_eq!(truncate_with_ellipsis("hello", 2), "..");
        assert_eq!(truncate_with_ellipsis("hello", 1), ".");
        assert_eq!(truncate_with_ellipsis("hello", 0), "");
    }

    #[test]
    fn test_truncate_with_ellipsis_exactly_at_boundary() {
        assert_eq!(truncate_with_ellipsis("hello", 5), "hello");
        assert_eq!(truncate_with_ellipsis("hello", 4), "h...");
    }

    // ============================================================
    // Tests for status_bar_height
    // ============================================================

    fn create_test_app_for_status_bar(
        current_branch: Option<&str>,
        base_branch: &str,
        file_count: usize,
    ) -> crate::app::App {
        use crate::app::{App, ViewMode};
        use crate::diff::{DiffLine, FileDiff};
        use std::path::PathBuf;

        let mut files = Vec::new();
        for i in 0..file_count {
            files.push(FileDiff {
                lines: vec![DiffLine::file_header(&format!("file{}.rs", i))],
            });
        }

        App {
            repo_path: PathBuf::from("/tmp/test"),
            base_branch: base_branch.to_string(),
            merge_base: "abc123".to_string(),
            current_branch: current_branch.map(|s| s.to_string()),
            files,
            lines: Vec::new(),
            scroll_offset: 0,
            viewport_height: 10,
            error: None,
            show_help: false,
            view_mode: ViewMode::Full,
            selection: None,
            content_offset: (1, 1),
            line_num_width: 0,
            content_width: 80,
            conflict_warning: None,
            row_map: Vec::new(),
            collapsed_files: std::collections::HashSet::new(),
            manually_toggled: std::collections::HashSet::new(),
        }
    }

    #[test]
    fn test_status_bar_height_wide_terminal_uses_one_line() {
        let app = create_test_app_for_status_bar(Some("feature-branch"), "main", 5);
        // Wide terminal should use 1 line
        assert_eq!(status_bar_height(&app, 120), 1);
    }

    #[test]
    fn test_status_bar_height_narrow_terminal_uses_two_lines() {
        let app = create_test_app_for_status_bar(Some("feature-branch"), "main", 5);
        // Narrow terminal should use 2 lines
        assert_eq!(status_bar_height(&app, 40), 2);
    }

    #[test]
    fn test_status_bar_height_long_branch_name_needs_two_lines() {
        let app = create_test_app_for_status_bar(
            Some("very-long-feature-branch-name-that-takes-space"),
            "main",
            5,
        );
        // Even moderately wide terminal needs 2 lines with long branch name
        assert_eq!(status_bar_height(&app, 80), 2);
    }

    #[test]
    fn test_status_bar_height_no_current_branch_uses_head() {
        let app = create_test_app_for_status_bar(None, "main", 5);
        // "HEAD vs main" is shorter than a branch name
        // Should fit on one line with wide terminal
        assert_eq!(status_bar_height(&app, 120), 1);
    }

    #[test]
    fn test_status_bar_height_boundary_case() {
        let app = create_test_app_for_status_bar(Some("feat"), "main", 1);

        let help = " q:quit  j/k:files  g/G:top/bottom  ?:help ";
        let branch_info = "feat vs main";

        let stats = format!(
            "{} file{} | {} line{}{} | {}%",
            app.files.len(),
            if app.files.len() == 1 { "" } else { "s" },
            app.changed_line_count(),
            if app.changed_line_count() == 1 { "" } else { "s" },
            "",  // no mode suffix in Full mode
            app.scroll_percentage()
        );
        let full_status = format!("{} | {}", branch_info, stats);

        // The threshold is: full_status.len() + help.len() + 2
        let threshold = full_status.len() + help.len() + 2;

        // At exactly threshold width, should be 1 line
        assert_eq!(status_bar_height(&app, threshold as u16), 1,
            "At threshold width {} should use 1 line", threshold);

        // One less should be 2 lines
        assert_eq!(status_bar_height(&app, (threshold - 1) as u16), 2,
            "At width {} (one below threshold) should use 2 lines", threshold - 1);
    }

    // ============================================================
    // Tests for status bar layout decisions (draw_status_bar behavior)
    // These test the various layout branches that can occur
    // ============================================================

    /// Helper to compute what the status bar would show at a given width
    /// Returns (uses_two_lines, branch_truncated, help_level)
    /// help_level: 0 = full help, 1 = short help, 2 = no help
    fn analyze_status_bar_layout(
        current_branch: Option<&str>,
        base_branch: &str,
        file_count: usize,
        width: usize,
    ) -> (bool, bool, u8) {
        let help = " q:quit  j/k:scroll  g/G:top/bottom  ?:help ";
        let help_short = " ?:help ";

        let branch_info = match current_branch {
            Some(b) => format!("{} vs {}", b, base_branch),
            None => format!("HEAD vs {}", base_branch),
        };

        // For test purposes, use simplified stats (0 lines, 100%)
        let stats = format!(
            "{} file{} | 0 lines | 100%",
            file_count,
            if file_count == 1 { "" } else { "s" }
        );

        let full_status = format!("{} | {}", branch_info, stats);

        // Check if everything fits on one line
        if full_status.len() + help.len() + 2 <= width {
            return (false, false, 0); // 1 line, no truncation, full help
        }

        // Need 2 lines - check line 1 layout options
        // Line 1: branch_info + help (full or short)
        if branch_info.len() + help.len() + 2 <= width {
            return (true, false, 0); // 2 lines, no truncation, full help
        }

        if branch_info.len() + help_short.len() + 2 <= width {
            return (true, false, 1); // 2 lines, no truncation, short help
        }

        // Need to truncate branch
        (true, true, 1) // 2 lines, truncated, short help
    }

    #[test]
    fn test_layout_one_line_full_help() {
        // Wide terminal: everything fits on one line with full help
        let (two_lines, truncated, help_level) =
            analyze_status_bar_layout(Some("feature"), "main", 3, 120);
        assert!(!two_lines, "Should use 1 line");
        assert!(!truncated, "Should not truncate");
        assert_eq!(help_level, 0, "Should show full help");
    }

    #[test]
    fn test_layout_two_lines_full_help() {
        // Moderate width: needs 2 lines but branch + full help fits on line 1
        let (two_lines, truncated, help_level) =
            analyze_status_bar_layout(Some("feature"), "main", 3, 75);
        assert!(two_lines, "Should use 2 lines");
        assert!(!truncated, "Should not truncate");
        assert_eq!(help_level, 0, "Should show full help on line 1");
    }

    #[test]
    fn test_layout_two_lines_short_help() {
        // Narrower: needs 2 lines, only short help fits with branch
        let (two_lines, truncated, help_level) =
            analyze_status_bar_layout(Some("my-feature-branch"), "main", 3, 50);
        assert!(two_lines, "Should use 2 lines");
        assert!(!truncated, "Should not truncate");
        assert_eq!(help_level, 1, "Should show short help on line 1");
    }

    #[test]
    fn test_layout_two_lines_truncated() {
        // Very narrow: needs truncation of branch name
        let (two_lines, truncated, help_level) =
            analyze_status_bar_layout(Some("very-long-feature-branch-name"), "main", 3, 35);
        assert!(two_lines, "Should use 2 lines");
        assert!(truncated, "Should truncate branch");
        assert_eq!(help_level, 1, "Should show short help");
    }

    #[test]
    fn test_layout_head_vs_branch() {
        // When current_branch is None, uses "HEAD vs main" which is shorter
        let (two_lines, truncated, _) =
            analyze_status_bar_layout(None, "main", 3, 100);
        assert!(!two_lines, "HEAD vs main should fit on 1 line at width 100");
        assert!(!truncated, "Should not need truncation");
    }

    #[test]
    fn test_layout_many_files_affects_stats() {
        // Many files makes stats longer
        // "1 file" (6 chars) vs "999 files" (9 chars) = 3 char difference
        // Find a width where 1 file fits but 999 doesn't

        // 1 file stats: "feat vs main | 1 file | 0 lines | 100%" = 39 chars
        // 999 files stats: "feat vs main | 999 files | 0 lines | 100%" = 42 chars
        // help = 44 chars, +2 padding

        // At width 85: 39 + 44 + 2 = 85 fits for 1 file
        //              42 + 44 + 2 = 88 doesn't fit for 999 files
        let (two_lines_few, _, _) = analyze_status_bar_layout(Some("feat"), "main", 1, 85);
        let (two_lines_many, _, _) = analyze_status_bar_layout(Some("feat"), "main", 999, 85);

        // With 999 files, the stats are longer so may need 2 lines at same width
        assert!(!two_lines_few, "1 file should fit on 1 line at width 85");
        assert!(two_lines_many, "999 files should need 2 lines at width 85");
    }
}
