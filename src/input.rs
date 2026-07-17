use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};

/// Actions that can be performed in the app
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppAction {
    Quit,
    ScrollUp(usize),
    ScrollDown(usize),
    PageUp,
    PageDown,
    GoToTop,
    GoToBottom,
    NextFile,
    PrevFile,
    ToggleHelp,
    CycleViewMode,
    StartSelection(u16, u16),
    UpdateSelection(u16, u16),
    EndSelection,
    CopyPath,
    CopyDiff,
    CopyPatch,
    CopyOrQuit,
    OpenSearch,
    ToggleDiffBase,
    ToggleReviewed,
    ToggleAllReviewed,
    OpenEditor,
    OpenRepo,
    /// Accept the recovery offered in the error banner (e.g. run
    /// `jj workspace update-stale`). A no-op when nothing is pending.
    RunRecovery,
    Resize,
    /// Throw away what we believe is on screen and repaint every cell.
    ForceRepaint,
    None,
}

/// Convert a crossterm event into an app action
pub fn handle_event(event: Event) -> AppAction {
    match event {
        Event::Key(key) => handle_key_event(key.code, key.modifiers),
        Event::Mouse(mouse) => handle_mouse_event(mouse.kind, mouse.column, mouse.row),
        // A resize repaints in full rather than relying on ratatui's autoresize:
        // autoresize only invalidates when the *dimensions* changed, so a
        // same-size SIGWINCH — what a lid-open or tmux reattach typically
        // delivers — would otherwise diff against a buffer we can no longer
        // trust and write almost nothing.
        Event::Resize(_, _) => AppAction::ForceRepaint,
        // The terminal was hidden or the display slept while we were focused
        // out; whatever is on screen now is not ours to trust.
        Event::FocusGained => AppAction::ForceRepaint,
        _ => AppAction::None,
    }
}

