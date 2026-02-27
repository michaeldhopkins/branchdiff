use crate::diff::DiffLine;

#[derive(Debug, Clone)]
pub struct SearchMatch {
    pub line_idx: usize,
    pub char_start: usize,
    pub char_len: usize,
}

#[derive(Debug)]
pub struct SearchState {
    pub query: String,
    pub input_active: bool,
    pub matches: Vec<SearchMatch>,
    pub current: usize,
    /// How many matches are in currently visible (non-collapsed, non-filtered) lines.
    pub visible_count: usize,
    /// 0-based position of `current` among visible matches only.
    pub visible_position: usize,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            query: String::new(),
            input_active: true,
            matches: Vec::new(),
            current: 0,
            visible_count: 0,
            visible_position: 0,
        }
    }
}

impl SearchState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// 1-based display position among visible matches (0 if no visible matches).
    pub fn current_display(&self) -> usize {
        if self.visible_count == 0 {
            0
        } else {
            self.visible_position + 1
        }
    }

    /// Update `visible_count` and `visible_position` from a set of visible line indices.
    pub fn update_visibility(&mut self, visible_lines: &std::collections::HashSet<usize>) {
        self.visible_count = self.matches.iter().filter(|m| visible_lines.contains(&m.line_idx)).count();
        self.visible_position = self.matches[..=self.current.min(self.matches.len().saturating_sub(1))]
            .iter()
            .filter(|m| visible_lines.contains(&m.line_idx))
            .count()
            .saturating_sub(1);
    }
}

/// Find all case-insensitive substring matches across all lines.
///
/// Uses char offsets (not byte offsets) so highlighting works correctly with
/// multi-byte UTF-8 content where lowercasing can change byte lengths.
pub fn compute_matches(lines: &[DiffLine], query: &str) -> Vec<SearchMatch> {
    if query.is_empty() {
        return vec![];
    }
    let query_lower = query.to_lowercase();
    let query_char_len = query_lower.chars().count();
    let mut matches = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let content_lower = line.content.to_lowercase();
        let mut byte_start = 0;
        while let Some(byte_pos) = content_lower[byte_start..].find(&query_lower) {
            let abs_byte = byte_start + byte_pos;
            let char_start = content_lower[..abs_byte].chars().count();
            matches.push(SearchMatch {
                line_idx: idx,
                char_start,
                char_len: query_char_len,
            });
            // Advance by one char (not one byte) to allow overlapping matches
            byte_start = abs_byte + content_lower[abs_byte..].chars().next().map_or(1, |c| c.len_utf8());
        }
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffLine, LineSource};

    fn make_line(content: &str) -> DiffLine {
        DiffLine::new(LineSource::Base, content.to_string(), ' ', None)
    }

    fn lines(contents: &[&str]) -> Vec<DiffLine> {
        contents.iter().map(|c| make_line(c)).collect()
    }

    #[test]
    fn empty_query_returns_no_matches() {
        let l = lines(&["hello world"]);
        assert!(compute_matches(&l, "").is_empty());
    }

    #[test]
    fn no_match_returns_empty() {
        let l = lines(&["hello world"]);
        assert!(compute_matches(&l, "xyz").is_empty());
    }

    #[test]
    fn single_match() {
        let l = lines(&["hello world"]);
        let m = compute_matches(&l, "world");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].line_idx, 0);
        assert_eq!(m[0].char_start, 6);
        assert_eq!(m[0].char_len, 5);
    }

    #[test]
    fn case_insensitive() {
        let l = lines(&["Hello World"]);
        let m = compute_matches(&l, "hello");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].char_start, 0);
    }

    #[test]
    fn multiple_matches_same_line() {
        let l = lines(&["aaa"]);
        let m = compute_matches(&l, "aa");
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].char_start, 0);
        assert_eq!(m[1].char_start, 1);
    }

    #[test]
    fn matches_across_lines() {
        let l = lines(&["foo bar", "baz foo", "nothing"]);
        let m = compute_matches(&l, "foo");
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].line_idx, 0);
        assert_eq!(m[0].char_start, 0);
        assert_eq!(m[1].line_idx, 1);
        assert_eq!(m[1].char_start, 4);
    }

    #[test]
    fn empty_lines_skipped() {
        let l = lines(&["", "hello", ""]);
        let m = compute_matches(&l, "hello");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].line_idx, 1);
    }

    #[test]
    fn multibyte_unicode_uses_char_offsets() {
        let l = lines(&["café résumé"]);
        let m = compute_matches(&l, "résumé");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].char_start, 5);
        assert_eq!(m[0].char_len, 6);
    }

    #[test]
    fn case_insensitive_multibyte() {
        let l = lines(&["ÜBER cool"]);
        let m = compute_matches(&l, "über");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].char_start, 0);
        assert_eq!(m[0].char_len, 4);
    }

    #[test]
    fn search_state_display_index() {
        use std::collections::HashSet;

        let mut s = SearchState::new();
        assert_eq!(s.current_display(), 0);
        s.matches.push(SearchMatch { line_idx: 0, char_start: 0, char_len: 3 });
        s.matches.push(SearchMatch { line_idx: 1, char_start: 0, char_len: 3 });
        let all_visible: HashSet<usize> = [0, 1].into();

        s.current = 0;
        s.update_visibility(&all_visible);
        assert_eq!(s.current_display(), 1);

        s.current = 1;
        s.update_visibility(&all_visible);
        assert_eq!(s.current_display(), 2);
    }

    #[test]
    fn search_state_display_with_hidden_matches() {
        use std::collections::HashSet;

        let mut s = SearchState::new();
        s.matches.push(SearchMatch { line_idx: 0, char_start: 0, char_len: 3 });
        s.matches.push(SearchMatch { line_idx: 1, char_start: 0, char_len: 3 });
        s.matches.push(SearchMatch { line_idx: 2, char_start: 0, char_len: 3 });

        // Only lines 0 and 2 are visible (line 1 is in a collapsed file)
        let visible: HashSet<usize> = [0, 2].into();
        s.current = 0;
        s.update_visibility(&visible);
        assert_eq!(s.visible_count, 2);
        assert_eq!(s.current_display(), 1);

        // Current points to match on line 2 (index 2 in matches array)
        s.current = 2;
        s.update_visibility(&visible);
        assert_eq!(s.current_display(), 2);
    }
}
