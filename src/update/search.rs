use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};

use crate::app::App;
use crate::message::UpdateResult;

/// Handle raw input events when the search bar is active.
pub(super) fn handle_search_input(event: Event, app: &mut App) -> UpdateResult {
    let mut result = UpdateResult {
        needs_redraw: true,
        ..Default::default()
    };

    match event {
        Event::Key(key) => match (key.code, key.modifiers) {
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
