use std::time::Instant;

use crate::app::App;
use crate::app::selection::MULTI_CLICK_MS;
use crate::input::AppAction;
use crate::message::{LoopAction, RefreshTrigger, UpdateResult};

use super::RefreshState;
const POSITION_TOLERANCE: u16 = 2;

/// Determine click count for multi-click detection (double/triple click).
fn detect_click_count(app: &App, x: u16, y: u16) -> u8 {
    if let Some((last_time, last_x, last_y, count)) = app.view.last_click {
        let elapsed = last_time.elapsed().as_millis();
        let close_enough =
            x.abs_diff(last_x) <= POSITION_TOLERANCE && y.abs_diff(last_y) <= POSITION_TOLERANCE;

        if elapsed < MULTI_CLICK_MS && close_enough {
            return count + 1;
        }
    }
    1
}

/// Handle click actions based on click count (single/double/triple).
fn handle_click(app: &mut App, x: u16, y: u16, click_count: u8) {
    match click_count {
        2 => {
            // Double-click: select word
            if app.get_file_header_at(x, y).is_none() {
                app.select_word_at(x, y);
            }
        }
        3 => {
            // Triple-click: select line
            if app.get_file_header_at(x, y).is_none() {
                app.select_line_at(x, y);
            }
        }
        _ => {
            // Single click (or 4+, which resets to single-click behavior)
            if let Some(file_path) = app.get_file_header_at(x, y) {
                app.toggle_file_collapsed(&file_path);
            } else {
                app.start_selection(x, y);
            }
        }
    }
}

/// Handle navigation actions (scrolling, file navigation).
fn handle_navigation(action: &AppAction, app: &mut App) {
    match action {
        AppAction::ScrollUp(n) => app.scroll_up(*n),
        AppAction::ScrollDown(n) => app.scroll_down(*n),
        AppAction::PageUp => app.page_up(),
        AppAction::PageDown => app.page_down(),
        AppAction::GoToTop => app.go_to_top(),
        AppAction::GoToBottom => app.go_to_bottom(),
        AppAction::NextFile => app.next_file(),
        AppAction::PrevFile => app.prev_file(),
        _ => {}
    }
}

/// Handle clipboard operations.
fn handle_clipboard(action: &AppAction, app: &mut App) -> Option<LoopAction> {
    match action {
        AppAction::CopyPath => {
            let _ = app.copy_current_path();
        }
        AppAction::CopyDiff => {
            let _ = app.copy_diff();
        }
        AppAction::CopyPatch => {
            let _ = app.copy_patch();
        }
        AppAction::CopyOrQuit => {
            if app.has_selection() {
                let _ = app.copy_selection();
            } else if app.should_quit() {
                return Some(LoopAction::Quit);
            }
        }
        _ => {}
    }
    None
}