/// Handle keyboard input
fn handle_key_event(code: KeyCode, modifiers: KeyModifiers) -> AppAction {
    match (code, modifiers) {
        // Quit
        (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => AppAction::Quit,

        // Ctrl+C: copy if selection exists, otherwise quit
        // (Cmd+C on macOS is intercepted by the terminal, not the app)
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => AppAction::CopyOrQuit,

        // Ctrl+L: the universal "redraw this screen" convention (readline, vim
        // :redraw, less). The always-available escape hatch when the terminal
        // has been repainted underneath us and no focus event told us so.
        (KeyCode::Char('l'), KeyModifiers::CONTROL) => AppAction::ForceRepaint,

        (KeyCode::Up, _) => AppAction::ScrollUp(1),
        (KeyCode::Down, _) => AppAction::ScrollDown(1),

        (KeyCode::Char('j'), _) => AppAction::NextFile,
        (KeyCode::Char('k'), _) => AppAction::PrevFile,

        // Page up
        (KeyCode::PageUp, _) => AppAction::PageUp,
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => AppAction::PageUp,

        // Page down
        (KeyCode::PageDown, _) => AppAction::PageDown,
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => AppAction::PageDown,

        // Go to top/bottom
        (KeyCode::Char('g'), KeyModifiers::NONE) => AppAction::GoToTop,
        (KeyCode::Char('G'), KeyModifiers::SHIFT) => AppAction::GoToBottom,
        (KeyCode::Char('G'), KeyModifiers::NONE) => AppAction::GoToBottom,
        (KeyCode::Home, _) => AppAction::GoToTop,
        (KeyCode::End, _) => AppAction::GoToBottom,

        // Review all / un-review all files
        (KeyCode::Char('R'), _) => AppAction::ToggleAllReviewed,

        // Help
        (KeyCode::Char('?'), _) => AppAction::ToggleHelp,

        // Cycle view mode
        (KeyCode::Char('c'), KeyModifiers::NONE) => AppAction::CycleViewMode,

        // Copy current file path with 'p'
        (KeyCode::Char('p'), KeyModifiers::NONE) => AppAction::CopyPath,

        // Copy entire diff with 'Y' (vim yank all)
        (KeyCode::Char('Y'), KeyModifiers::SHIFT) => AppAction::CopyDiff,
        (KeyCode::Char('Y'), KeyModifiers::NONE) => AppAction::CopyDiff,

        // Copy git patch format with 'D'
        (KeyCode::Char('D'), KeyModifiers::SHIFT) => AppAction::CopyPatch,
        (KeyCode::Char('D'), KeyModifiers::NONE) => AppAction::CopyPatch,

        // Search
        (KeyCode::Char('/'), _) => AppAction::OpenSearch,
        (KeyCode::Char('f'), KeyModifiers::CONTROL) => AppAction::OpenSearch,

        // Toggle diff base (fork point vs trunk tip)
        (KeyCode::Char('m'), KeyModifiers::NONE) => AppAction::ToggleDiffBase,

        // Toggle reviewed state for current file
        (KeyCode::Char('r'), KeyModifiers::NONE) => AppAction::ToggleReviewed,

        (KeyCode::Char('e'), KeyModifiers::NONE) => AppAction::OpenEditor,

        // Open the whole repo in the editor. Some terminals report a capital
        // letter without the SHIFT modifier, so accept both (matching G/Y/D).
        (KeyCode::Char('E'), KeyModifiers::SHIFT | KeyModifiers::NONE) => AppAction::OpenRepo,

        // Accept the suggested recovery shown in the error banner. The handler
        // ignores this when no recovery is pending, so plain 'u' remains a
        // no-op outside of error-banner state.
        (KeyCode::Char('u'), KeyModifiers::NONE) => AppAction::RunRecovery,

        _ => AppAction::None,
    }
}

/// Handle mouse input
fn handle_mouse_event(kind: MouseEventKind, column: u16, row: u16) -> AppAction {
    match kind {
        MouseEventKind::ScrollUp => AppAction::ScrollUp(3),
        MouseEventKind::ScrollDown => AppAction::ScrollDown(3),
        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
            AppAction::StartSelection(column, row)
        }
        MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
            AppAction::UpdateSelection(column, row)
        }
        MouseEventKind::Up(crossterm::event::MouseButton::Left) => AppAction::EndSelection,
        _ => AppAction::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyEventKind, KeyEventState, MouseButton, MouseEvent};

    fn key_event(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    /// Returning to a terminal that was hidden or asleep is the whole bug: the
    /// screen may have been repainted underneath us while we sat idle drawing
    /// nothing, so trust nothing and rewrite it all.
    #[test]
    fn test_focus_gained_forces_a_repaint() {
        assert_eq!(handle_event(Event::FocusGained), AppAction::ForceRepaint);
    }

    /// Losing focus changes nothing on screen — don't do work for it.
    #[test]
    fn test_focus_lost_is_a_no_op() {
        assert_eq!(handle_event(Event::FocusLost), AppAction::None);
    }

    /// A same-size SIGWINCH (lid open, tmux reattach) must still repaint:
    /// ratatui's autoresize only invalidates when the dimensions actually
    /// changed, so relying on it would leave the stale screen in place.
    #[test]
    fn test_resize_forces_a_repaint() {
        assert_eq!(handle_event(Event::Resize(80, 24)), AppAction::ForceRepaint);
    }

    /// Ctrl+L is the universal redraw convention and the escape hatch for
    /// terminals that never send focus events at all.
    #[test]
    fn test_ctrl_l_forces_a_repaint() {
        assert_eq!(
            handle_event(key_event(KeyCode::Char('l'), KeyModifiers::CONTROL)),
            AppAction::ForceRepaint
        );
    }

    /// A bare `l` must stay free for normal use — only the Ctrl chord repaints.
    #[test]
    fn test_plain_l_is_not_a_repaint() {
        assert_ne!(
            handle_event(key_event(KeyCode::Char('l'), KeyModifiers::NONE)),
            AppAction::ForceRepaint
        );
    }

    fn mouse_event(kind: MouseEventKind) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
    }

    // Quit tests
    #[test]
    fn test_quit_with_q() {
        let event = key_event(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::Quit);
    }

    #[test]
    fn test_quit_with_escape() {
        let event = key_event(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::Quit);
    }

    #[test]
    fn test_copy_or_quit_with_ctrl_c() {
        let event = key_event(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_event(event), AppAction::CopyOrQuit);
    }


    #[test]
    fn test_scroll_up_with_arrow() {
        let event = key_event(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::ScrollUp(1));
    }

    #[test]
    fn test_scroll_down_with_arrow() {
        let event = key_event(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::ScrollDown(1));
    }

    #[test]
    fn test_open_editor_with_e() {
        let event = key_event(KeyCode::Char('e'), KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::OpenEditor);
    }

    #[test]
    fn test_open_repo_with_shift_e() {
        // Both reported forms of a capital E (some terminals omit the modifier).
        let shifted = key_event(KeyCode::Char('E'), KeyModifiers::SHIFT);
        assert_eq!(handle_event(shifted), AppAction::OpenRepo);
        let bare = key_event(KeyCode::Char('E'), KeyModifiers::NONE);
        assert_eq!(handle_event(bare), AppAction::OpenRepo);
    }

    #[test]
    fn test_next_file_with_j() {
        let event = key_event(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::NextFile);
    }

    #[test]
    fn test_prev_file_with_k() {
        let event = key_event(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::PrevFile);
    }

    // Page tests
    #[test]
    fn test_page_up() {
        let event = key_event(KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::PageUp);
    }

    #[test]
    fn test_page_up_with_ctrl_u() {
        let event = key_event(KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(handle_event(event), AppAction::PageUp);
    }

    #[test]
    fn test_page_down() {
        let event = key_event(KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::PageDown);
    }

    #[test]
    fn test_page_down_with_ctrl_d() {
        let event = key_event(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(handle_event(event), AppAction::PageDown);
    }

    // Navigation tests
    #[test]
    fn test_go_to_top_with_g() {
        let event = key_event(KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::GoToTop);
    }

    #[test]
    fn test_go_to_bottom_with_shift_g() {
        let event = key_event(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(handle_event(event), AppAction::GoToBottom);
    }

    #[test]
    fn test_go_to_top_with_home() {
        let event = key_event(KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::GoToTop);
    }

    #[test]
    fn test_go_to_bottom_with_end() {
        let event = key_event(KeyCode::End, KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::GoToBottom);
    }

    // Review all test (Shift+R)
    #[test]
    fn test_toggle_all_reviewed_with_shift_r() {
        let event = key_event(KeyCode::Char('R'), KeyModifiers::SHIFT);
        assert_eq!(handle_event(event), AppAction::ToggleAllReviewed);
    }

    // Toggle reviewed test
    #[test]
    fn test_toggle_reviewed_with_r() {
        let event = key_event(KeyCode::Char('r'), KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::ToggleReviewed);
    }

    // Help test
    #[test]
    fn test_help_with_question_mark() {
        let event = key_event(KeyCode::Char('?'), KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::ToggleHelp);
    }

    // Mouse tests
    #[test]
    fn test_mouse_scroll_up() {
        let event = mouse_event(MouseEventKind::ScrollUp);
        assert_eq!(handle_event(event), AppAction::ScrollUp(3));
    }

    #[test]
    fn test_mouse_scroll_down() {
        let event = mouse_event(MouseEventKind::ScrollDown);
        assert_eq!(handle_event(event), AppAction::ScrollDown(3));
    }

    #[test]
    fn test_mouse_left_click_starts_selection() {
        let event = mouse_event(MouseEventKind::Down(MouseButton::Left));
        assert_eq!(handle_event(event), AppAction::StartSelection(0, 0));
    }

    #[test]
    fn test_mouse_right_click_is_none() {
        let event = mouse_event(MouseEventKind::Down(MouseButton::Right));
        assert_eq!(handle_event(event), AppAction::None);
    }

    #[test]
    fn test_mouse_drag_updates_selection() {
        let event = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(handle_event(event), AppAction::UpdateSelection(10, 5));
    }

    #[test]
    fn test_mouse_release_ends_selection() {
        let event = mouse_event(MouseEventKind::Up(MouseButton::Left));
        assert_eq!(handle_event(event), AppAction::EndSelection);
    }

    #[test]
    fn test_y_is_unbound() {
        let event = key_event(KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::None);
    }

    #[test]
    fn test_cycle_view_mode_with_c() {
        let event = key_event(KeyCode::Char('c'), KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::CycleViewMode);
    }

    #[test]
    fn test_toggle_diff_base_with_m() {
        let event = key_event(KeyCode::Char('m'), KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::ToggleDiffBase);
    }

    #[test]
    fn run_recovery_bound_to_plain_u() {
        // Plain 'u' invokes the recovery offered in the error banner. The
        // handler is a no-op when nothing is pending, so this remains safe
        // for users who don't have a banner showing.
        let event = key_event(KeyCode::Char('u'), KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::RunRecovery);
    }

    #[test]
    fn ctrl_u_is_still_page_up_after_recovery_binding() {
        // Regression guard: adding plain 'u' for recovery must not steal the
        // existing Ctrl+u shortcut.
        let event = key_event(KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(handle_event(event), AppAction::PageUp);
    }

    #[test]
    fn test_copy_path_with_p() {
        let event = key_event(KeyCode::Char('p'), KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::CopyPath);
    }

    #[test]
    fn test_copy_diff_with_shift_y() {
        let event = key_event(KeyCode::Char('Y'), KeyModifiers::SHIFT);
        assert_eq!(handle_event(event), AppAction::CopyDiff);
    }

    #[test]
    fn test_copy_patch_with_shift_d() {
        let event = key_event(KeyCode::Char('D'), KeyModifiers::SHIFT);
        assert_eq!(handle_event(event), AppAction::CopyPatch);
    }

    #[test]
    fn test_search_with_slash() {
        let event = key_event(KeyCode::Char('/'), KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::OpenSearch);
    }

    #[test]
    fn test_search_with_ctrl_f() {
        let event = key_event(KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert_eq!(handle_event(event), AppAction::OpenSearch);
    }

    #[test]
    fn test_unknown_key_is_none() {
        let event = key_event(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::None);
    }
}
