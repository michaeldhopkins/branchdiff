//! Drives the real `--html` page in headless Chrome, in real time, over the DevTools
//! protocol: the live refresh must keep the reader's place, and the theme toggle must
//! work when `localStorage` throws.
//!
//! The page is served by a tiny in-process server whose body flips to a second page on
//! request. Run with `--features browser-tests`; a missing Chrome is a failure, never a
//! skip, and every launch is bounded by a deadline that kills the browser it started.

mod browser;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use browser::devtools::Browser;
use serde_json::Value;
use tempfile::TempDir;

const FILES: usize = 40;
const READER: &str = "f18.txt";
/// The height a section that has not been laid out reports (`html_style.rs`).
const PLACEHOLDER_HEIGHT: f64 = 600.0;
const LAUNCH_BUDGET: Duration = Duration::from_secs(90);

#[path = "html_live_page/frame_budget.rs"]
mod frame_budget;

static ONE_BROWSER_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Held for a whole test. The frame-budget test slows the CPU and times callbacks, which
/// another browser running beside it would distort, so none run at once.
fn exclusive() -> MutexGuard<'static, ()> {
    ONE_BROWSER_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner)
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn render_page(dir: &Path) -> String {
    let path = std::env::var("PATH").expect("PATH is set");
    let out = Command::new(env!("CARGO_BIN_EXE_branchdiff"))
        .args([".", "--html", "--no-auto-fetch"])
        .current_dir(dir)
        .env_clear()
        .env("PATH", path)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run branchdiff --html");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("utf-8 page")
}

fn name(file: usize) -> String {
    format!("f{file:02}.txt")
}

fn body_of(file: usize, tweak: &str) -> String {
    (1..=10).map(|n| format!("line {n} of file {file}{tweak}\n")).collect()
}

/// A repository with `FILES` changed files, and the page for it before and after `edit`
/// changes the working tree.
fn two_pages(edit: impl FnOnce(&Path)) -> (String, String) {
    pages(|_| {}, edit)
}

/// Like `two_pages`, with `before` shaping the tree for the first page too.
fn pages(before: impl FnOnce(&Path), edit: impl FnOnce(&Path)) -> (String, String) {
    let dir = TempDir::new().expect("temp dir");
    git(dir.path(), &["init", "-q"]);
    for f in 0..FILES {
        std::fs::write(dir.path().join(name(f)), body_of(f, "")).unwrap();
    }
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "base"]);
    for f in 0..FILES {
        std::fs::write(dir.path().join(name(f)), body_of(f, " edited")).unwrap();
    }
    before(dir.path());
    let first = render_page(dir.path());
    edit(dir.path());
    (first, render_page(dir.path()))
}

fn swap_one_line(dir: &Path) {
    std::fs::write(dir.join(name(3)), body_of(3, " SWAPPED")).unwrap();
}

/// A page that serves each of its pages in turn, one more per `flip`.
struct Site {
    address: String,
    step: Arc<AtomicUsize>,
}

impl Site {
    fn flip(&self) {
        self.step.fetch_add(1, Ordering::SeqCst);
    }
}

fn serve(first: String, second: String) -> Site {
    serve_steps(vec![first, second])
}

/// Serves `pages[0]` until the first `flip`, `pages[1]` until the next, and the last
/// page from then on.
fn serve_steps(pages: Vec<String>) -> Site {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = format!("http://{}/", listener.local_addr().unwrap());
    let step = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&step);
    let pages = Arc::new(pages);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (pages, seen) = (Arc::clone(&pages), Arc::clone(&seen));
            std::thread::spawn(move || answer(stream, &pages, &seen));
        }
    });
    Site { address, step }
}

