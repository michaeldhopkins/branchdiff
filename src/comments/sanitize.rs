//! Comment text comes from a file anyone may have written, so control
//! characters are replaced before it reaches a terminal or a patch: an escape
//! sequence in a comment body could otherwise retitle the terminal window,
//! clear the screen or hide text.

use std::borrow::Cow;

/// `s` with every control character except tab replaced by a space.
pub fn printable(s: &str) -> Cow<'_, str> {
    if s.chars().any(is_unsafe) {
        Cow::Owned(s.chars().map(|c| if is_unsafe(c) { ' ' } else { c }).collect())
    } else {
        Cow::Borrowed(s)
    }
}

fn is_unsafe(c: char) -> bool {
    c.is_control() && c != '\t'
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn escape_sequences_and_c1_controls_become_spaces() {
        assert_eq!(printable("a\x1b]0;title\x07b"), "a ]0;title b");
        assert_eq!(printable("x\u{9b}31m\r\ny"), "x 31m  y");
        assert_eq!(printable("tab\tkept"), "tab\tkept");
    }

    #[test]
    fn clean_text_is_borrowed() {
        assert!(matches!(printable("plain é 你"), Cow::Borrowed(_)));
    }

    proptest! {
        #[test]
        fn output_has_no_control_but_tab_and_keeps_width_in_chars(s in any::<String>()) {
            let out = printable(&s);
            prop_assert!(out.chars().all(|c| !c.is_control() || c == '\t'));
            prop_assert_eq!(out.chars().count(), s.chars().count());
        }
    }
}
