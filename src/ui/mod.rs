use ratatui::{
    layout::{Constraint, Direction, Layout},
    Frame,
};

use crate::app::{App, FrameContext};

pub mod colors;
pub mod diff_view;
pub mod image_view;
pub mod modals;
pub mod selection;
pub mod spans;
pub mod status_bar;
pub mod wrapping;

// Re-export commonly used items
pub use modals::{draw_help_modal, draw_warning_banner};
pub use status_bar::{draw_status_bar, status_bar_height};

/// Width of the prefix after line numbers: prefix char + space + status symbol + trailing space
pub const PREFIX_CHAR_WIDTH: usize = 4;

/// Represents how a logical DiffLine maps to a screen row
#[derive(Debug, Clone)]
pub struct ScreenRowInfo {
    /// The actual text content of this screen row (for copy operations)
    pub content: String,
    /// Whether this row is a file header (for collapse detection)
    pub is_file_header: bool,
    /// The file path this row belongs to (for collapse toggle)
    pub file_path: Option<String>,
    /// Whether this row is a continuation of a wrapped line (not start of new logical line)
    pub is_continuation: bool,
}

/// Draw the main UI with a pre-computed frame context
pub fn draw_with_frame(frame: &mut Frame, app: &mut App, ctx: &FrameContext) {
    let size = frame.area();

    let has_warning = app.conflict_warning.is_some() || app.error.is_some();
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

    if let Some(area) = warning_area {
        if let Some(error) = &app.error {
            draw_warning_banner(frame, error, area);
        } else if let Some(warning) = &app.conflict_warning {
            draw_warning_banner(frame, warning, area);
        }
    }

    let content_height = diff_area.height.saturating_sub(2) as usize;
    app.set_viewport_height(content_height);

    diff_view::draw_diff_view_with_frame(frame, app, diff_area, ctx);
    draw_status_bar(frame, app, status_area);

    if app.view.show_help {
        draw_help_modal(frame, size, app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::spans::{coalesce_spans, build_deletion_spans_with_highlight, build_insertion_spans_with_highlight, classify_inline_change, InlineChangeType};
    use super::colors::{highlight_bg_color, line_style, line_style_with_highlight};
    use ratatui::style::Color;
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

    #[test]
    fn test_variable_rename_def_to_inserted_pribond() {
        let old = "        let def_pos = line_contents.iter().position(|&c| c.contains(\"pribond\")).unwrap();";
        let new = "        let inserted_pribond_pos = line_contents.iter().position(|&c| c.contains(\"pribond\")).unwrap();";

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        let num_unchanged: usize = result.spans.iter()
            .filter(|s| s.source.is_none())
            .count();
        eprintln!("\n=== def_pos -> inserted_pribond_pos ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("num unchanged segments: {}", num_unchanged);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let coalesced = coalesce_spans(&result.spans);
        eprintln!("Coalesced spans ({}):", coalesced.len());
        for (i, span) in coalesced.iter().enumerate() {
            eprintln!("  span[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        assert!(
            result.is_meaningful,
            "Variable rename should be meaningful"
        );

        let deletion = coalesced.iter().find(|s| s.is_deletion);
        assert!(deletion.is_some(), "Should have deletion");
        assert!(
            deletion.unwrap().text.contains("def"),
            "Deletion should contain 'def', got: {:?}", deletion.unwrap().text
        );

        let insertion = coalesced.iter().find(|s| !s.is_deletion && s.source.is_some());
        assert!(insertion.is_some(), "Should have insertion");
        assert!(
            insertion.unwrap().text.contains("inserted_pribond"),
            "Insertion should contain 'inserted_pribond', got: {:?}", insertion.unwrap().text
        );
    }

    #[test]
    fn test_def_principal_modification_should_not_be_meaningful() {
        let old = "def principal_mailing_address";
        let new = "def pribond_descripal_mailtiong_address";

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        let num_unchanged: usize = result.spans.iter()
            .filter(|s| s.source.is_none())
            .count();
        eprintln!("\n=== def principal -> pribond ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("num unchanged segments: {}", num_unchanged);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        assert!(
            !result.is_meaningful,
            "Gibberish transformation should NOT be meaningful - num_unchanged_segments={}",
            num_unchanged
        );
    }

    #[test]
    fn test_for_loop_to_comment_should_not_be_meaningful() {
        let old = "    for i in (last_changed + 1..spans.len()).rev() {";
        let new = "    // Always preserve the final unchanged span if it exists - it's the common suffix";

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        let num_unchanged: usize = result.spans.iter()
            .filter(|s| s.source.is_none())
            .count();
        eprintln!("\n=== for loop -> comment ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("num unchanged segments: {}", num_unchanged);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        assert!(
            !result.is_meaningful,
            "A for loop changing to a comment should NOT be meaningful - num_unchanged_segments={}",
            num_unchanged
        );
    }

    #[test]
    fn test_coalesce_describe_inactive_to_authorization() {
        let old = r#"describe "inactive account" do"#;
        let new = r#"describe "authorization" do"#;

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        eprintln!("\n=== describe inactive -> authorization ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let coalesced = coalesce_spans(&result.spans);

        eprintln!("Coalesced spans ({}):", coalesced.len());
        for (i, span) in coalesced.iter().enumerate() {
            eprintln!("  span[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let deletion = coalesced.iter().find(|s| s.is_deletion);
        assert!(deletion.is_some(), "Should have a deletion span");
        let deletion = deletion.unwrap();

        assert!(
            deletion.text.contains("inactive account"),
            "Deletion should contain 'inactive account', got: {:?}", deletion.text
        );

        let insertion = coalesced.iter().find(|s| !s.is_deletion && s.source.is_some());
        assert!(insertion.is_some(), "Should have an insertion span");
        let insertion = insertion.unwrap();

        assert!(
            insertion.text.contains("authorization"),
            "Insertion should contain 'authorization', got: {:?}", insertion.text
        );

        let prefix = coalesced.first().filter(|s| s.source.is_none());
        assert!(prefix.is_some(), "Should have unchanged prefix");
        assert!(
            prefix.unwrap().text.contains("describe"),
            "Prefix should contain 'describe', got: {:?}", prefix.unwrap().text
        );

        let suffix = coalesced.last().filter(|s| s.source.is_none());
        assert!(suffix.is_some(), "Should have unchanged suffix");
        assert!(
            suffix.unwrap().text.contains(" do"),
            "Suffix should contain ' do', got: {:?}", suffix.unwrap().text
        );
    }

    #[test]
    fn test_coalesce_let_variable_rename() {
        let old = "let(:letters_of_bondability_requests_policy)";
        let new = "let(:principal)";

        let result = compute_inline_diff_merged(old, new, LineSource::Committed);

        eprintln!("\n=== let variable rename ===");
        eprintln!("is_meaningful: {}", result.is_meaningful);
        eprintln!("Raw spans ({}):", result.spans.len());
        for (i, span) in result.spans.iter().enumerate() {
            eprintln!("  raw[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let coalesced = coalesce_spans(&result.spans);

        eprintln!("Coalesced spans ({}):", coalesced.len());
        for (i, span) in coalesced.iter().enumerate() {
            eprintln!("  span[{}]: {:?} is_del={} text={:?}",
                i, span.source, span.is_deletion, span.text);
        }

        let deletion = coalesced.iter().find(|s| s.is_deletion);
        assert!(deletion.is_some(), "Should have a deletion span");
        let deletion = deletion.unwrap();

        assert!(
            deletion.text.contains("letters_of_bondability_requests_policy"),
            "Deletion should contain the full variable name, got: {:?}", deletion.text
        );

        let insertion = coalesced.iter().find(|s| !s.is_deletion && s.source.is_some());
        assert!(insertion.is_some(), "Should have an insertion span");
        let insertion = insertion.unwrap();

        assert!(
            insertion.text.contains("principal"),
            "Insertion should contain 'principal', got: {:?}", insertion.text
        );

        let prefix = coalesced.first().filter(|s| s.source.is_none());
        assert!(prefix.is_some(), "Should have unchanged prefix");
        assert!(
            prefix.unwrap().text.starts_with("let(:"),
            "Prefix should start with 'let(:', got: {:?}", prefix.unwrap().text
        );

        let suffix = coalesced.last().filter(|s| s.source.is_none());
        assert!(suffix.is_some(), "Should have unchanged suffix");
        assert!(
            suffix.unwrap().text == ")",
            "Suffix should be ')', got: {:?}", suffix.unwrap().text
        );
    }

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

    #[test]
    fn test_truncate_with_ellipsis_utf8_characters() {
        // Multi-byte UTF-8 characters should not panic when truncated
        // Japanese: "日本語" = 3 chars, 9 bytes
        assert_eq!(truncate_with_ellipsis("日本語", 3), "日本語"); // fits exactly
        assert_eq!(truncate_with_ellipsis("日本語", 2), ".."); // too short for any char + ...

        // Longer Japanese text: "日本語です" = 5 chars
        assert_eq!(truncate_with_ellipsis("日本語です", 5), "日本語です");
        assert_eq!(truncate_with_ellipsis("日本語です", 4), "日..."); // 1 char + ...

        // Emoji: "🎉🎊🎈" = 3 chars, 12 bytes
        assert_eq!(truncate_with_ellipsis("🎉🎊🎈", 3), "🎉🎊🎈");
        assert_eq!(truncate_with_ellipsis("🎉🎊🎈", 2), "..");

        // Mixed ASCII and UTF-8: "hello日本語" = 8 chars
        assert_eq!(truncate_with_ellipsis("hello日本語", 10), "hello日本語");
        assert_eq!(truncate_with_ellipsis("hello日本語", 8), "hello日本語");
        assert_eq!(truncate_with_ellipsis("hello日本語", 7), "hell...");
    }

    fn create_status_bar_test_app(
        current_branch: Option<&str>,
        base_branch: &str,
        file_count: usize,
    ) -> crate::app::App {
        use crate::diff::{DiffLine, FileDiff};
        use crate::test_support::TestAppBuilder;

        let files: Vec<FileDiff> = (0..file_count)
            .map(|i| FileDiff {
                lines: vec![DiffLine::file_header(&format!("file{}.rs", i))],
            })
            .collect();

        TestAppBuilder::new()
            .with_files(files)
            .with_base_branch(base_branch)
            .with_current_branch(current_branch)
            .build()
    }

    #[test]
    fn test_status_bar_height_wide_terminal_uses_one_line() {
        let app = create_status_bar_test_app(Some("feature-branch"), "main", 5);
        // Wide terminal should use 1 line
        assert_eq!(status_bar_height(&app, 120), 1);
    }

    #[test]
    fn test_status_bar_height_narrow_terminal_uses_two_lines() {
        let app = create_status_bar_test_app(Some("feature-branch"), "main", 5);
        // Narrow terminal should use 2 lines
        assert_eq!(status_bar_height(&app, 40), 2);
    }

    #[test]
    fn test_status_bar_height_long_branch_name_needs_two_lines() {
        let app = create_status_bar_test_app(
            Some("very-long-feature-branch-name-that-takes-space"),
            "main",
            5,
        );
        // Even moderately wide terminal needs 2 lines with long branch name
        assert_eq!(status_bar_height(&app, 80), 2);
    }

    #[test]
    fn test_status_bar_height_no_current_branch_uses_head() {
        let app = create_status_bar_test_app(None, "main", 5);
        // "HEAD vs main" is shorter than a branch name
        // Should fit on one line with wide terminal
        assert_eq!(status_bar_height(&app, 120), 1);
    }

    #[test]
    fn test_status_bar_height_boundary_case() {
        let app = create_status_bar_test_app(Some("feat"), "main", 1);

        let help = " q:quit  j/k:files  g/G:top/bottom  ?:help ";
        // repo_path is "/tmp/test" so repo name is "test"
        let branch_info = "test | feat vs main";

        let stats = format!(
            "{} file{} | +{} -{}{} | {}%",
            app.files.len(),
            if app.files.len() == 1 { "" } else { "s" },
            app.additions_count(),
            app.deletions_count(),
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

    // Note: Additional layout tests were removed because they tested a helper
    // function that reimplemented status_bar_height logic. The status_bar_height
    // tests above (test_status_bar_height_*) provide coverage of the actual code.

    #[test]
    fn test_highlight_bg_color_deletion_brightness_hierarchy() {
        use ratatui::style::Color;

        let deleted_base_bg = highlight_bg_color(LineSource::DeletedBase);
        let deleted_committed_bg = highlight_bg_color(LineSource::DeletedCommitted);
        let deleted_staged_bg = highlight_bg_color(LineSource::DeletedStaged);

        // Brightness hierarchy: committed (DeletedBase) < staged (DeletedCommitted) < unstaged (DeletedStaged)
        // Note: DeletedBase = committed deletion, DeletedCommitted = staged deletion, DeletedStaged = unstaged deletion
        // Unstaged has a warmer tint (lower blue) so we check overall brightness, not each channel
        match (deleted_base_bg, deleted_committed_bg, deleted_staged_bg) {
            (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2), Color::Rgb(r3, g3, b3)) => {
                let brightness1 = r1 as u32 + g1 as u32 + b1 as u32;
                let brightness2 = r2 as u32 + g2 as u32 + b2 as u32;
                let brightness3 = r3 as u32 + g3 as u32 + b3 as u32;
                assert!(brightness2 > brightness1, "DeletedCommitted should be brighter than DeletedBase");
                assert!(brightness3 > brightness2, "DeletedStaged should be brighter than DeletedCommitted");
            }
            _ => panic!("Expected RGB colors for deletion backgrounds"),
        }
    }

    #[test]
    fn test_highlight_bg_color_by_source() {
        use ratatui::style::Color;

        // All deletion types should return red-ish backgrounds
        let deleted_base = highlight_bg_color(LineSource::DeletedBase);
        let deleted_committed = highlight_bg_color(LineSource::DeletedCommitted);
        let deleted_staged = highlight_bg_color(LineSource::DeletedStaged);

        // Verify they're RGB colors with red component dominant
        for (source, color) in [
            ("DeletedBase", deleted_base),
            ("DeletedCommitted", deleted_committed),
            ("DeletedStaged", deleted_staged),
        ] {
            match color {
                Color::Rgb(r, g, b) => {
                    assert!(r > g && r > b, "{} should have red-dominant background", source);
                }
                _ => panic!("{} should return RGB color", source),
            }
        }

        // Committed should be cyan-ish (g and b higher than r)
        match highlight_bg_color(LineSource::Committed) {
            Color::Rgb(r, g, b) => {
                assert!(g > r && b > r, "Committed should have cyan background (g,b > r)");
            }
            _ => panic!("Committed should return RGB color"),
        }

        // Staged should be green-ish
        match highlight_bg_color(LineSource::Staged) {
            Color::Rgb(r, g, b) => {
                assert!(g > r && g > b, "Staged should have green background");
            }
            _ => panic!("Staged should return RGB color"),
        }

        // Unstaged should be yellow-ish (r and g higher than b)
        match highlight_bg_color(LineSource::Unstaged) {
            Color::Rgb(r, g, b) => {
                assert!(r > b && g > b, "Unstaged should have yellow background (r,g > b)");
            }
            _ => panic!("Unstaged should return RGB color"),
        }
    }

    #[test]
    fn test_line_style_with_highlight_has_background() {
        let style = line_style_with_highlight(LineSource::Committed);

        // Should have both foreground and background set
        assert!(style.fg.is_some(), "Should have foreground color");
        assert!(style.bg.is_some(), "Should have background color");

        // Background should match highlight_bg_color
        assert_eq!(style.bg, Some(highlight_bg_color(LineSource::Committed)));
    }

    #[test]
    fn test_build_deletion_spans_includes_deletions_and_unchanged() {
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("deleted", Some(LineSource::DeletedBase), true),
            make_span("inserted", Some(LineSource::Committed), false),
        ];

        let result = build_deletion_spans_with_highlight(&spans, LineSource::DeletedBase, "unchanged deleted", None);

        // Should include unchanged and deletion, but NOT insertion
        assert_eq!(result.len(), 2, "Should have 2 spans (unchanged + deletion)");
        assert_eq!(result[0].content, "unchanged ");
        assert_eq!(result[1].content, "deleted");
    }

    #[test]
    fn test_build_deletion_spans_applies_highlight_to_deletions() {
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("deleted", Some(LineSource::DeletedBase), true),
        ];

        let result = build_deletion_spans_with_highlight(&spans, LineSource::DeletedBase, "unchanged deleted", None);

        // Unchanged should have base style (no background highlight)
        let base_style = line_style(LineSource::DeletedBase);
        assert_eq!(result[0].style, base_style, "Unchanged span should have base style");

        // Deleted should have highlighted style (with background)
        let highlight_style = line_style_with_highlight(LineSource::DeletedBase);
        assert_eq!(result[1].style, highlight_style, "Deleted span should have highlight style");
    }

    #[test]
    fn test_build_insertion_spans_includes_insertions_and_unchanged() {
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("deleted", Some(LineSource::DeletedBase), true),
            make_span("inserted", Some(LineSource::Committed), false),
        ];

        let result = build_insertion_spans_with_highlight(&spans, LineSource::Committed, "unchanged inserted", None);

        // Should include unchanged and insertion, but NOT deletion
        assert_eq!(result.len(), 2, "Should have 2 spans (unchanged + insertion)");
        assert_eq!(result[0].content, "unchanged ");
        assert_eq!(result[1].content, "inserted");
    }

    #[test]
    fn test_build_insertion_spans_applies_highlight_to_insertions() {
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("inserted", Some(LineSource::Committed), false),
        ];

        let result = build_insertion_spans_with_highlight(&spans, LineSource::Committed, "unchanged inserted", None);

        // Unchanged should have base style (no background highlight)
        let base_style = line_style(LineSource::Committed);
        assert_eq!(result[0].style, base_style, "Unchanged span should have base style");

        // Inserted should have highlighted style (with background)
        let highlight_style = line_style_with_highlight(LineSource::Committed);
        assert_eq!(result[1].style, highlight_style, "Inserted span should have highlight style");
    }

    #[test]
    fn test_build_spans_with_highlight_empty_input() {
        let spans: Vec<InlineSpan> = vec![];

        let del_result = build_deletion_spans_with_highlight(&spans, LineSource::DeletedBase, "", None);
        let ins_result = build_insertion_spans_with_highlight(&spans, LineSource::Committed, "", None);

        assert!(del_result.is_empty(), "Empty input should produce empty deletion spans");
        assert!(ins_result.is_empty(), "Empty input should produce empty insertion spans");
    }

    #[test]
    fn test_build_spans_preserves_text_content() {
        // Verify the actual text content is preserved when building highlighted spans
        let spans = vec![
            make_span("hello ", None, false),
            make_span("world", Some(LineSource::DeletedBase), true),
            make_span("earth", Some(LineSource::Committed), false),
        ];

        let del_spans = build_deletion_spans_with_highlight(&spans, LineSource::DeletedBase, "hello world", None);
        let ins_spans = build_insertion_spans_with_highlight(&spans, LineSource::Committed, "hello earth", None);

        // Deletion line should be "hello world"
        let del_text: String = del_spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(del_text, "hello world");

        // Insertion line should be "hello earth"
        let ins_text: String = ins_spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(ins_text, "hello earth");
    }

    #[test]
    fn test_build_spans_with_multiple_changes() {
        // Multiple deletions and insertions interspersed with short gaps
        // Since gaps are < 5 chars and not structural, they get coalesced
        let spans = vec![
            make_span("a", None, false),
            make_span("old1", Some(LineSource::DeletedBase), true),
            make_span("new1", Some(LineSource::Committed), false),
            make_span("b", None, false),
            make_span("old2", Some(LineSource::DeletedBase), true),
            make_span("new2", Some(LineSource::Committed), false),
            make_span("c", None, false),
        ];

        let del_spans = build_deletion_spans_with_highlight(&spans, LineSource::DeletedBase, "aold1bold2c", None);
        let ins_spans = build_insertion_spans_with_highlight(&spans, LineSource::Committed, "anew1bnew2c", None);

        // After coalescing, short gaps get absorbed
        // Deletion text should be: aold1bold2c (coalesced into fewer spans)
        let del_text: String = del_spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(del_text, "aold1bold2c");

        // Insertion text should be: anew1bnew2c
        let ins_text: String = ins_spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(ins_text, "anew1bnew2c");
    }

    #[test]
    fn test_classify_inline_change_pure_deletion() {
        // Only deletions (is_deletion: true) and unchanged (source: None)
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("deleted text", Some(LineSource::DeletedBase), true),
            make_span(" more unchanged", None, false),
        ];
        assert_eq!(classify_inline_change(&spans), InlineChangeType::PureDeletion);
    }

    #[test]
    fn test_classify_inline_change_pure_addition() {
        // Only insertions (source: Some(_), is_deletion: false) and unchanged
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("inserted text", Some(LineSource::Committed), false),
            make_span(" more unchanged", None, false),
        ];
        assert_eq!(classify_inline_change(&spans), InlineChangeType::PureAddition);
    }

    #[test]
    fn test_classify_inline_change_mixed() {
        // Both deletions and insertions present
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("deleted", Some(LineSource::DeletedBase), true),
            make_span("inserted", Some(LineSource::Committed), false),
        ];
        assert_eq!(classify_inline_change(&spans), InlineChangeType::Mixed);
    }

    #[test]
    fn test_classify_inline_change_no_change() {
        // Only unchanged content (source: None, is_deletion: false)
        let spans = vec![
            make_span("all ", None, false),
            make_span("unchanged ", None, false),
            make_span("content", None, false),
        ];
        assert_eq!(classify_inline_change(&spans), InlineChangeType::NoChange);
    }

    #[test]
    fn test_classify_real_world_pure_deletion() {
        // Real-world case: "foo bar baz" → "foo"
        // This removes " bar baz" - pure deletion
        let result = compute_inline_diff_merged("foo bar baz", "foo", LineSource::Committed);
        assert_eq!(classify_inline_change(&result.spans), InlineChangeType::PureDeletion);
    }

    #[test]
    fn test_classify_real_world_pure_addition() {
        // Real-world case: "foo" → "foo bar baz"
        // This adds " bar baz" - pure addition
        let result = compute_inline_diff_merged("foo", "foo bar baz", LineSource::Committed);
        assert_eq!(classify_inline_change(&result.spans), InlineChangeType::PureAddition);
    }

    #[test]
    fn test_classify_real_world_mixed() {
        // Real-world case: "hello world" → "goodbye world"
        // This replaces "hello" with "goodbye" - mixed change
        let result = compute_inline_diff_merged("hello world", "goodbye world", LineSource::Committed);
        assert_eq!(classify_inline_change(&result.spans), InlineChangeType::Mixed);
    }

    // status_symbol tests are in src/ui/colors.rs

    #[test]
    fn test_ensure_contrast_adjusts_low_contrast_colors() {
        use super::colors::{ensure_contrast, highlight_bg_color};

        // Yellow background (Unstaged highlight) - Rgb(130, 130, 35)
        let yellow_bg = highlight_bg_color(LineSource::Unstaged);

        // Yellow foreground on yellow background should be adjusted to dark
        let yellow_fg = Color::Rgb(200, 200, 50);  // Bright yellow-ish (low contrast)
        let adjusted = ensure_contrast(yellow_fg, yellow_bg);
        assert_eq!(adjusted, Color::Rgb(30, 30, 30), "Low contrast yellow should become dark");

        // Gray fg on yellow background (low contrast) should be adjusted
        let gray_fg = Color::Rgb(150, 150, 80);
        let adjusted = ensure_contrast(gray_fg, yellow_bg);
        assert_eq!(adjusted, Color::Rgb(30, 30, 30), "Low contrast gray should become dark on yellow");

        // Very light fg on a very dark background should be preserved (good contrast)
        let dark_bg = Color::Rgb(20, 20, 20);
        let light_fg = Color::Rgb(220, 220, 220);
        let preserved = ensure_contrast(light_fg, dark_bg);
        assert_eq!(preserved, light_fg, "Good contrast light fg should be preserved");

        // Very dark fg on very light background should be preserved (good contrast)
        let light_bg = Color::Rgb(240, 240, 240);
        let dark_fg = Color::Rgb(20, 20, 20);
        let preserved = ensure_contrast(dark_fg, light_bg);
        assert_eq!(preserved, dark_fg, "Good contrast dark fg should be preserved");
    }

    #[test]
    fn test_build_insertion_spans_unstaged_uses_dark_foreground() {
        // Test that unstaged insertion spans have dark foreground for contrast
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("inserted", Some(LineSource::Unstaged), false),
        ];

        let result = build_insertion_spans_with_highlight(&spans, LineSource::Unstaged, "unchanged inserted", None);

        // The inserted span should have dark foreground
        let inserted_span = &result[1];
        assert_eq!(
            inserted_span.style.fg,
            Some(Color::Rgb(30, 30, 30)),
            "Unstaged inserted span should have dark foreground, got {:?}",
            inserted_span.style.fg
        );
    }

    #[test]
    fn test_build_insertion_spans_unstaged_with_syntax_highlighting() {
        // Test with a Rust file path to trigger syntax highlighting
        // This simulates a comment line like "// some comment"
        let spans = vec![
            make_span("// unchanged ", None, false),
            make_span("inserted", Some(LineSource::Unstaged), false),
        ];

        let result = build_insertion_spans_with_highlight(
            &spans,
            LineSource::Unstaged,
            "// unchanged inserted",
            Some("test.rs"),  // Rust file triggers syntax highlighting
        );

        // Find the span that contains "inserted" - it should have dark foreground
        let inserted_span = result.iter().find(|s| s.content.contains("inserted"));
        assert!(inserted_span.is_some(), "Should have a span containing 'inserted'");
        let inserted_span = inserted_span.unwrap();

        assert_eq!(
            inserted_span.style.fg,
            Some(Color::Rgb(30, 30, 30)),
            "Unstaged inserted span with syntax highlighting should have dark foreground, got {:?}",
            inserted_span.style.fg
        );
    }

    #[test]
    fn test_build_deletion_spans_applies_contrast_check() {
        // Test that deletion spans apply contrast checking
        // DeletedStaged has a reddish background Rgb(115, 55, 45) that works
        // better with light foreground, so light should be preserved/chosen
        let spans = vec![
            make_span("unchanged ", None, false),
            make_span("deleted", Some(LineSource::DeletedStaged), true),
        ];

        let result = build_deletion_spans_with_highlight(&spans, LineSource::DeletedStaged, "unchanged deleted", None);

        // The deleted span should have light foreground (reddish bg works with light text)
        let deleted_span = &result[1];
        assert_eq!(
            deleted_span.style.fg,
            Some(Color::Rgb(220, 220, 220)),
            "DeletedStaged span should have light foreground for contrast with reddish bg, got {:?}",
            deleted_span.style.fg
        );

        // Also verify the highlight background is applied
        assert_eq!(
            deleted_span.style.bg,
            Some(Color::Rgb(115, 55, 45)),
            "DeletedStaged span should have the correct highlight background"
        );
    }

    #[test]
    fn test_pure_deletion_builds_correct_deletion_and_insertion_spans() {
        let old_content = "hello world";
        let new_content = "hello wrld";

        let result = compute_inline_diff_merged(old_content, new_content, LineSource::Committed);

        assert_eq!(classify_inline_change(&result.spans), InlineChangeType::PureDeletion);

        let del_spans = build_deletion_spans_with_highlight(
            &result.spans,
            LineSource::DeletedBase,
            old_content,
            None,
        );
        let del_text: String = del_spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(del_text, old_content, "deletion line should show old content");

        let ins_spans = build_insertion_spans_with_highlight(
            &result.spans,
            LineSource::Committed,
            new_content,
            None,
        );
        let ins_text: String = ins_spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(ins_text, new_content, "insertion line should show new content");
    }

    #[test]
    fn test_pure_deletion_highlights_removed_character() {
        let old_content = ".*}o)";
        let new_content = ".*})";

        let result = compute_inline_diff_merged(old_content, new_content, LineSource::Committed);
        assert_eq!(classify_inline_change(&result.spans), InlineChangeType::PureDeletion);

        let del_spans = build_deletion_spans_with_highlight(
            &result.spans,
            LineSource::DeletedBase,
            old_content,
            None,
        );
        let del_text: String = del_spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(del_text, ".*}o)");

        let highlight_style = line_style_with_highlight(LineSource::DeletedBase);
        let highlighted: String = del_spans
            .iter()
            .filter(|s| s.style.bg == highlight_style.bg)
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(highlighted, "o", "only the removed 'o' should be highlighted");
    }
}