/// One connection a browser opened. A connection that never sends a request (Chrome
/// opens spare ones) times out here instead of holding anything up.
fn answer(mut stream: TcpStream, pages: &[String], step: &AtomicUsize) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = [0u8; 2048];
    let n = stream.read(&mut buf).unwrap_or(0);
    let request = String::from_utf8_lossy(&buf[..n]);
    let body = if request.starts_with("GET / ") {
        pages[step.load(Ordering::SeqCst).min(pages.len() - 1)].as_str()
    } else {
        ""
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

/// Helpers installed in the page: sections are found by the path in their summary.
const HELPERS: &str = r#"
window.bd = {
  sleep: ms => new Promise(r => setTimeout(r, ms)),
  frames: () => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))),
  section: path => [...document.querySelectorAll('details[id^="file-"]')]
    .find(e => e.querySelector('summary').firstChild.textContent.trim() === path),
  measure(path) {
    const s = bd.section(path);
    return {
      top: s.getBoundingClientRect().top,
      height: s.getBoundingClientRect().height,
      table: s.querySelector('table').getBoundingClientRect().height,
    };
  },
  async scrollPast(path, step = 250, limit = 600) {
    for (let i = 0; i < limit && bd.section(path).getBoundingClientRect().top > 200; i++) {
      scrollBy(0, step);
      await bd.frames();
    }
    scrollBy(0, bd.section(path).getBoundingClientRect().top + 50);
  },
  async waitFor(text) {
    for (let i = 0; i < 150 && !document.body.textContent.includes(text); i++) await bd.sleep(100);
    return document.body.textContent.includes(text);
  },
};
true"#;

fn number(value: &Value, key: &str) -> f64 {
    value[key].as_f64().unwrap_or_else(|| panic!("no {key} in {value}"))
}

/// Opens the first page of `site` with the helpers installed.
fn open(site: &Site) -> Browser {
    let mut browser = Browser::launch(LAUNCH_BUDGET);
    browser.navigate(&site.address);
    browser.two_frames();
    browser.eval(HELPERS);
    browser
}

/// Scrolls to the reader's section, lets it render, and returns its measurements.
fn settle_on_reader(browser: &mut Browser) -> Value {
    browser.eval(&format!("bd.scrollPast('{READER}')"));
    browser.two_frames();
    browser.eval(&format!("bd.measure('{READER}')"))
}

/// Flips the site, waits for the poll to swap the body in and two frames to run.
fn refresh(browser: &mut Browser, site: &Site, marker: &str) {
    site.flip();
    let swapped = browser.eval(&format!("bd.waitFor('{marker}')"));
    assert_eq!(swapped, true, "the poll never swapped the body in");
    browser.two_frames();
}

#[test]
fn a_live_refresh_keeps_the_section_the_reader_is_in() {
    let _alone = exclusive();
    let (first, second) = two_pages(swap_one_line);
    let site = serve(first, second);
    let mut browser = open(&site);
    let before = settle_on_reader(&mut browser);
    let (height, table) = (number(&before, "height"), number(&before, "table"));
    assert!(
        table > 0.0 && (height - PLACEHOLDER_HEIGHT).abs() > 1.0,
        "the reader's section is still a placeholder: {before}"
    );

    refresh(&mut browser, &site, "SWAPPED");

    let after = browser.eval(&format!("bd.measure('{READER}')"));
    let (was, now) = (number(&before, "top"), number(&after, "top"));
    assert!(
        (now - was).abs() <= 4.0,
        "{READER} was {was}px from the top before the refresh and {now}px after"
    );
}

#[test]
fn a_live_refresh_keeps_the_same_file_when_one_is_inserted_before_it() {
    let _alone = exclusive();
    let (first, second) = two_pages(|dir| {
        std::fs::write(dir.join("f17a.txt"), body_of(99, " INSERTED")).unwrap();
    });
    let site = serve(first, second);
    let mut browser = open(&site);
    let before = settle_on_reader(&mut browser);
    assert!(number(&before, "table") > 0.0, "the reader's section is a placeholder: {before}");

    refresh(&mut browser, &site, "INSERTED");

    let after = browser.eval(&format!("bd.measure('{READER}')"));
    let (was, now) = (number(&before, "top"), number(&after, "top"));
    assert!(
        (now - was).abs() <= 4.0,
        "{READER} was {was}px from the top before a file was inserted above it and {now}px after"
    );
}

#[test]
fn a_reader_at_the_top_stays_at_the_top_when_the_file_list_grows() {
    let _alone = exclusive();
    let (first, second) = two_pages(|dir| {
        std::fs::write(dir.join("a00.txt"), body_of(98, " ADDED")).unwrap();
    });
    let site = serve(first, second);
    let mut browser = open(&site);
    assert_eq!(browser.eval("scrollY"), 0, "the page starts at the top");

    refresh(&mut browser, &site, "ADDED");

    assert_eq!(browser.eval("scrollY"), 0, "the refresh moved a reader who had not scrolled");
}

