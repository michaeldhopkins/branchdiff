use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};

use crate::app::App;
use crate::message::{Repaint, UpdateResult};

/// Handle raw input events when the search bar is active.
pub(super) fn handle_search_input(event: Event, app: &mut App) -> UpdateResult {
    let mut result = UpdateResult {
        needs_redraw: true,
        ..Default::default()
    };

    match event {
        // A repaint request is orthogonal to search: the screen is stale
        // regardless of which widget has focus. Without these, opening the
        // search box silently disabled both the automatic repaint and its
        // manual escape hatch — leave search open, walk away, come back, and
        // there was no way to fix the screen.
        Event::FocusGained | Event::Resize(_, _) => {
            result.repaint = Repaint::Full;
            return result;
        }
        Event::Key(key) => match (key.code, key.modifiers) {
            // Before the printable-char arm, which only matches NONE|SHIFT and
            // so would never see this — but keep the ordering explicit so a
            // future edit can't make Ctrl+L type an 'l' into the query.
            (KeyCode::Char('l'), KeyModifiers::CONTROL) => result.repaint = Repaint::Full,
            (KeyCode::Esc, _) => app.close_search(),
            (KeyCode::Enter, m) if m.contains(KeyModifiers::SHIFT) => app.search_prev(),
            (KeyCode::Enter, _) => app.search_next(),
            (KeyCode::Backspace, _) => app.search_delete_char(),
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                app.search_insert_char(c)
            }

            // Passthrough navigation
            (KeyCode::Up, _) => app.scroll_up(1),
            (KeyCode::Down, _) => app.scroll_down(1),
            (KeyCode::PageUp, _) => app.page_up(),
            (KeyCode::PageDown, _) => app.page_down(),

            _ => result.needs_redraw = false,
        },
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => app.scroll_up(3),
            MouseEventKind::ScrollDown => app.scroll_down(3),
            _ => result.needs_redraw = false,
        },
        _ => result.needs_redraw = false,
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyEventKind, KeyEventState};

    use crate::test_support::{base_line, TestAppBuilder};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    fn build_app_with_search() -> App {
        let lines = vec![
            base_line("hello world"),
            base_line("foo bar"),
            base_line("hello again"),
        ];
        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_viewport_height(20)
            .build();
        app.open_search();
        app
    }

    /// A stale screen is stale regardless of which widget has focus. Every
    /// other mode repaints on FocusGained; search must not be the one place
    /// where returning to the terminal leaves a half-painted screen.
    #[test]
    fn test_search_focus_gained_forces_a_repaint() {
        let mut app = build_app_with_search();
        let result = handle_search_input(Event::FocusGained, &mut app);
        assert_eq!(result.repaint, Repaint::Full,
            "FocusGained must repaint even while the search box is open");
        assert!(result.needs_redraw);
    }

    /// Ctrl+L is the advertised "always works" escape hatch. If it silently
    /// stops working with the search box open, it isn't one.
    #[test]
    fn test_search_ctrl_l_forces_a_repaint() {
        let mut app = build_app_with_search();
        let result = handle_search_input(key(KeyCode::Char('l'), KeyModifiers::CONTROL), &mut app);
        assert_eq!(result.repaint, Repaint::Full,
            "Ctrl+L must repaint even while the search box is open");
    }

    /// ...and it must not be mistaken for typing the letter 'l' into the query.
    #[test]
    fn test_search_ctrl_l_does_not_type_into_the_query() {
        let mut app = build_app_with_search();
        handle_search_input(key(KeyCode::Char('l'), KeyModifiers::CONTROL), &mut app);
        assert_eq!(app.search.as_ref().unwrap().query, "", "Ctrl+L is a command, not input");
    }

    /// A plain 'l' must still type.
    #[test]
    fn test_search_plain_l_still_types() {
        let mut app = build_app_with_search();
        handle_search_input(key(KeyCode::Char('l'), KeyModifiers::NONE), &mut app);
        assert_eq!(app.search.as_ref().unwrap().query, "l");
    }

    /// Resize while searching must repaint too: ratatui's autoresize only
    /// invalidates on a dimension change, so a same-size SIGWINCH would leave
    /// the stale screen exactly as it is.
    #[test]
    fn test_search_resize_forces_a_repaint() {
        let mut app = build_app_with_search();
        let result = handle_search_input(Event::Resize(80, 24), &mut app);
        assert_eq!(result.repaint, Repaint::Full);
    }

    #[test]
    fn typing_adds_to_query_and_computes_matches() {
        let mut app = build_app_with_search();

        handle_search_input(key(KeyCode::Char('h'), KeyModifiers::NONE), &mut app);
        handle_search_input(key(KeyCode::Char('e'), KeyModifiers::NONE), &mut app);
        handle_search_input(key(KeyCode::Char('l'), KeyModifiers::NONE), &mut app);

        let search = app.search.as_ref().unwrap();
        assert_eq!(search.query, "hel");
        assert_eq!(search.matches.len(), 2);
    }

    #[test]
    fn backspace_removes_from_query() {
        let mut app = build_app_with_search();

        handle_search_input(key(KeyCode::Char('h'), KeyModifiers::NONE), &mut app);
        handle_search_input(key(KeyCode::Char('e'), KeyModifiers::NONE), &mut app);
        handle_search_input(key(KeyCode::Backspace, KeyModifiers::NONE), &mut app);

        let search = app.search.as_ref().unwrap();
        assert_eq!(search.query, "h");
    }

    #[test]
    fn enter_advances_to_next_match() {
        let mut app = build_app_with_search();
        app.search_insert_char('h');
        app.search_insert_char('e');
        app.search_insert_char('l');

        assert_eq!(app.search.as_ref().unwrap().current, 0);

        handle_search_input(key(KeyCode::Enter, KeyModifiers::NONE), &mut app);
        assert_eq!(app.search.as_ref().unwrap().current, 1);

        // Wraps around
        handle_search_input(key(KeyCode::Enter, KeyModifiers::NONE), &mut app);
        assert_eq!(app.search.as_ref().unwrap().current, 0);
    }

    #[test]
    fn shift_enter_goes_to_previous_match() {
        let mut app = build_app_with_search();
        app.search_insert_char('h');
        app.search_insert_char('e');
        app.search_insert_char('l');

        assert_eq!(app.search.as_ref().unwrap().current, 0);

        handle_search_input(key(KeyCode::Enter, KeyModifiers::SHIFT), &mut app);
        // Wraps to last match
        assert_eq!(app.search.as_ref().unwrap().current, 1);
    }

    #[test]
    fn escape_closes_search() {
        let mut app = build_app_with_search();
        handle_search_input(key(KeyCode::Esc, KeyModifiers::NONE), &mut app);
        assert!(app.search.is_none());
    }

    #[test]
    fn arrow_keys_scroll_passthrough() {
        let lines: Vec<_> = (0..30).map(|i| base_line(&format!("line {i}"))).collect();
        let mut app = TestAppBuilder::new()
            .with_lines(lines)
            .with_viewport_height(10)
            .build();
        app.open_search();

        handle_search_input(key(KeyCode::Down, KeyModifiers::NONE), &mut app);
        assert_eq!(app.view.scroll_offset, 1);

        handle_search_input(key(KeyCode::Up, KeyModifiers::NONE), &mut app);
        assert_eq!(app.view.scroll_offset, 0);
    }

    #[test]
    fn unknown_key_does_not_trigger_redraw() {
        let mut app = build_app_with_search();
        let result = handle_search_input(key(KeyCode::F(5), KeyModifiers::NONE), &mut app);
        assert!(!result.needs_redraw);
    }
}
