//! Per-line syntax highlighting cache.
//!
//! Syntect's parser is the dominant per-frame cost in the diff view: a 40-char
//! Rust line costs ~27 µs to parse, and a line of real markdown prose ~33 µs.
//! Re-running it over the whole viewport on every frame made scrolling cost
//! O(visible lines) per keypress — and long prose paragraphs were re-parsed in
//! full for every one-row scroll step, even when only two of their twenty rows
//! were on screen.
//!
//! This caches each line's segments against its index in `App::lines`, so a
//! scroll step only parses the lines it newly exposed.
//!
//! Highlighting is stateful (a block comment colours the lines after it), so a
//! cached line has to record the parser state it left behind. Recording it is
//! what makes an entry reusable across scroll positions, rather than only
//! within the frame that produced it.
//!
//! The cache never parses a line nobody asked for. A line whose predecessors
//! are already cached inherits their context — so scrolling down through a file
//! builds a chain from its first line, and a block comment or fenced block
//! keeps colouring correctly however far you scroll past it. A line landed on
//! cold gets a fresh parser instead, which is what *every* line got before this
//! cache existed. Reconstructing context by parsing backwards was tried and
//! removed: per-line cost varies ~90x with content (33 µs of prose against
//! 2.95 ms for a blank-ish line in Markdown), so any bound expressed in lines
//! let a cold jump stall for hundreds of milliseconds.

use std::collections::HashMap;
use std::sync::Arc;

use syntect::highlighting::{HighlightIterator, HighlightState, Highlighter};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference};

use super::{SyntaxHighlighter, SyntaxSegment, MAX_HIGHLIGHT_LINE_LEN};
use crate::diff::{DiffLine, LineSource};
use crate::ui::colors::DEFAULT_FG;

/// Parser and theme state carried from one line to the next.
///
/// Both halves clone in ~30 ns, so storing one per cached line is cheap enough
/// that a scroll step never has to re-derive its starting context.
#[derive(Clone)]
struct ResumeState {
    parse: ParseState,
    highlight: HighlightState,
}

impl ResumeState {
    fn new(syntax: &SyntaxReference) -> Self {
        Self {
            parse: ParseState::new(syntax),
            highlight: HighlightState::new(theme_highlighter(), ScopeStack::new()),
        }
    }

    /// Highlight `content` and advance past it.
    ///
    /// Never hand this a blank line. Markdown costs **88x more per line** when
    /// blank lines are fed in (2.95 ms vs 33 µs, measured over `jjpr/AGENTS.md`):
    /// each one pops the parser out of its block context, and re-entering on the
    /// next line of prose is enormously expensive in the extended syntax set.
    /// Skipping them is also what the streaming API has always done, so colours
    /// match what the renderer produced before this cache existed.
    fn advance(&mut self, content: &str) -> Vec<SyntaxSegment> {
        debug_assert!(!content.is_empty(), "blank lines must not reach the parser");
        let highlighter = SyntaxHighlighter::global();

        // The syntax set is the "newlines" variant, so a line has to be handed
        // over with its terminator or constructs that end at end-of-line never
        // close. Measured over pairs of syntax-significant lines, dropping it
        // changes the result for 25% of JavaScript, 27% of Python and 20% of
        // SQL pairs — an unterminated string swallows everything below it. It
        // happens to make no difference at all for Rust, Markdown, HTML or PHP,
        // so a test using those languages will not catch its removal.
        let line = format!("{content}\n");
        let Ok(ops) = self.parse.parse_line(&line, &highlighter.syntax_set) else {
            return plain_segments(content);
        };

        HighlightIterator::new(&mut self.highlight, &ops, &line, theme_highlighter())
            .filter_map(|(style, text)| {
                let text = text.strip_suffix('\n').unwrap_or(text);
                (!text.is_empty()).then(|| SyntaxSegment {
                    text: text.to_string(),
                    fg_color: super::style_color(style),
                })
            })
            .collect()
    }
}

fn theme_highlighter() -> &'static Highlighter<'static> {
    use std::sync::OnceLock;
    static THEME_HIGHLIGHTER: OnceLock<Highlighter<'static>> = OnceLock::new();
    THEME_HIGHLIGHTER.get_or_init(|| Highlighter::new(&SyntaxHighlighter::global().theme))
}

fn plain_segments(content: &str) -> Vec<SyntaxSegment> {
    if content.is_empty() {
        return Vec::new();
    }
    vec![SyntaxSegment {
        text: content.to_string(),
        fg_color: DEFAULT_FG,
    }]
}

/// The file whose syntax chain a line belongs to. Lines with no path (blank
/// separators between files) get `""`, which ends the chain either side of them.
fn chain_path(line: &DiffLine) -> &str {
    line.file_path.as_deref().unwrap_or("")
}

/// Whether a line's text is real file content that should advance the parser.
///
/// Headers and elision markers carry chrome (a path, "42 lines hidden") that
/// would corrupt the chain for every line below them.
fn is_content(line: &DiffLine) -> bool {
    !matches!(line.source, LineSource::FileHeader | LineSource::Elided) && !line.is_image_marker()
}

