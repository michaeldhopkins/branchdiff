//! The Per-Frame Work ratchet for the live page: the code the page runs on animation
//! frames is timed from inside a headless Chrome, with the CPU slowed, at the largest
//! screens the page is made for, on a large diff.
//!
//! The only per-frame code is the settle loop after a live refresh (`hold` in
//! `src/html_page.rs`), which runs at most 12 frames. It is not a continuous loop, so the
//! rule's floor of 10 callbacks a second becomes: each refresh must run at least
//! `MIN_SETTLE_CALLBACKS` of them, and a refresh that ran none fails.

use super::{Browser, HELPERS, LAUNCH_BUDGET, exclusive, git, name, render_page, serve_steps};

use serde_json::Value;
use tempfile::TempDir;

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
/// Empties the log, runs `flip` (which makes the page swap its body in), then collects
/// the duration of every animation-frame callback in the `WINDOW_MS` that follow the first one.
/// every animation-frame callback in the `WINDOW_MS` that follow the first one.
fn settle_callbacks(
    browser: &mut Browser,
    flip: impl FnOnce(&mut Browser),
    marker: &str,
) -> Vec<f64> {
    browser.eval("frameLog.length = 0; true");
    flip(browser);
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
        per_refresh.push(settle_callbacks(&mut browser, |_| site.flip(), marker));
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
    // At most 12 callbacks a refresh: the nearest-rank p95 of 24 skips the slowest one, so
    // the total binds only through a single long callback, as here.
    let mut outlier = vec![0.1; 11];
    outlier.push(250.0);
    let calm = vec![0.1; 12];
    assert!(p95(&[outlier.clone(), calm.clone()].concat()) < P95_BUDGET_MS);
    assert!(over_budget(&frames(vec![outlier, calm])).is_some(), "one long callback");
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

#[test]
fn the_fixture_is_a_large_diff_with_long_files_among_it() {
    assert_eq!(FILES, 200);
    let long_files = (0..FILES).filter(|f| long(*f)).count();
    assert_eq!(long_files, 8);
    assert!(big_body(0, "").lines().count() > 1000 && big_body(1, "").lines().count() < 20);
}

#[test]
fn the_log_is_emptied_before_the_page_is_flipped_not_after() {
    let _alone = exclusive();
    let site = serve_steps(vec!["<html><body>start</body></html>".to_string()]);
    let mut browser = Browser::launch(LAUNCH_BUDGET);
    browser.before_each_document(TIME_CALLBACKS);
    browser.navigate(&site.address);
    browser.two_frames();
    browser.eval(HELPERS);
    // A callback logged by an earlier refresh's chain, still in the log.
    browser.eval("frameLog.push({ start: performance.now(), ms: 99 }); true");
    let mut seen_at_flip = None;
    let timings = settle_callbacks(
        &mut browser,
        |b| {
            seen_at_flip = Some(b.eval("frameLog.length"));
            b.eval("document.body.append('MARK'); requestAnimationFrame(() => {}); true");
        },
        "MARK",
    );
    assert_eq!(seen_at_flip, Some(Value::from(0)), "the log still held old callbacks at the flip");
    assert!(!timings.contains(&99.0), "an old callback was counted: {timings:?}");
}
