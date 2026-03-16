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
    Refresh,
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
    Resize,
    None,
}

/// Convert a crossterm event into an app action
pub fn handle_event(event: Event) -> AppAction {
    match event {
        Event::Key(key) => handle_key_event(key.code, key.modifiers),
        Event::Mouse(mouse) => handle_mouse_event(mouse.kind, mouse.column, mouse.row),
        Event::Resize(_, _) => AppAction::Resize,
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

        // Refresh
        (KeyCode::Char('r'), _) => AppAction::Refresh,

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

    // Refresh test
    #[test]
    fn test_refresh_with_r() {
        let event = key_event(KeyCode::Char('r'), KeyModifiers::NONE);
        assert_eq!(handle_event(event), AppAction::Refresh);
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
