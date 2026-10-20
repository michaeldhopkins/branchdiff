//! The Per-Frame Work ratchet for the live page: the code the page runs on animation
//! frames is timed from inside a headless Chrome, with the CPU slowed, at the largest
//! screens the page is made for, on a large diff.
//!
//! The only per-frame code is the settle loop after a live refresh (`hold` in
//! `src/html_page.rs`), which runs at most 12 frames. It is not a continuous loop, so the
//! rule's floor of 10 callbacks a second becomes: each refresh must run at least
//! `MIN_SETTLE_CALLBACKS` of them, and a refresh that ran none fails.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tempfile::TempDir;

use super::{Browser, HELPERS, LAUNCH_BUDGET, exclusive, git, name, render_page, serve_steps};

/// Every page whose code runs on animation frames, with what runs there. The guard fails
/// when a file in `src/` runs per-frame code that is not named here.
const MEASURED_PAGES: [(&str, &str); 1] =
    [("src/html_page.rs", "the live --html page: the settle loop after a refresh")];

const CPU_SLOWDOWN: u32 = 6;
/// iPad Pro 13" in portrait, and a desktop window, both at 2x.
const SCREENS: [(&str, u32, u32); 2] =
    [("iPad Pro 13in portrait", 1032, 1376), ("2560x1440 desktop", 2560, 1440)];
const SCALE: u32 = 2;

const FILES: usize = 200;
const LONG_EVERY: usize = 25;
const LONG_LINES: usize = 1200;
const SHORT_LINES: usize = 12;
const BIG_READER: &str = "f100.txt";
/// Pixels per step while scrolling down to the reader through 200 sections.
const SCROLL_STEP: u32 = 1500;

/// The window each refresh is measured over, from its first settle callback.
const WINDOW_MS: f64 = 1000.0;
const REFRESHES: usize = 2;
const P95_BUDGET_MS: f64 = 4.0;
const TOTAL_BUDGET_SHARE: f64 = 0.10;
const MIN_SETTLE_CALLBACKS: usize = 2;

/// Wraps `requestAnimationFrame` so every callback is timed from inside the page.
const TIME_CALLBACKS: &str = r#"
(() => {
  const native = window.requestAnimationFrame.bind(window);
  window.frameLog = [];
  window.requestAnimationFrame = callback => native(stamp => {
    const start = performance.now();
    try { callback(stamp); } finally { window.frameLog.push({ start, ms: performance.now() - start }); }
  });
})()"#;

fn long(file: usize) -> bool {
    file.is_multiple_of(LONG_EVERY)
}

fn big_body(file: usize, tweak: &str) -> String {
    let lines = if long(file) { LONG_LINES } else { SHORT_LINES };
    (1..=lines).map(|n| format!("line {n} of file {file}{tweak}\n")).collect()
}

/// A 200-file diff, eight of the files 1,200 lines long, and the three pages a reader
/// sees: the diff, the diff with one line swapped (same file count), and that with a file
/// inserted ahead of the reader.
fn large_diff_pages() -> Vec<String> {
    let dir = TempDir::new().expect("temp dir");
    git(dir.path(), &["init", "-q"]);
    let write_all = |tweak: &str| {
        for f in 0..FILES {
            std::fs::write(dir.path().join(name(f)), big_body(f, tweak)).unwrap();
        }
    };
    write_all("");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "base"]);
    write_all(" edited");
    let first = render_page(dir.path());
    std::fs::write(dir.path().join(name(3)), big_body(3, " SWAPPED")).unwrap();
    let swapped = render_page(dir.path());
    std::fs::write(dir.path().join("f099a.txt"), big_body(99, " INSERTED")).unwrap();
    vec![first, swapped, render_page(dir.path())]
}

/// The nearest-rank 95th percentile.
fn p95(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (sorted.len() as f64 * 0.95).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// What one screen measured, over both refreshes.
struct Frames {
    per_refresh: Vec<Vec<f64>>,
}

impl Frames {
    fn all(&self) -> Vec<f64> {
        self.per_refresh.concat()
    }
}

/// Flips the site, then collects the duration of every animation-frame callback in the
/// `WINDOW_MS` that follow the first one.
fn settle_callbacks(browser: &mut Browser, marker: &str) -> Vec<f64> {
    browser.eval("frameLog.length = 0; true");
    assert_eq!(browser.eval(&format!("bd.waitFor('{marker}')")), true, "{marker} never arrived");
    let timings = browser.eval(&format!(
        "(async () => {{
           for (let i = 0; i < 100 && !frameLog.length; i++) await bd.sleep(50);
           const t0 = frameLog.length ? frameLog[0].start : performance.now();
           while (performance.now() < t0 + {WINDOW_MS}) await bd.sleep(25);
           return frameLog.filter(f => f.start < t0 + {WINDOW_MS}).map(f => f.ms);
         }})()"
    ));
    timings.as_array().expect("an array of timings").iter().filter_map(Value::as_f64).collect()
}

fn measure_screen(pages: &[String], width: u32, height: u32) -> Frames {
    let site = serve_steps(pages.to_vec());
    let mut browser = Browser::launch(LAUNCH_BUDGET);
    browser.before_each_document(TIME_CALLBACKS);
    browser.set_screen(width, height, SCALE);
    browser.navigate(&site.address);
    browser.two_frames();
    browser.eval(HELPERS);
    browser.eval(&format!("bd.scrollPast('{BIG_READER}', {SCROLL_STEP}, 3000)"));
    browser.two_frames();
    browser.throttle_cpu(CPU_SLOWDOWN);

    let mut per_refresh = Vec::new();
    for marker in ["SWAPPED", "INSERTED"] {
        site.flip();
        per_refresh.push(settle_callbacks(&mut browser, marker));
    }
    Frames { per_refresh }
}