#[test]
fn a_live_refresh_keeps_the_horizontal_scroll_too() {
    let _alone = exclusive();
    // A file name with no break opportunity is wider than the window, so the page
    // itself scrolls sideways.
    let (first, second) = pages(
        |dir| std::fs::write(dir.join(format!("{}.txt", "w".repeat(200))), "wide\n").unwrap(),
        |dir| std::fs::write(dir.join(name(3)), body_of(3, " WIDE_SWAP")).unwrap(),
    );
    let site = serve(first, second);
    let mut browser = open(&site);
    settle_on_reader(&mut browser);
    browser.eval("scrollTo(300, scrollY)");
    browser.two_frames();
    assert_eq!(browser.eval("scrollX"), 300, "precondition: the page scrolls sideways");

    refresh(&mut browser, &site, "WIDE_SWAP");

    assert_eq!(browser.eval("scrollX"), 300, "the refresh lost the horizontal scroll");
}

#[test]
fn the_theme_toggle_works_when_local_storage_throws() {
    let _alone = exclusive();
    let (first, second) = two_pages(swap_one_line);
    let site = serve(first, second);
    let mut browser = Browser::launch(LAUNCH_BUDGET);
    browser.before_each_document(
        "Object.defineProperty(window,'localStorage',{get(){throw new Error('blocked')}})",
    );
    browser.navigate(&site.address);
    browser.two_frames();
    let seen = browser.eval(
        "(()=>{const before=document.documentElement.dataset.theme;
           document.querySelector('.theme-toggle').click();
           return {before, after: document.documentElement.dataset.theme}})()",
    );
    assert_eq!(
        (seen["before"].as_str(), seen["after"].as_str()),
        (Some("dark"), Some("light")),
        "{seen}"
    );
}

/// Takes over `requestAnimationFrame`: callbacks queue until `bd.pump()` runs them, so a
/// test decides exactly what happens between two frames of the settle loop.
const QUEUE_FRAMES: &str = r#"
window.queued = [];
window.requestAnimationFrame = cb => window.queued.push(cb);
window.bd.pump = () => { const run = window.queued.splice(0); run.forEach(cb => cb(performance.now())); return window.queued.length; };
true"#;

/// A reader settled on `READER` and a refresh swapped in, with the settle loop's first
/// frame queued and not yet run.
fn refreshed_with_the_settle_frame_pending() -> Browser {
    let (first, second) = two_pages(swap_one_line);
    let site = serve(first, second);
    let mut browser = open(&site);
    settle_on_reader(&mut browser);
    browser.eval(QUEUE_FRAMES);
    site.flip();
    assert_eq!(browser.eval("bd.waitFor('SWAPPED')"), true, "the poll never swapped in");
    assert_eq!(browser.eval("queued.length"), 1, "the settle loop queued its first frame");
    browser
}

#[test]
fn a_settle_chain_whose_section_was_replaced_again_leaves_the_scroll_alone() {
    let _alone = exclusive();
    let mut browser = refreshed_with_the_settle_frame_pending();
    // A second refresh swaps the body again before the first one's frame runs: its
    // anchor is now a detached element, which measures as zero.
    browser.eval("document.body.innerHTML = document.body.innerHTML; scrollBy(0, 37); true");
    let scrolled = browser.eval("scrollY");

    browser.eval("bd.pump()");

    assert_eq!(browser.eval("scrollY"), scrolled, "a stale anchor chain moved the page");
}

#[test]
fn a_reader_who_scrolls_during_the_settle_is_not_pulled_back() {
    let _alone = exclusive();
    let mut browser = refreshed_with_the_settle_frame_pending();
    browser.eval("scrollBy(0, 123); dispatchEvent(new WheelEvent('wheel')); true");
    let scrolled = browser.eval("scrollY");

    let left = browser.eval("bd.pump()");

    assert_eq!(browser.eval("scrollY"), scrolled, "the settle loop fought the reader");
    assert_eq!(left, 0, "the chain went on after the reader took over");
}

#[test]
fn an_idle_settle_still_anchors_the_reader() {
    let _alone = exclusive();
    // The control for the two tests above: with no one touching the page, a frame that
    // finds the section moved scrolls it back.
    let mut browser = refreshed_with_the_settle_frame_pending();
    browser.eval("scrollBy(0, 123); true");
    let scrolled = number(&browser.eval("({y: scrollY})"), "y");

    browser.eval("bd.pump()");

    let now = number(&browser.eval("({y: scrollY})"), "y");
    assert!((now - scrolled).abs() > 50.0, "the frame did not re-anchor: {scrolled} -> {now}");
}