/// Returns the line's segments and whether the parser was actually run.
fn advance_over(state: &mut ResumeState, line: &DiffLine) -> (Vec<SyntaxSegment>, bool) {
    // Over-long lines are left unparsed and deliberately do not advance the
    // state: parsing them is the cost being avoided, and they are generated
    // content (minified bundles, search indexes) with no structure to carry.
    if !is_content(line) || line.content.len() > MAX_HIGHLIGHT_LINE_LEN {
        return (plain_segments(&line.content), false);
    }
    if line.content.is_empty() {
        return (Vec::new(), false);
    }
    (state.advance(&line.content), true)
}

/// Spacing of the lines that keep their exit state. A resume state costs ~775
/// bytes, so keeping one per line would add several MB to a large diff for no
/// gain: a miss only has to reach *some* earlier state, and re-parsing up to
/// this many lines to get there is one frame's work.
const CHECKPOINT_EVERY: usize = 16;

/// Backstop on the walk back to a stored parser state.
///
/// The walk only crosses lines that are already cached, and the checkpoint grid
/// guarantees one every `CHECKPOINT_EVERY`, so this should never bind. It is
/// here so the cost of a lookup is bounded by something local rather than by an
/// invariant maintained elsewhere.
const MAX_CONTEXT_REPLAY: usize = CHECKPOINT_EVERY * 2;

struct CacheEntry {
    segments: Arc<[SyntaxSegment]>,
    /// Parser state *after* this line — the entry state for the next one.
    /// Kept at checkpoints and at the frontier (see `frontier`); `None`
    /// elsewhere, where it can be re-derived from the checkpoint behind it.
    exit: Option<ResumeState>,
}

/// Syntax highlighting memoized against line indices in `App::lines`.
///
/// Must be invalidated whenever those lines are replaced; see
/// `App::apply_refresh_result`.
#[derive(Default)]
pub struct HighlightCache {
    entries: Vec<Option<CacheEntry>>,
    /// Deletion-side (`old_content`) highlights. These sit off the main chain —
    /// the text is the *previous* version of the line, so it must not advance
    /// the state the following lines are parsed from.
    alt: HashMap<usize, Arc<[SyntaxSegment]>>,
    /// Lines actually handed to the parser. The point of the cache is that this
    /// stays flat while scrolling, so tests assert against it rather than
    /// against wall-clock time.
    parses: usize,
    /// The last two lines parsed, whose exit states are kept even off the
    /// checkpoint grid, most recent first.
    ///
    /// Two, not one: rendering a modified line needs its own exit state *and*
    /// the one before it, because the deletion side is parsed from the state
    /// the line was entered in. Keeping only the newest would send every
    /// modified line back to the previous checkpoint to re-derive it.
    frontier: [Option<usize>; 2],
}

