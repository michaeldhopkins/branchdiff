//! Shared per-frame work and production-code scanner for `html_live_page` and `perf_rules`.

/// Markers of per-frame work. They are looked for with whitespace removed and `\"` read
/// as `"`, so a call spread over lines, or written inside a Rust string, still matches.
const FRAME_MARKERS: [&str; 9] = [
    "requestAnimationFrame",
    "requestVideoFrameCallback",
    "createElement('canvas')",
    "createElement(\"canvas\")",
    "<canvas",
    "getContext(",
    "CADisplayLink",
    "TimelineView",
    "MTKView",
];
const TIMER_CALLS: [&str; 2] = ["setInterval", "setTimeout"];
const SHORTEST_PLAIN_TIMER_MS: u64 = 100;
const CFG_TEST: &str = "#[cfg(test)]";

/// The index just past the Rust string, raw string, character literal or comment that
/// starts at `at`, or `None` when none starts there.
fn past_literal(text: &str, at: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let next = bytes.get(at + 1).copied();
    match bytes[at] {
        b'/' if next == Some(b'/') => Some(text[at..].find('\n').map_or(text.len(), |n| at + n)),
        b'/' if next == Some(b'*') => {
            Some(text[at..].find("*/").map_or(text.len(), |n| at + n + 2))
        }
        b'"' => {
            let mut i = at + 1;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => return Some(i + 1),
                    _ => i += 1,
                }
            }
            Some(bytes.len())
        }
        b'r' if at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_') => {
            let hashes = bytes[at + 1..].iter().take_while(|b| **b == b'#').count();
            (bytes.get(at + 1 + hashes) == Some(&b'"')).then(|| {
                let close = format!("\"{}", "#".repeat(hashes));
                text[at + 2 + hashes..]
                    .find(&close)
                    .map_or(text.len(), |n| at + 2 + hashes + n + close.len())
            })
        }
        b'\'' if next == Some(b'\\') => text[at + 2..].find('\'').map(|n| at + 2 + n + 1),
        b'\'' => {
            let ch = text[at + 1..].chars().next()?;
            (bytes.get(at + 1 + ch.len_utf8()) == Some(&b'\'')).then_some(at + 2 + ch.len_utf8())
        }
        _ => None,
    }
}

/// The index just past the brace that closes the one at `open`, skipping literals.
fn past_block(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let (mut depth, mut i) = (0usize, open);
    while i < bytes.len() {
        if let Some(next) = past_literal(text, i) {
            i = next;
            continue;
        }
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Where the `mod name { … }` that follows a `#[cfg(test)]` ends, or `None` when what
/// follows (from `from`) is not a module with a body, such as a `use` or a function.
fn test_module_end(text: &str, from: usize) -> Option<usize> {
    let mut i = from;
    loop {
        let rest = text[i..].trim_start();
        i = text.len() - rest.len();
        if !rest.starts_with("#[") {
            break;
        }
        i += rest.find(']')? + 1;
    }
    let header_end = text[i..].find(['{', ';'])? + i;
    let is_module = text[i..header_end].split_whitespace().any(|word| word == "mod");
    (is_module && text.as_bytes()[header_end] == b'{').then(|| past_block(text, header_end))?
}

/// What ships: the text with the body of every `#[cfg(test)] mod … { }` removed. A
/// `#[cfg(test)]` on anything else removes nothing, and what follows it is scanned.
pub fn production(text: &str) -> String {
    let (mut kept, mut at) = (String::new(), 0);
    while let Some(found) = text[at..].find(CFG_TEST) {
        let attribute = at + found;
        let after = attribute + CFG_TEST.len();
        if let Some(end) = test_module_end(text, after) {
            kept.push_str(&text[at..attribute]);
            at = end;
        } else {
            kept.push_str(&text[at..after]);
            at = after;
        }
    }
    kept.push_str(&text[at..]);
    kept
}

/// The delay literal of the timer call whose argument list starts at `from`, just past
/// its opening parenthesis: its last top-level argument, found by counting brackets.
/// `None` when the call has one argument, or its last is not a plain number.
fn timer_delay(text: &str, from: usize) -> Option<u64> {
    let bytes = text.as_bytes();
    let (mut depth, mut arguments, mut last_start, mut i) = (0usize, 1, from, from);
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            quote @ (b'\'' | b'`') => {
                if let Some(n) = text[i + 1..].find(quote as char) {
                    i += n + 1;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth > 0 => depth -= 1,
            b')' => {
                let last = text[last_start..i].trim();
                let literal = last.bytes().all(|b| b.is_ascii_digit()) && !last.is_empty();
                return (arguments > 1 && literal).then(|| last.parse().ok())?;
            }
            b',' if depth == 0 => {
                arguments += 1;
                last_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Every timer call that ships, with its delay when it can be read.
pub fn timer_delays(text: &str) -> Vec<(String, Option<u64>)> {
    let shipped = strip_line_continuations(&production(text));
    let mut found: Vec<(usize, String, Option<u64>)> = Vec::new();
    for call in TIMER_CALLS {
        for (at, _) in shipped.match_indices(call) {
            let after = at + call.len();
            let rest = shipped[after..].trim_start();
            if rest.starts_with('(') {
                let open = shipped.len() - rest.len() + 1;
                found.push((at, format!("{call}("), timer_delay(&shipped, open)));
            }
        }
    }
    found.sort();
    found.into_iter().map(|(_, call, delay)| (call, delay)).collect()
}

/// A Rust string's `\` before a newline joins the lines and drops the next line's
/// leading whitespace; the markers are matched on the joined text.
fn strip_line_continuations(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("\\\n") {
        out.push_str(&rest[..at]);
        rest = rest[at + 2..].trim_start();
    }
    out.push_str(rest);
    out
}

/// Why a source file runs per-frame work, or `None`. A timer counts unless its delay can
/// be read and is at least `SHORTEST_PLAIN_TIMER_MS`.
pub fn per_frame_work(text: &str) -> Option<String> {
    let joined = strip_line_continuations(&production(text)).replace("\\\"", "\"");
    let compact: String = joined.chars().filter(|c| !c.is_whitespace()).collect();
    if let Some(marker) = FRAME_MARKERS.iter().find(|m| compact.contains(**m)) {
        return Some(format!("uses {marker}"));
    }
    for (call, delay) in timer_delays(text) {
        match delay {
            Some(ms) if ms >= SHORTEST_PLAIN_TIMER_MS => {}
            Some(ms) => return Some(format!("runs a {call}) every {ms} ms")),
            None => return Some(format!("has a {call}) whose delay cannot be read")),
        }
    }
    None
}
