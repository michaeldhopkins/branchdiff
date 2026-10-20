//! Whether to ask the terminal which image protocol it speaks at startup.

use std::sync::atomic::{AtomicBool, Ordering};

use ratatui_image::picker::Picker;

/// Remembers, for the session, that the terminal did not answer the startup query.
#[derive(Debug, Default)]
pub struct QueryMemory {
    failed: AtomicBool,
}

impl QueryMemory {
    pub const fn new() -> Self {
        Self { failed: AtomicBool::new(false) }
    }

    pub fn note_failure(&self) {
        self.failed.store(true, Ordering::Relaxed);
    }

    pub fn has_failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

/// Whether to ask the terminal which image protocol it speaks. A terminal that
/// cannot answer (`TERM` unset, empty or `dumb`), a multiplexer that swallows the
/// query, or one that already failed to answer this session is not asked: the
/// wait for a reply holds the first keypress and leaves raw mode switched off.
pub fn should_query_terminal(
    term: Option<&str>,
    in_multiplexer: bool,
    memory: &QueryMemory,
) -> bool {
    let answerable = !matches!(term, None | Some("" | "dumb"));
    answerable && !in_multiplexer && !memory.has_failed()
}

static SESSION_MEMORY: QueryMemory = QueryMemory::new();

/// The image-protocol picker for this terminal: asked once, and halfblocks
/// wherever asking would not be answered.
pub fn detect_image_picker() -> Picker {
    let in_multiplexer = std::env::var("ZELLIJ").is_ok()
        || std::env::var("TMUX").is_ok()
        || std::env::var("STY").is_ok();
    let term = std::env::var("TERM").ok();
    if should_query_terminal(term.as_deref(), in_multiplexer, &SESSION_MEMORY) {
        Picker::from_query_stdio().unwrap_or_else(|_| {
            SESSION_MEMORY.note_failure();
            Picker::halfblocks()
        })
    } else {
        Picker::halfblocks()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dumb_or_unset_terminal_is_never_queried() {
        let memory = QueryMemory::new();
        for term in [None, Some(""), Some("dumb")] {
            assert!(!should_query_terminal(term, false, &memory), "{term:?}");
        }
        assert!(should_query_terminal(Some("xterm-256color"), false, &memory));
    }

    #[test]
    fn a_multiplexer_is_never_queried() {
        assert!(!should_query_terminal(Some("xterm-256color"), true, &QueryMemory::new()));
    }

    #[test]
    fn one_failed_query_stops_every_later_one() {
        let memory = QueryMemory::new();
        assert!(!memory.has_failed());
        assert!(should_query_terminal(Some("xterm"), false, &memory));
        memory.note_failure();
        assert!(memory.has_failed());
        assert!(!should_query_terminal(Some("xterm"), false, &memory));
    }
}