impl HighlightCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop everything. Call whenever `App::lines` is rebuilt.
    pub fn invalidate(&mut self) {
        self.entries.clear();
        self.alt.clear();
        self.frontier = [None, None];
    }

    /// How many lines this cache has sent to the parser over its lifetime.
    pub fn parse_count(&self) -> usize {
        self.parses
    }

    /// Highlighted segments for `lines[idx]`, parsing only what isn't cached.
    pub fn segments(&mut self, lines: &[DiffLine], idx: usize) -> Arc<[SyntaxSegment]> {
        if idx >= lines.len() {
            return Arc::from(Vec::new());
        }
        self.sync_len(lines.len());

        if let Some(entry) = &self.entries[idx] {
            return Arc::clone(&entry.segments);
        }

        let (start, mut state) = self.anchor(lines, idx);
        for (i, line) in lines.iter().enumerate().take(idx + 1).skip(start) {
            let (segments, parsed) = advance_over(&mut state, line);
            self.parses += usize::from(parsed);
            let keep_exit = i == idx || i.is_multiple_of(CHECKPOINT_EVERY);
            self.entries[i] = Some(CacheEntry {
                segments: Arc::from(segments),
                exit: keep_exit.then(|| state.clone()),
            });
        }
        self.move_frontier(idx);

        Arc::clone(&self.entries[idx].as_ref().expect("just populated").segments)
    }

    /// Highlighted segments for the deletion side of a modified line, coloured
    /// in the syntax context the deleted text sat in.
    pub fn alt_segments(
        &mut self,
        lines: &[DiffLine],
        idx: usize,
        content: &str,
    ) -> Arc<[SyntaxSegment]> {
        if idx >= lines.len() {
            return Arc::from(plain_segments(content));
        }
        // Before the cache lookup, not after: `sync_len` is what drops entries
        // belonging to a previous set of lines.
        self.sync_len(lines.len());
        if let Some(segments) = self.alt.get(&idx) {
            return Arc::clone(segments);
        }

        // Populate the chain up to this line so its entry state is available.
        self.segments(lines, idx);
        let mut state = self.entry_state(lines, idx);

        let segments: Arc<[SyntaxSegment]> = Arc::from(
            if content.is_empty() || content.len() > MAX_HIGHLIGHT_LINE_LEN {
                plain_segments(content)
            } else {
                self.parses += 1;
                state.advance(content)
            },
        );
        self.alt.insert(idx, Arc::clone(&segments));
        segments
    }

    /// Adopt `idx` as the newest frontier, retiring whatever falls out of the
    /// two-line window unless the checkpoint grid wants it anyway.
    fn move_frontier(&mut self, idx: usize) {
        if self.frontier[0] == Some(idx) {
            return;
        }
        let evicted = self.frontier[1];
        self.frontier = [Some(idx), self.frontier[0]];

        if let Some(prev) = evicted
            && !prev.is_multiple_of(CHECKPOINT_EVERY)
            && !self.frontier.contains(&Some(prev))
            && let Some(entry) = self.entries.get_mut(prev).and_then(|e| e.as_mut())
        {
            entry.exit = None;
        }
    }

    /// The state a line is parsed *from*. Derived from the nearest stored state
    /// behind it rather than read off `idx - 1`, so the answer does not depend
    /// on which lines happen to be holding an exit state right now.
    fn entry_state(&mut self, lines: &[DiffLine], idx: usize) -> ResumeState {
        let (start, mut state) = self.anchor(lines, idx);
        for line in &lines[start..idx] {
            let (_, parsed) = advance_over(&mut state, line);
            self.parses += usize::from(parsed);
        }
        state
    }

    /// Where to start parsing to reach `idx`, and the state to start from.
    ///
    /// Walks back only over lines this cache has *already* parsed, to the
    /// nearest stored exit state. It will not parse a line that nobody asked
    /// for in order to colour one that was asked for: that is the unbounded
    /// case, and per-line parse cost is not predictable enough to bound it by
    /// counting lines (prose is ~33 µs; a whitespace-only line in Markdown is
    /// ~3 ms, and makes the line after it expensive too). On a cold jump this
    /// yields a fresh parser — exactly what the renderer did for every line
    /// before this cache existed. Context is then inherited for free as the
    /// surrounding lines get scrolled through.
    fn anchor(&self, lines: &[DiffLine], idx: usize) -> (usize, ResumeState) {
        let path = chain_path(&lines[idx]);
        let mut start = idx;

        for _ in 0..MAX_CONTEXT_REPLAY {
            let Some(prev) = start.checked_sub(1) else { break };
            if chain_path(&lines[prev]) != path {
                break;
            }
            let Some(entry) = &self.entries[prev] else { break };
            if let Some(exit) = &entry.exit {
                return (start, exit.clone());
            }
            start = prev;
        }

        (start, ResumeState::new(syntax_for(path)))
    }

    fn sync_len(&mut self, len: usize) {
        if self.entries.len() != len {
            self.entries.clear();
            self.entries.resize_with(len, || None);
            self.alt.clear();
            self.frontier = [None, None];
        }
    }
}