fn report(screen: &str, frames: &Frames) -> String {
    let all = frames.all();
    let counts: Vec<usize> = frames.per_refresh.iter().map(Vec::len).collect();
    format!(
        "{screen}: {} callbacks {counts:?}, p95 {:.2} ms, max {:.2} ms, total {:.2} ms of {} ms",
        all.len(),
        p95(&all),
        all.iter().copied().fold(0.0, f64::max),
        all.iter().sum::<f64>(),
        WINDOW_MS as usize * REFRESHES,
    )
}

/// The verdict on one screen's callbacks; `None` when within every budget.
fn over_budget(frames: &Frames) -> Option<String> {
    for (refresh, timings) in frames.per_refresh.iter().enumerate() {
        if timings.len() < MIN_SETTLE_CALLBACKS {
            return Some(format!(
                "refresh {refresh} ran {} settle callbacks, fewer than {MIN_SETTLE_CALLBACKS}: \
                 nothing was measured",
                timings.len()
            ));
        }
    }
    let all = frames.all();
    if p95(&all) > P95_BUDGET_MS {
        return Some(format!("95th percentile over {P95_BUDGET_MS} ms"));
    }
    let allowed = TOTAL_BUDGET_SHARE * WINDOW_MS * REFRESHES as f64;
    (all.iter().sum::<f64>() > allowed).then(|| format!("callbacks together over {allowed} ms"))
}

#[test]
fn the_settle_loop_stays_in_its_frame_budget_on_the_largest_screens_with_the_cpu_slowed() {
    let _alone = exclusive();
    let pages = large_diff_pages();
    let mut failures = Vec::new();
    for (screen, width, height) in SCREENS {
        let frames = measure_screen(&pages, width, height);
        let line = report(screen, &frames);
        eprintln!("frame budget, CPU x{CPU_SLOWDOWN}: {line}");
        if let Some(verdict) = over_budget(&frames) {
            failures.push(format!("{line}: {verdict}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_budget_check_rejects_a_slow_frame_a_heavy_total_and_a_refresh_that_never_ran() {
    let frames = |per_refresh: Vec<Vec<f64>>| Frames { per_refresh };
    assert_eq!(over_budget(&frames(vec![vec![0.5, 0.5], vec![0.5, 0.5]])), None);
    assert!(over_budget(&frames(vec![vec![0.5, 4.5], vec![0.5, 0.5]])).is_some(), "slow frame");
    let heavy = vec![3.9; 30];
    assert!(over_budget(&frames(vec![heavy.clone(), heavy])).is_some(), "heavy total");
    assert!(over_budget(&frames(vec![vec![0.1, 0.1], vec![0.1]])).is_some(), "too few callbacks");
    assert!(over_budget(&frames(vec![vec![], vec![0.1, 0.1]])).is_some(), "none ran");
}

#[test]
fn the_95th_percentile_is_the_nearest_rank() {
    let twenty: Vec<f64> = (1..=20).map(f64::from).collect();
    assert_eq!(p95(&twenty), 19.0);
    assert_eq!(p95(&[7.0]), 7.0);
    assert_eq!(p95(&[3.0, 1.0, 2.0]), 3.0);
}

// The guard.

const FRAME_MARKERS: [&str; 6] =
    ["requestAnimationFrame", "<canvas", "getContext(", "CADisplayLink", "TimelineView", "MTKView"];
const TIMER_CALLS: [&str; 2] = ["setInterval(", "setTimeout("];
const SHORTEST_PLAIN_TIMER_MS: u64 = 100;

/// The text before the file's test module, which is what ships.
fn production(text: &str) -> &str {
    text.split("#[cfg(test)]").next().unwrap_or(text)
}

/// The first `, <digits>)` after `from`: the delay of a timer call.
fn delay_after(text: &str, from: usize) -> Option<u64> {
    let rest = &text.as_bytes()[from..];
    (0..rest.len()).filter(|&i| rest[i] == b',').find_map(|comma| {
        let mut at = comma + 1;
        while rest.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        let digits = rest[at..].iter().take_while(|b| b.is_ascii_digit()).count();
        let mut end = at + digits;
        while rest.get(end).is_some_and(u8::is_ascii_whitespace) {
            end += 1;
        }
        (digits > 0 && rest.get(end) == Some(&b')'))
            .then(|| text[from + at..from + at + digits].parse().ok())?
    })
}

/// Why a source file runs per-frame work, or `None`. A timer counts unless its delay can
/// be read and is at least `SHORTEST_PLAIN_TIMER_MS`.
fn per_frame_work(text: &str) -> Option<String> {
    let shipped = production(text);
    if let Some(marker) = FRAME_MARKERS.iter().find(|m| shipped.contains(**m)) {
        return Some(format!("uses {marker}"));
    }
    for call in TIMER_CALLS {
        for (at, _) in shipped.match_indices(call) {
            match delay_after(shipped, at + call.len()) {
                Some(ms) if ms >= SHORTEST_PLAIN_TIMER_MS => {}
                Some(ms) => return Some(format!("runs a {call}) every {ms} ms")),
                None => return Some(format!("has a {call}) whose delay cannot be read")),
            }
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
        } else if path.extension().is_some_and(|e| e == "rs") {
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
            (relative, std::fs::read_to_string(p).expect("read source"))
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

#[test]
fn the_fixture_is_a_large_diff_with_long_files_among_it() {
    assert_eq!(FILES, 200);
    let long_files = (0..FILES).filter(|f| long(*f)).count();
    assert_eq!(long_files, 8);
    assert!(big_body(0, "").lines().count() > 1000 && big_body(1, "").lines().count() < 20);
}