/// Handle user input actions.
pub(super) fn handle_input(
    action: AppAction,
    app: &mut App,
    refresh_state: &mut RefreshState,
) -> UpdateResult {
    let mut result = UpdateResult {
        needs_redraw: !matches!(action, AppAction::None),
        ..Default::default()
    };

    match &action {
        // Control actions
        AppAction::Quit => {
            if app.should_quit() {
                result.loop_action = LoopAction::Quit;
            }
        }
        AppAction::Refresh => {
            if refresh_state.is_idle() {
                result.refresh = RefreshTrigger::Full;
            } else {
                refresh_state.cancel_and_mark_pending();
            }
        }

        // Navigation actions
        AppAction::ScrollUp(_)
        | AppAction::ScrollDown(_)
        | AppAction::PageUp
        | AppAction::PageDown
        | AppAction::GoToTop
        | AppAction::GoToBottom
        | AppAction::NextFile
        | AppAction::PrevFile => handle_navigation(&action, app),

        // View actions
        AppAction::ToggleHelp => app.toggle_help(),
        AppAction::CycleViewMode => app.cycle_view_mode(),

        // Selection actions
        AppAction::StartSelection(x, y) => {
            app.cancel_pending_copy();
            let click_count = detect_click_count(app, *x, *y);
            app.view.last_click = Some((Instant::now(), *x, *y, click_count));
            handle_click(app, *x, *y, click_count);
        }
        AppAction::UpdateSelection(x, y) => {
            app.update_selection(*x, *y);
            app.view.last_click = None; // Clear to prevent false double-clicks during drag
        }
        AppAction::EndSelection => app.end_selection_with_auto_copy(),

        // Clipboard actions
        AppAction::CopyPath
        | AppAction::CopyDiff
        | AppAction::CopyPatch
        | AppAction::CopyOrQuit => {
            if let Some(loop_action) = handle_clipboard(&action, app) {
                result.loop_action = loop_action;
            }
        }

        // Search
        AppAction::OpenSearch => app.open_search(),

        // Toggle diff base (fork point vs trunk tip)
        AppAction::ToggleDiffBase => {
            app.toggle_diff_base();
            if refresh_state.is_idle() {
                result.refresh = RefreshTrigger::Full;
            } else {
                refresh_state.cancel_and_mark_pending();
            }
        }

        // No-op actions
        AppAction::Resize | AppAction::None => {}
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::Instant;

    use crate::test_support::{base_line, TestAppBuilder};

    #[test]
    fn test_handle_input_quit() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::Quit, &mut app, &mut refresh_state);
        assert_eq!(result.loop_action, LoopAction::Quit);
    }

    #[test]
    fn test_handle_input_scroll_down() {
        let lines: Vec<_> = (0..20).map(|i| base_line(&format!("line{}", i))).collect();
        let mut app = TestAppBuilder::new().with_lines(lines).build();
        let mut refresh_state = RefreshState::Idle;

        handle_input(AppAction::ScrollDown(5), &mut app, &mut refresh_state);
        assert_eq!(app.view.scroll_offset, 5);
    }

    #[test]
    fn test_handle_input_refresh_when_idle() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::Refresh, &mut app, &mut refresh_state);
        assert_eq!(result.refresh, RefreshTrigger::Full);
    }

    #[test]
    fn test_handle_input_refresh_when_busy_marks_pending() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };

        let result = handle_input(AppAction::Refresh, &mut app, &mut refresh_state);
        assert_eq!(result.refresh, RefreshTrigger::None);
        assert!(refresh_state.has_pending());
    }

    #[test]
    fn test_handle_input_sets_needs_redraw_for_scroll() {
        let mut app = TestAppBuilder::new()
            .with_lines(vec![base_line("line")])
            .build();
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::ScrollDown(1), &mut app, &mut refresh_state);
        assert!(result.needs_redraw);
    }

    #[test]
    fn test_handle_input_sets_needs_redraw_for_resize() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::Resize, &mut app, &mut refresh_state);
        assert!(result.needs_redraw);
    }

    #[test]
    fn test_handle_input_no_redraw_for_none_action() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::None, &mut app, &mut refresh_state);
        assert!(!result.needs_redraw);
    }

    #[test]
    fn test_double_click_selects_word() {
        use crate::ui::ScreenRowInfo;

        // With line_num_width=3, prefix_len = 3 + 1 + 4 = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![ScreenRowInfo {
            content: "hello world".to_string(),
            is_file_header: false,
            file_path: None,
            is_continuation: false,
        }];

        let mut refresh_state = RefreshState::Idle;

        // First click - starts selection
        // Click on 'w' in "world" - content col 6, screen col = 6 + prefix(8) + offset(1) = 15
        handle_input(AppAction::StartSelection(15, 1), &mut app, &mut refresh_state);
        assert!(app.view.last_click.is_some());
        // Should have started a point selection
        assert!(app.view.selection.is_some());

        // Second click at same position (simulate double-click by keeping last_click recent)
        // last_click is already set from first click, and time elapsed is negligible
        handle_input(AppAction::StartSelection(15, 1), &mut app, &mut refresh_state);

        // Should have selected the word "world"
        let sel = app.view.selection.as_ref().expect("Should have selection");
        assert_eq!(sel.start.col, 14); // "world" starts at content col 6 + prefix 8
        assert_eq!(sel.end.col, 19); // "world" ends at content col 11 + prefix 8
    }

    #[test]
    fn test_triple_click_selects_line() {
        use crate::ui::ScreenRowInfo;

        // With line_num_width=3, prefix_len = 3 + 1 + 4 = 8
        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![ScreenRowInfo {
            content: "hello world".to_string(),
            is_file_header: false,
            file_path: None,
            is_continuation: false,
        }];

        let mut refresh_state = RefreshState::Idle;

        // First click - screen_x = 0 + 8 + 1 = 9
        handle_input(AppAction::StartSelection(10, 1), &mut app, &mut refresh_state);
        // Second click (double-click)
        handle_input(AppAction::StartSelection(10, 1), &mut app, &mut refresh_state);
        // Third click (triple-click)
        handle_input(AppAction::StartSelection(10, 1), &mut app, &mut refresh_state);

        // Should have selected the entire line
        let sel = app.view.selection.as_ref().expect("Should have selection");
        assert_eq!(sel.start.row, 0);
        assert_eq!(sel.end.row, 0);
        // Line selection starts at prefix_len = 8
        assert_eq!(sel.start.col, 8);
        // Line selection ends at content length + prefix_len (11 + 8 = 19)
        assert_eq!(sel.end.col, 19);
        // Line selection anchor should be set
        assert!(app.view.line_selection_anchor.is_some());
    }

    #[test]
    fn test_single_click_does_not_select_word() {
        use crate::ui::ScreenRowInfo;

        let mut app = TestAppBuilder::new().build();
        app.view.line_num_width = 3;
        app.view.content_offset = (1, 1);
        app.view.row_map = vec![ScreenRowInfo {
            content: "hello world".to_string(),
            is_file_header: false,
            file_path: None,
            is_continuation: false,
        }];

        let mut refresh_state = RefreshState::Idle;

        // Single click
        handle_input(AppAction::StartSelection(13, 1), &mut app, &mut refresh_state);

        // Should have a point selection, not a word selection
        let sel = app.view.selection.as_ref().expect("Should have selection");
        // Point selection has start == end (or very close)
        assert_eq!(sel.start.col, sel.end.col);
    }

    #[test]
    fn test_drag_clears_last_click() {
        let mut app = TestAppBuilder::new().build();
        app.view.last_click = Some((Instant::now(), 10, 10, 1));

        let mut refresh_state = RefreshState::Idle;

        // Drag action should clear last_click
        handle_input(AppAction::UpdateSelection(15, 10), &mut app, &mut refresh_state);

        assert!(app.view.last_click.is_none());
    }

    #[test]
    fn test_handle_input_copy_patch_sets_needs_redraw() {
        let mut app = TestAppBuilder::new()
            .with_lines(vec![base_line("content")])
            .build();
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::CopyPatch, &mut app, &mut refresh_state);

        // CopyPatch should trigger redraw (to show "Copied" flash)
        assert!(result.needs_redraw);
    }

    #[test]
    fn test_handle_input_copy_diff_sets_needs_redraw() {
        let mut app = TestAppBuilder::new()
            .with_lines(vec![base_line("content")])
            .build();
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::CopyDiff, &mut app, &mut refresh_state);

        assert!(result.needs_redraw);
    }

    #[test]
    fn test_handle_input_copy_path_sets_needs_redraw() {
        let mut app = TestAppBuilder::new()
            .with_lines(vec![base_line("content")])
            .build();
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::CopyPath, &mut app, &mut refresh_state);

        assert!(result.needs_redraw);
    }

    #[test]
    fn test_handle_input_cycle_view_mode() {
        use crate::app::ViewMode;

        let mut app = TestAppBuilder::new()
            .with_view_mode(ViewMode::Full)
            .build();
        let mut refresh_state = RefreshState::Idle;

        let result = handle_input(AppAction::CycleViewMode, &mut app, &mut refresh_state);
        assert_ne!(app.view.view_mode, ViewMode::Full, "view mode should have cycled");
        assert!(result.needs_redraw);
    }

    #[test]
    fn test_handle_input_toggle_help() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;

        assert!(!app.view.show_help);
        handle_input(AppAction::ToggleHelp, &mut app, &mut refresh_state);
        assert!(app.view.show_help);
        handle_input(AppAction::ToggleHelp, &mut app, &mut refresh_state);
        assert!(!app.view.show_help);
    }

    #[test]
    fn test_handle_input_quit_with_help_open_closes_help() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;

        app.view.show_help = true;
        assert!(app.view.show_help);
        let result = handle_input(AppAction::Quit, &mut app, &mut refresh_state);
        assert_ne!(result.loop_action, LoopAction::Quit, "should close help, not quit");
        assert!(!app.view.show_help);
    }

    #[test]
    fn test_handle_input_quit_with_search_open_closes_search() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;

        app.open_search();
        assert!(app.search.is_some());
        let result = handle_input(AppAction::Quit, &mut app, &mut refresh_state);
        assert_ne!(result.loop_action, LoopAction::Quit);
        assert!(app.search.is_none());
    }

    #[test]
    fn test_handle_input_open_search() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;

        assert!(app.search.is_none());
        handle_input(AppAction::OpenSearch, &mut app, &mut refresh_state);
        assert!(app.search.is_some());
        assert!(app.is_search_input_active());
    }

    #[test]
    fn test_quit_cascade_search_then_help_then_quit() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;

        app.open_search();
        app.view.show_help = true;

        let r1 = handle_input(AppAction::Quit, &mut app, &mut refresh_state);
        assert_ne!(r1.loop_action, LoopAction::Quit);
        assert!(app.search.is_none(), "first Quit closes search");
        assert!(app.view.show_help, "help still open");

        let r2 = handle_input(AppAction::Quit, &mut app, &mut refresh_state);
        assert_ne!(r2.loop_action, LoopAction::Quit);
        assert!(!app.view.show_help, "second Quit closes help");

        let r3 = handle_input(AppAction::Quit, &mut app, &mut refresh_state);
        assert_eq!(r3.loop_action, LoopAction::Quit, "third Quit actually quits");
    }

    #[test]
    fn test_start_selection_cancels_pending_copy() {
        let mut app = TestAppBuilder::new().build();
        app.view.pending_copy = Some(Instant::now());
        let mut refresh_state = RefreshState::Idle;

        handle_input(AppAction::StartSelection(10, 5), &mut app, &mut refresh_state);

        assert!(app.view.pending_copy.is_none(), "StartSelection should cancel pending copy");
    }

    #[test]
    fn test_toggle_diff_base_triggers_refresh() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::Idle;
        assert_eq!(app.diff_base, crate::vcs::DiffBase::ForkPoint);

        let result = handle_input(AppAction::ToggleDiffBase, &mut app, &mut refresh_state);
        assert_eq!(app.diff_base, crate::vcs::DiffBase::TrunkTip);
        assert_eq!(result.refresh, RefreshTrigger::Full);
    }

    #[test]
    fn test_toggle_diff_base_when_busy_marks_pending() {
        let mut app = TestAppBuilder::new().build();
        let mut refresh_state = RefreshState::InProgress {
            started_at: Instant::now(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        };

        let result = handle_input(AppAction::ToggleDiffBase, &mut app, &mut refresh_state);
        assert_eq!(app.diff_base, crate::vcs::DiffBase::TrunkTip);
        assert_eq!(result.refresh, RefreshTrigger::None);
        assert!(refresh_state.has_pending());
    }
}
