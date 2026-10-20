//! Whether to ask the terminal which image protocol it speaks at startup.

use ratatui_image::picker::Picker;

/// Environment variables a terminal multiplexer sets; each swallows the query.
const MULTIPLEXER_VARS: [&str; 3] = ["ZELLIJ", "TMUX", "STY"];

/// Whether `TERM` leaves room for an answer. Windows consoles do not set `TERM`
/// and still answer, so an unset one is asked there; elsewhere an unset, empty
/// or `dumb` one cannot answer.
pub(crate) fn term_can_answer(term: Option<&str>, windows: bool) -> bool {
    match term {
        None => windows,
        Some("" | "dumb") => false,
        Some(_) => true,
    }
}

/// Whether any multiplexer variable is set, given a lookup for "is this variable set".
pub(crate) fn in_multiplexer(is_set: impl Fn(&str) -> bool) -> bool {
    MULTIPLEXER_VARS.iter().any(|name| is_set(name))
}

/// Whether to ask the terminal which image protocol it speaks. A terminal that
/// cannot answer or a multiplexer that swallows the query is not asked: the wait
/// for a reply holds the first keypress and leaves raw mode switched off.
pub(crate) fn should_query_terminal(
    term: Option<&str>,
    in_multiplexer: bool,
    windows: bool,
) -> bool {
    term_can_answer(term, windows) && !in_multiplexer
}

/// The image-protocol picker for this terminal: asked once, and halfblocks
/// wherever asking would not be answered.
#[doc(hidden)]
pub fn detect_image_picker() -> Picker {
    let multiplexed = in_multiplexer(|name| std::env::var_os(name).is_some());
    let term = std::env::var("TERM").ok();
    if should_query_terminal(term.as_deref(), multiplexed, cfg!(windows)) {
        Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks())
    } else {
        Picker::halfblocks()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dumb_or_empty_terminal_is_never_queried() {
        for windows in [false, true] {
            for term in [Some(""), Some("dumb")] {
                assert!(!should_query_terminal(term, false, windows), "{term:?} {windows}");
            }
            assert!(should_query_terminal(Some("xterm-256color"), false, windows));
        }
    }

    #[test]
    fn an_unset_term_is_queried_on_windows_only() {
        assert!(should_query_terminal(None, false, true));
        assert!(!should_query_terminal(None, false, false));
    }

    #[test]
    fn a_multiplexer_is_never_queried() {
        for windows in [false, true] {
            assert!(!should_query_terminal(Some("xterm-256color"), true, windows));
            assert!(!should_query_terminal(None, true, windows));
        }
    }

    #[test]
    fn each_multiplexer_variable_alone_marks_a_multiplexer() {
        for name in ["ZELLIJ", "TMUX", "STY"] {
            assert!(in_multiplexer(|var| var == name), "{name}");
        }
        assert!(!in_multiplexer(|_| false));
        assert!(!in_multiplexer(|var| var == "TERM"));
    }
}