fn syntax_for(path: &str) -> &'static SyntaxReference {
    let highlighter = SyntaxHighlighter::global();
    if path.is_empty() {
        highlighter.syntax_set.find_syntax_plain_text()
    } else {
        highlighter.syntax_for_path(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(content: &str, path: &str) -> DiffLine {
        let mut line = DiffLine::new(LineSource::Base, content.to_string(), ' ', None);
        line.file_path = Some(path.to_string());
        line
    }

    fn text_of(segments: &[SyntaxSegment]) -> String {
        segments.iter().map(|s| s.text.as_str()).collect()
    }

    /// Content chosen to poke at the ways a segment split can go wrong:
    /// multi-byte text, wide glyphs, combining marks, tabs, unterminated
    /// constructs, blank and whitespace-only lines.
    const AWKWARD: &[&str] = &[
        "fn main() { let s = \"hi\"; }",
        "",
        "   ",
        "\tindented\twith\ttabs",
        "let s = \"日本語のテキスト\";",
        "// emoji 🎉 and a combining é",
        "let s = \"unterminated",
        "still inside it",
        "closed\"; // done",
        "**bold** and `code` and [a link](http://example.com)",
        "- a list item",
        "```rust",
        "let inside_a_fence = 1;",
        "```",
    ];

    /// Every segment set must reproduce its line exactly. A gap or an overlap
    /// here is dropped or duplicated characters on screen — the renderer
    /// displays what the segments say, not the line.
    #[test]
    fn segments_reproduce_the_line_exactly() {
        for path in ["a.rs", "notes.md", "no_extension", "x.unknownext"] {
            let mut samples: Vec<String> = AWKWARD.iter().map(|s| s.to_string()).collect();
            samples.push("x".repeat(MAX_HIGHLIGHT_LINE_LEN + 5));
            let lines: Vec<DiffLine> = samples.iter().map(|s| line(s, path)).collect();

            let mut cache = HighlightCache::new();
            for (i, sample) in samples.iter().enumerate() {
                let segments = cache.segments(&lines, i);
                assert_eq!(
                    text_of(&segments),
                    *sample,
                    "segments did not reproduce line {i} of {path}"
                );
            }
        }
    }

    /// The deletion side has to reproduce its text too — it is sliced by the
    /// inline-diff spans, so a short segment set silently drops characters.
    #[test]
    fn deletion_side_segments_reproduce_their_text_exactly() {
        let lines: Vec<DiffLine> = AWKWARD.iter().map(|s| line(s, "notes.md")).collect();
        let mut cache = HighlightCache::new();
        for (i, sample) in AWKWARD.iter().enumerate() {
            let segments = cache.alt_segments(&lines, i, sample);
            assert_eq!(text_of(&segments), *sample, "deletion side dropped text on line {i}");
        }
    }

    /// The cache replaced a renderer that walked the same lines through
    /// `syntax::highlight_line`. For unchanged lines anchored at the top of a
    /// file, the two must agree exactly — any divergence is a colour change
    /// users would see, and should be deliberate rather than discovered.
    #[test]
    fn matches_the_streaming_highlighter_it_replaced() {
        for path in ["a.rs", "notes.md"] {
            let lines: Vec<DiffLine> = AWKWARD.iter().map(|s| line(s, path)).collect();

            crate::syntax::reset_highlight_state();
            let expected: Vec<Vec<SyntaxSegment>> = AWKWARD
                .iter()
                .map(|s| crate::syntax::highlight_line(s, Some(path)))
                .collect();

            let mut cache = HighlightCache::new();
            for (i, want) in expected.iter().enumerate() {
                let got = cache.segments(&lines, i);
                assert_eq!(
                    got.iter().map(|s| (s.text.clone(), s.fg_color)).collect::<Vec<_>>(),
                    want.iter().map(|s| (s.text.clone(), s.fg_color)).collect::<Vec<_>>(),
                    "line {i} of {path} is coloured differently than the renderer this replaced"
                );
            }
        }
    }

    use proptest::strategy::Strategy as _;

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

        /// The hand-picked cases above only cover what I thought to write down.
        /// Over arbitrary lines, read in the order a scroll reads them, the
        /// cache must still reproduce each line exactly and colour it exactly
        /// as the streaming highlighter it replaced.
        #[test]
        fn agrees_with_the_streaming_highlighter_over_arbitrary_lines(
            contents in proptest::collection::vec(
                // Built from fragments that open and close multi-line
                // constructs, so the generated files actually exercise the
                // carried parser state rather than inert prose.
                proptest::collection::vec(
                    proptest::sample::select(vec![
                        "\"", "'", "`", "/*", "*/", "//", "```", "```rust",
                        "#", "<!--", "-->", "let x = 1;", "fn f() {", "}",
                        "   ", "\t", "日本語", "🎉", "- item", "",
                    ]),
                    0..6usize,
                ).prop_map(|parts| parts.concat()),
                1..20usize,
            )
        ) {
            // Rust and Markdown alone are not enough: several behaviours here
            // are invisible in them and only show up in JavaScript, Python or
            // SQL. Covering one newline-insensitive and three sensitive
            // languages is what makes this property load-bearing.
            for path in ["a.rs", "notes.md", "s.js", "p.py", "q.sql"] {
                let lines: Vec<DiffLine> = contents.iter().map(|s| line(s, path)).collect();

                crate::syntax::reset_highlight_state();
                let expected: Vec<Vec<SyntaxSegment>> = contents
                    .iter()
                    .map(|s| crate::syntax::highlight_line(s, Some(path)))
                    .collect();

                let mut cache = HighlightCache::new();
                for (i, want) in expected.iter().enumerate() {
                    let got = cache.segments(&lines, i);
                    proptest::prop_assert_eq!(
                        text_of(&got),
                        contents[i].clone(),
                        "segments did not reproduce line {} of {}", i, path
                    );
                    proptest::prop_assert_eq!(
                        got.iter().map(|s| (s.text.clone(), s.fg_color)).collect::<Vec<_>>(),
                        want.iter().map(|s| (s.text.clone(), s.fg_color)).collect::<Vec<_>>(),
                        "line {} of {} coloured differently than the renderer this replaced", i, path
                    );
                }
            }
        }
    }

    /// `advance` hands each line to the parser with its newline, so constructs
    /// that end at end-of-line actually close. JavaScript is used deliberately:
    /// dropping the newline provably changes nothing in Rust or Markdown, so a
    /// test written in those languages passes either way and pins nothing.
    #[test]
    fn a_line_terminated_construct_does_not_bleed_into_the_lines_below() {
        let lines = vec![
            line("const s = \"unterminated", "test.js"),
            line("const AFTER = [];", "test.js"),
        ];

        let mut cache = HighlightCache::new();
        let opener = cache.segments(&lines, 0);
        let after = cache.segments(&lines, 1);

        let string_colour = opener
            .iter()
            .find(|s| s.text.contains("unterminated"))
            .expect("the opening line should have string content")
            .fg_color;

        assert!(
            after.iter().any(|s| s.fg_color != string_colour),
            "the line below is entirely coloured as the unterminated string: {after:?}"
        );
    }

    #[test]
    fn highlights_a_line() {
        let lines = vec![line("fn main() {}", "a.rs")];
        let mut cache = HighlightCache::new();
        let segments = cache.segments(&lines, 0);
        assert!(segments.iter().any(|s| s.text == "fn"));
    }

    #[test]
    fn repeat_lookup_returns_the_cached_arc() {
        let lines = vec![line("fn main() {}", "a.rs")];
        let mut cache = HighlightCache::new();
        let first = cache.segments(&lines, 0);
        let second = cache.segments(&lines, 0);
        assert!(
            Arc::ptr_eq(&first, &second),
            "second lookup re-parsed instead of hitting the cache"
        );
    }

    #[test]
    fn scrolling_forward_parses_only_the_newly_exposed_line() {
        let lines: Vec<DiffLine> = (0..50).map(|i| line(&format!("let x{i} = {i};"), "a.rs")).collect();
        let mut cache = HighlightCache::new();

        // Prime a window, as a first frame would.
        for i in 0..40 {
            cache.segments(&lines, i);
        }
        let before: Vec<_> = (0..40).map(|i| cache.segments(&lines, i)).collect();

        // The next row down: only line 40 is new.
        cache.segments(&lines, 40);

        for (i, segments) in before.iter().enumerate() {
            assert!(
                Arc::ptr_eq(segments, &cache.segments(&lines, i)),
                "line {i} was re-parsed by a one-row scroll"
            );
        }
    }

    /// Scrolling down through a file chains its lines, so a construct that
    /// spans several of them keeps colouring past its first line.
    #[test]
    fn state_carries_across_lines_scrolled_through_in_order() {
        // The string opens on one line and closes on the next; the middle line
        // is only coloured as a string if the parser state carried over.
        let lines = vec![
            line("let s = \"open", "a.rs"),
            line("still inside the string", "a.rs"),
            line("closed\";", "a.rs"),
        ];
        let mut cache = HighlightCache::new();
        cache.segments(&lines, 0);
        let middle = cache.segments(&lines, 1);

        let plain = {
            let solo = vec![line("still inside the string", "a.rs")];
            HighlightCache::new().segments(&solo, 0)
        };
        assert_ne!(
            middle.iter().map(|s| s.fg_color).collect::<Vec<_>>(),
            plain.iter().map(|s| s.fg_color).collect::<Vec<_>>(),
            "continuation line was not coloured in its predecessor's context"
        );
    }

    /// Landing cold in the middle of a file must not drag its predecessors
    /// through the parser. Doing so is unbounded work for a line nobody asked
    /// to see — the line gets a fresh parser instead, as it did before this
    /// cache existed.
    #[test]
    fn a_cold_jump_parses_only_the_line_it_landed_on() {
        let lines: Vec<DiffLine> = (0..500)
            .map(|i| line(&format!("let x{i} = {i};"), "a.rs"))
            .collect();

        let mut cache = HighlightCache::new();
        cache.segments(&lines, 400);

        assert_eq!(
            cache.parse_count(),
            1,
            "the jump parsed lines behind the one it landed on"
        );
    }

    /// Once the earlier lines have been visited, their context is picked up for
    /// free — no re-parsing, and the answer matches having scrolled through.
    #[test]
    fn context_is_inherited_from_already_cached_lines() {
        let lines = vec![
            line("/* block comment opens", "a.rs"),
            line("still commented", "a.rs"),
            line("closes */ let x = 1;", "a.rs"),
        ];

        let mut cache = HighlightCache::new();
        cache.segments(&lines, 0);
        cache.segments(&lines, 1);
        let after_scrolling = cache.segments(&lines, 2);

        // Cold, the same line has no predecessor to inherit from.
        let cold = HighlightCache::new().segments(&lines, 2);

        assert_ne!(
            after_scrolling.iter().map(|s| s.fg_color).collect::<Vec<_>>(),
            cold.iter().map(|s| s.fg_color).collect::<Vec<_>>(),
            "scrolling in gained no context over landing cold"
        );
        assert_eq!(cache.parse_count(), 3, "inheriting context re-parsed something");
    }

    #[test]
    fn a_files_chain_does_not_leak_into_the_next() {
        let lines = vec![
            line("let s = \"unterminated", "a.rs"),
            line("fn main() {}", "b.rs"),
        ];
        let mut cache = HighlightCache::new();
        let second = cache.segments(&lines, 1);

        let alone = HighlightCache::new().segments(&[line("fn main() {}", "b.rs")], 0);
        assert_eq!(
            second.iter().map(|s| s.fg_color).collect::<Vec<_>>(),
            alone.iter().map(|s| s.fg_color).collect::<Vec<_>>(),
            "the previous file's open string bled across the file boundary"
        );
    }

    /// End-to-end: a header above a line must not change how that line is
    /// coloured. Note this asserts the *outcome*, and cannot on its own prove
    /// the header never reached the parser — a header's path parses to a
    /// benign state, so these colours match either way. The mechanism is
    /// pinned by `chrome_lines_are_never_handed_to_the_parser`.
    #[test]
    fn a_header_does_not_change_the_colours_below_it() {
        let mut header = DiffLine::file_header("a.rs");
        header.file_path = Some("a.rs".to_string());
        let lines = vec![header, line("fn main() {}", "a.rs")];

        let mut cache = HighlightCache::new();
        cache.segments(&lines, 0);
        let after_header = cache.segments(&lines, 1);
        let alone = HighlightCache::new().segments(&[line("fn main() {}", "a.rs")], 0);

        assert_eq!(
            after_header.iter().map(|s| s.fg_color).collect::<Vec<_>>(),
            alone.iter().map(|s| s.fg_color).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn over_long_lines_are_not_parsed() {
        let huge = "a".repeat(MAX_HIGHLIGHT_LINE_LEN + 1);
        let lines = vec![line(&huge, "a.rs")];
        let mut cache = HighlightCache::new();
        let segments = cache.segments(&lines, 0);
        assert_eq!(segments.len(), 1);
        // Asserted on the parser, not the colour: `DEFAULT_FG` is the same
        // RGB the theme gives unstyled text, so a colour check here passes
        // whether or not the line was parsed.
        assert_eq!(cache.parse_count(), 0, "an over-long line was parsed");
    }

    /// The threshold is the largest length still worth parsing, and the line
    /// one byte past it is the first that is skipped. Both halves need pinning
    /// separately or a comparison can drift by one in either direction without
    /// any test noticing.
    #[test]
    fn the_length_threshold_includes_the_line_that_sits_exactly_on_it() {
        let at = "a".repeat(MAX_HIGHLIGHT_LINE_LEN);
        let over = "a".repeat(MAX_HIGHLIGHT_LINE_LEN + 1);
        let lines = vec![line(&at, "a.rs"), line(&over, "a.rs")];

        let mut cache = HighlightCache::new();
        cache.segments(&lines, 0);
        assert_eq!(
            cache.parse_count(),
            1,
            "a line exactly at the threshold was skipped"
        );
        cache.segments(&lines, 1);
        assert_eq!(
            cache.parse_count(),
            1,
            "a line one byte past the threshold was parsed"
        );
    }

    /// Chrome — file headers, elision markers, image placeholders — carries
    /// text that is not source. Asserted on the parser rather than on the
    /// colours below it: a header's path parses as a harmless identifier, so
    /// the lines after it come out identical whether or not it was fed in.
    #[test]
    fn chrome_lines_are_never_handed_to_the_parser() {
        let mut header = DiffLine::file_header("a.rs");
        header.file_path = Some("a.rs".to_string());

        let mut elided = DiffLine::new(LineSource::Elided, "42 lines hidden".into(), ' ', None);
        elided.file_path = Some("a.rs".to_string());

        let mut image = DiffLine::new(LineSource::Base, "[image]".into(), ' ', None);
        image.file_path = Some("a.rs".to_string());

        let lines = vec![header, elided, image];
        let mut cache = HighlightCache::new();
        for i in 0..lines.len() {
            cache.segments(&lines, i);
        }

        assert_eq!(cache.parse_count(), 0, "chrome was parsed as source");
    }

    #[test]
    fn over_long_lines_do_not_disturb_the_lines_after_them() {
        let huge = "a".repeat(MAX_HIGHLIGHT_LINE_LEN + 1);
        let lines = vec![line(&huge, "a.rs"), line("fn main() {}", "a.rs")];
        let mut cache = HighlightCache::new();
        let after = cache.segments(&lines, 1);
        assert!(after.iter().any(|s| s.text == "fn"));
    }

    #[test]
    fn empty_lines_produce_no_segments() {
        let lines = vec![line("", "a.rs")];
        assert!(HighlightCache::new().segments(&lines, 0).is_empty());
    }

    /// Blank lines must never reach the parser. Markdown costs ~88x more per
    /// line when they do, which is exactly the prose content this cache exists
    /// to make scrollable.
    #[test]
    fn blank_lines_are_never_parsed() {
        let lines = vec![
            line("# Heading", "notes.md"),
            line("", "notes.md"),
            line("Some prose.", "notes.md"),
            line("", "notes.md"),
            line("More prose.", "notes.md"),
        ];

        let mut cache = HighlightCache::new();
        for i in 0..lines.len() {
            cache.segments(&lines, i);
        }

        assert_eq!(
            cache.parse_count(),
            3,
            "the two blank lines were handed to the parser"
        );
    }

    #[test]
    fn out_of_range_index_is_empty() {
        let lines = vec![line("fn main() {}", "a.rs")];
        assert!(HighlightCache::new().segments(&lines, 7).is_empty());
    }

    /// Re-reading an already-scrolled region is free, however large it is and
    /// whichever direction it is read in. (The *bound* on re-deriving context
    /// is pinned separately, by
    /// `the_longest_possible_context_walk_still_reaches_a_checkpoint`.)
    #[test]
    fn revisiting_cached_lines_parses_nothing() {
        let lines: Vec<DiffLine> = (0..400)
            .map(|i| line(&format!("let x{i} = {i};"), "a.rs"))
            .collect();

        let mut cache = HighlightCache::new();
        for i in 0..400 {
            cache.segments(&lines, i);
        }

        // Every line is cached, so re-reading any of them parses nothing,
        // however large the region or whichever direction it is read in.
        let settled = cache.parse_count();
        for i in (0..400).rev() {
            cache.segments(&lines, i);
        }
        assert_eq!(cache.parse_count(), settled);
    }

    /// Modified lines render both sides, so a scroll step over one asks for
    /// `segments` *and* `alt_segments`. That must stay a small constant — most
    /// lines in a real diff are modified, so any per-line replay here lands on
    /// the common path, not a rare one.
    #[test]
    fn scrolling_over_modified_lines_stays_at_a_constant_cost() {
        let lines: Vec<DiffLine> = (0..120)
            .map(|i| line(&format!("let x{i} = new_value_{i};"), "a.rs"))
            .collect();

        let mut cache = HighlightCache::new();
        // Prime a viewport's worth.
        for i in 0..40 {
            cache.segments(&lines, i);
            cache.alt_segments(&lines, i, &format!("let x{i} = old_value_{i};"));
        }

        let before = cache.parse_count();
        for i in 40..80 {
            cache.segments(&lines, i);
            cache.alt_segments(&lines, i, &format!("let x{i} = old_value_{i};"));
        }
        let per_line = (cache.parse_count() - before) as f64 / 40.0;

        assert!(
            per_line <= 3.0,
            "each newly exposed modified line cost {per_line} parses; \
             expected ~2 (the line itself plus its deletion side)"
        );
    }

    #[test]
    fn exit_states_are_kept_sparsely() {
        let lines: Vec<DiffLine> = (0..200).map(|i| line(&format!("let x{i} = {i};"), "a.rs")).collect();
        let mut cache = HighlightCache::new();
        for i in 0..200 {
            cache.segments(&lines, i);
        }

        let kept = cache.entries.iter().flatten().filter(|e| e.exit.is_some()).count();
        // 13 checkpoints across 200 lines on a 16-line grid, plus the two-line
        // frontier window. Written flat rather than derived from
        // `CHECKPOINT_EVERY`, so widening the grid cannot move the bound and
        // the expectation together.
        assert_eq!(
            kept, 15,
            "expected 15 stored exit states across 200 lines, found {kept}"
        );
    }

    #[test]
    fn deletion_side_is_coloured_in_the_context_its_line_was_entered_in() {
        // The deleted text sat inside a string that opens on the line above,
        // so it must be coloured as string contents — not as fresh code.
        let lines = vec![
            line("let s = \"open", "a.rs"),
            line("let x = 1;", "a.rs"),
        ];
        let mut cache = HighlightCache::new();
        cache.segments(&lines, 0);
        let inside_string = cache.alt_segments(&lines, 1, "let y = 2;");

        let solo = vec![line("let y = 2;", "a.rs")];
        let standalone = HighlightCache::new().alt_segments(&solo, 0, "let y = 2;");

        assert_ne!(
            inside_string.iter().map(|s| s.fg_color).collect::<Vec<_>>(),
            standalone.iter().map(|s| s.fg_color).collect::<Vec<_>>(),
            "deletion side ignored the context its line sat in"
        );
    }

    #[test]
    fn invalidate_drops_cached_segments() {
        let lines = vec![line("fn main() {}", "a.rs")];
        let mut cache = HighlightCache::new();
        let first = cache.segments(&lines, 0);
        cache.invalidate();
        let second = cache.segments(&lines, 0);
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(text_of(&first), text_of(&second));
    }

    #[test]
    fn a_change_in_line_count_drops_stale_entries() {
        let mut cache = HighlightCache::new();
        let before = vec![line("fn main() {}", "a.rs")];
        let first = cache.segments(&before, 0);

        let after = vec![line("fn other() {}", "a.rs"), line("let x = 1;", "a.rs")];
        let second = cache.segments(&after, 0);

        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(text_of(&second), "fn other() {}");
    }

    #[test]
    fn alt_segments_highlight_the_deletion_side() {
        let lines = vec![line("let x = 2;", "a.rs")];
        let mut cache = HighlightCache::new();
        let del = cache.alt_segments(&lines, 0, "let x = 1;");
        assert_eq!(text_of(&del), "let x = 1;");
        assert!(del.iter().any(|s| s.text == "let"));
    }

    #[test]
    fn alt_segments_do_not_advance_the_main_chain() {
        let lines = vec![line("let a = 1;", "a.rs"), line("fn main() {}", "a.rs")];
        let mut cache = HighlightCache::new();

        let expected = HighlightCache::new().segments(&lines, 1);
        cache.alt_segments(&lines, 0, "let s = \"unterminated");
        let actual = cache.segments(&lines, 1);

        assert_eq!(
            actual.iter().map(|s| s.fg_color).collect::<Vec<_>>(),
            expected.iter().map(|s| s.fg_color).collect::<Vec<_>>(),
            "deletion-side text leaked into the following line's context"
        );
    }

    #[test]
    fn a_change_in_line_count_drops_stale_alt_entries() {
        let mut cache = HighlightCache::new();
        let before = vec![line("let x = 2;", "a.rs")];
        cache.alt_segments(&before, 0, "let x = 1;");

        let after = vec![line("let y = 2;", "a.rs"), line("let z = 3;", "a.rs")];
        let second = cache.alt_segments(&after, 0, "let y = 1;");

        assert_eq!(
            text_of(&second),
            "let y = 1;",
            "the deletion side served text from the previous set of lines"
        );
    }

    #[test]
    fn alt_segments_are_cached() {
        let lines = vec![line("let x = 2;", "a.rs")];
        let mut cache = HighlightCache::new();
        let first = cache.alt_segments(&lines, 0, "let x = 1;");
        let second = cache.alt_segments(&lines, 0, "let x = 1;");
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn alt_segments_skip_over_long_content() {
        let huge = "a".repeat(MAX_HIGHLIGHT_LINE_LEN + 1);
        let lines = vec![line("let x = 2;", "a.rs")];
        let mut cache = HighlightCache::new();
        cache.segments(&lines, 0);
        let before = cache.parse_count();

        let del = cache.alt_segments(&lines, 0, &huge);
        assert_eq!(del.len(), 1);
        assert_eq!(
            cache.parse_count(),
            before,
            "over-long deletion text was parsed"
        );
    }

    /// The same threshold as the content side, pinned on the deletion side too
    /// — it is a separate comparison and drifted independently.
    #[test]
    fn the_deletion_side_threshold_includes_the_length_that_sits_exactly_on_it() {
        let at = "a".repeat(MAX_HIGHLIGHT_LINE_LEN);
        let over = "a".repeat(MAX_HIGHLIGHT_LINE_LEN + 1);
        let lines = vec![line("let a = 1;", "a.rs"), line("let b = 2;", "a.rs")];

        let mut cache = HighlightCache::new();
        cache.segments(&lines, 0);
        cache.segments(&lines, 1);
        let before = cache.parse_count();

        cache.alt_segments(&lines, 0, &at);
        assert_eq!(
            cache.parse_count(),
            before + 1,
            "deletion text exactly at the threshold was skipped"
        );

        cache.alt_segments(&lines, 1, &over);
        assert_eq!(
            cache.parse_count(),
            before + 1,
            "deletion text past the threshold was parsed"
        );
    }

    /// The worst case for re-deriving context: landing one line short of a
    /// checkpoint, so the walk back has to cross the entire grid spacing.
    ///
    /// This is what pins `MAX_CONTEXT_REPLAY`. The backstop is meant never to
    /// bind — the checkpoint grid already guarantees a stored state within
    /// `CHECKPOINT_EVERY` steps — so it is only observable if it is set *below*
    /// that guarantee, at which point the walk gives up early and colours the
    /// line from a fresh parser instead of its real context.
    #[test]
    fn the_longest_possible_context_walk_still_reaches_a_checkpoint() {
        let lines: Vec<DiffLine> = (0..400)
            .map(|i| line(&format!("let x{i} = {i};"), "a.rs"))
            .collect();

        let mut cache = HighlightCache::new();
        for i in 0..400 {
            cache.segments(&lines, i);
        }
        let before = cache.parse_count();

        // Line 47 sits 15 past the checkpoint at 32 — the furthest a walk can
        // ever be from one. Re-deriving its entry state replays lines 33..=46,
        // and the deleted text itself is the fifteenth parse. Written flat: if
        // it were derived from the constants, shrinking the backstop below the
        // grid spacing would move the expectation along with the behaviour.
        cache.alt_segments(&lines, 47, "let x47 = was;");

        assert_eq!(
            cache.parse_count() - before,
            15,
            "the walk back gave up before reaching the checkpoint behind it"
        );
    }

    /// Rendering a deletion side costs its own parse *plus* whatever context it
    /// has to re-derive. Both are counted, and both are asserted exactly:
    /// every other count assertion here is an upper bound, which a mutation
    /// that stops counting satisfies just as well.
    #[test]
    fn the_deletion_side_counts_its_own_parse_and_the_context_it_re_derives() {
        let lines: Vec<DiffLine> = (0..40)
            .map(|i| line(&format!("let x{i} = {i};"), "a.rs"))
            .collect();

        let mut cache = HighlightCache::new();
        for i in 0..40 {
            cache.segments(&lines, i);
        }
        let before = cache.parse_count();

        // Line 35 is cached but holds no exit state: it is neither a
        // checkpoint (32 is the nearest behind it) nor inside the two-line
        // frontier window left at 38/39. So reaching its entry state re-derives
        // lines 33 and 34, and the deleted text itself is the third parse.
        cache.alt_segments(&lines, 35, "let x35 = was;");

        assert_eq!(
            cache.parse_count() - before,
            3,
            "the deletion side did not count its re-derived context"
        );
    }
}
