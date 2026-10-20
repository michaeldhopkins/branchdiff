//! The guard of the Per-Frame Work ratchet: a source file that runs code on animation
//! frames (or on a short timer, or in a canvas) must be named in `MEASURED_PAGES`, whose
//! pages `frame_budget.rs` times in a real browser.

use std::path::{Path, PathBuf};

/// Every page whose code runs on animation frames, with what runs there. The guard fails
/// when a file in `src/` runs per-frame code that is not named here.
const MEASURED_PAGES: [(&str, &str); 1] =
    [("src/html_page.rs", "the live --html page: the settle loop after a refresh")];

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
fn production(text: &str) -> String {
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
fn timer_delays(text: &str) -> Vec<(String, Option<u64>)> {
    let shipped = production(text);
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
fn per_frame_work(text: &str) -> Option<String> {
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

/// Files that run per-frame work and are not named in `measured`, and entries of
/// `measured` that no longer run any.
fn unmeasured(files: &[(String, String)], measured: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    for (path, text) in files {
        if let Some(why) = per_frame_work(text)
            && !measured.contains(&path.as_str())
        {
            problems.push(format!("{path} {why} but no measured page names it"));
        }
    }
    for page in measured {
        let runs = files.iter().any(|(p, t)| p == page && per_frame_work(t).is_some());
        if !runs {
            problems.push(format!("{page} is listed as measured but runs no per-frame work"));
        }
    }
    problems
}

fn source_files(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src").flatten() {
        let path = entry.path();
        if path.is_dir() {
            source_files(&path, found);
        } else {
            found.push(path);
        }
    }
}

#[test]
fn every_source_file_with_per_frame_work_is_a_measured_page() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    source_files(&root.join("src"), &mut paths);
    assert!(paths.len() > 20, "the walk found only {} files in src/", paths.len());
    let files: Vec<(String, String)> = paths
        .iter()
        .map(|p| {
            let relative = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            (
                relative,
                String::from_utf8_lossy(&std::fs::read(p).expect("read source")).into_owned(),
            )
        })
        .collect();
    let measured: Vec<&str> = MEASURED_PAGES.iter().map(|(path, _)| *path).collect();
    let problems = unmeasured(&files, &measured);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_guard_sees_each_kind_of_per_frame_work_and_ignores_the_rest() {
    let file = |text: &str| vec![("src/x.rs".to_string(), text.to_string())];
    let flagged = |text: &str| !unmeasured(&file(text), &[]).is_empty();
    assert!(flagged("const J: &str = \"requestAnimationFrame(f)\";"));
    assert!(flagged("let c = \"<canvas id=a>\";"));
    assert!(flagged("const J: &str = \"ctx = c.getContext('2d')\";"));
    assert!(flagged("const J: &str = \"setInterval(() => {\\n step();\\n}, 16)\";"));
    assert!(flagged("const J: &str = \"setTimeout(step, 50)\";"));
    assert!(flagged("const J: &str = \"setInterval(step, delay)\";"), "unreadable delay");
    assert!(!flagged("const J: &str = \"setInterval(() => poll(), 2000)\";"), "slow timer");
    assert!(!flagged(
        "fn a() {}\n#[cfg(test)]\nmod tests { const J: &str = \"requestAnimationFrame\"; }"
    ));
    assert!(!flagged("fn plain() {}"));
    assert!(
        unmeasured(&file("const J: &str = \"requestAnimationFrame(f)\";"), &["src/x.rs"])
            .is_empty(),
        "a named page passes"
    );
    assert!(!unmeasured(&file("fn plain() {}"), &["src/x.rs"]).is_empty(), "a stale entry fails");
    assert!(!unmeasured(&[], &["src/gone.rs"]).is_empty(), "a missing file fails");
}

fn flagged_text(text: &str) -> bool {
    per_frame_work(text).is_some()
}

#[test]
fn only_the_bodies_of_test_modules_are_left_unscanned() {
    assert!(flagged_text("#[cfg(test)]\nuse x;\nconst J: &str = \"requestAnimationFrame(f)\";"));
    assert!(flagged_text(
        "#[cfg(test)]\nmod tests { fn a() { if x { } } }\nconst J: &str = \"requestAnimationFrame\";"
    ));
    assert!(flagged_text("#[cfg(test)]\nfn helper() { let j = \"requestAnimationFrame\"; }"));
    assert!(!flagged_text(
        "#[cfg(test)]\n#[allow(dead_code)]\nmod tests { const J: &str = \"requestAnimationFrame {\"; }\nfn ok() {}"
    ));
    assert!(!flagged_text(
        "#[cfg(test)]\nmod tests {\n    const J: &str = \"} requestAnimationFrame\";\n}"
    ));
}

#[test]
fn a_timer_is_judged_by_its_own_last_argument() {
    assert!(flagged_text("\"setInterval(() => { f(a, 500); }, 16)\""), "inner 500, delay 16");
    assert!(flagged_text("\"setTimeout(step)\""), "no delay");
    assert!(flagged_text("\"setTimeout(fn, 0)\""), "self-chaining");
    assert!(flagged_text("\"setInterval(step, delay)\""), "not a literal");
    assert!(flagged_text("\"setInterval(() => {\\n step();\\n}, 16)\""));
    assert!(!flagged_text("\"setInterval(() => xs.reduce((a,b)=>a+b,0), 2000)\""), "slow poll");
    assert!(!flagged_text("\"setTimeout(() => { go('a,)'); }, 100)\""), "exactly the floor");
}

#[test]
fn a_timer_call_may_have_whitespace_before_its_paren() {
    assert!(flagged_text("\"setTimeout (step, 16)\""));
    assert!(flagged_text("\"setInterval\n(step, 16)\""));
    assert!(!flagged_text("\"setInterval\n(step, 2000)\""));
}

#[test]
fn a_rust_line_continuation_inside_a_marker_is_joined() {
    assert!(flagged_text("const J: &str = \"requestAnimation\\\n      Frame(f)\";"));
}

#[test]
fn plain_text_sources_are_scanned_like_rust_ones() {
    let files = vec![
        ("src/a.js".to_string(), "requestAnimationFrame(f)".to_string()),
        ("src/b.html".to_string(), "<canvas id=a>".to_string()),
        ("src/c.css".to_string(), "/* setInterval(f, 16) */".to_string()),
    ];
    assert_eq!(unmeasured(&files, &[]).len(), 3);
}

#[test]
fn the_walk_reads_files_of_every_extension() {
    let dir = std::env::temp_dir().join(format!("bd-guard-walk-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    for name in ["a.rs", "sub/b.js", "sub/c.html", "d.css", "e"] {
        std::fs::write(dir.join(name), "x").unwrap();
    }
    let mut found = Vec::new();
    source_files(&dir, &mut found);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(found.len(), 5, "{found:?}");
}

#[test]
fn the_real_poll_is_a_two_second_timer_and_the_only_one() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(root.join("src/html_page.rs")).expect("read");
    assert_eq!(timer_delays(&text), vec![("setInterval(".to_string(), Some(2000))]);
    assert_eq!(per_frame_work(&text).as_deref(), Some("uses requestAnimationFrame"));
}

#[test]
fn every_kind_of_frame_marker_is_seen() {
    for marker in [
        "requestAnimationFrame(f)",
        "document.createElement('canvas')",
        "document.createElement(\\\"canvas\\\")",
        "document.createElement( 'canvas' )",
        "<canvas id=a>",
        "requestVideoFrameCallback(f)",
        "c.getContext('2d')",
        "CADisplayLink",
        "TimelineView",
        "MTKView",
    ] {
        assert!(flagged_text(&format!("const J: &str = \"{marker}\";")), "{marker} not flagged");
    }
}
