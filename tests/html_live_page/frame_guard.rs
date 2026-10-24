//! The guard of the Per-Frame Work ratchet: a source file that runs code on animation
//! frames (or on a short timer, or in a canvas) must be named in `MEASURED_PAGES`, whose
//! pages `frame_budget.rs` times in a real browser.

use std::path::{Path, PathBuf};

#[path = "../support/frame_scan.rs"]
mod frame_scan;

use frame_scan::{per_frame_work, timer_delays};

/// Every page whose code runs on animation frames, with what runs there. The guard fails
/// when a file in `src/` runs per-frame code that is not named here.
const MEASURED_PAGES: [(&str, &str); 1] =
    [("src/html_page.rs", "the live --html page: the settle loop after a refresh")];

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
fn a_rust_line_continuation_inside_a_timer_call_is_joined() {
    assert!(flagged_text("\"setTimeout\\\n    (step, 16)\""), "joined call, 16 ms");
    assert!(!flagged_text("\"setTimeout(step, \\\n    500)\""), "joined delay is readable");
    assert_eq!(
        timer_delays("\"setTimeout(step, \\\n    500)\""),
        vec![("setTimeout(".to_string(), Some(500))]
    );
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
