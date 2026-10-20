//! Drives the real `--html` page in headless Chrome: the live refresh must keep the
//! reader's place, and the theme toggle must work when `localStorage` throws.
//!
//! The page is served by a tiny in-process server; a script injected into its head
//! plays the reader and writes what it measured into `#bd-result`, which
//! `--dump-dom` prints. No browser found is a failure, never a skip.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tempfile::TempDir;

const FILES: usize = 40;
const READER_AT: usize = 18;

fn chrome() -> PathBuf {
    let mut candidates: Vec<PathBuf> =
        std::env::var_os("BRANCHDIFF_CHROME").map(PathBuf::from).into_iter().collect();
    candidates.extend(
        [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
        ]
        .map(PathBuf::from),
    );
    candidates
        .into_iter()
        .find(|p| p.exists())
        .expect("no Chrome found: install one or set BRANCHDIFF_CHROME")
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

fn body_of(file: usize, tweak: &str) -> String {
    (1..=10).map(|n| format!("line {n} of file {file}{tweak}\n")).collect()
}

/// A repository with `FILES` changed files, and the page for it before and after
/// one line of file 3 changes.
fn two_pages() -> (String, String) {
    let dir = TempDir::new().expect("temp dir");
    git(dir.path(), &["init", "-q"]);
    for f in 0..FILES {
        std::fs::write(dir.path().join(format!("f{f:02}.txt")), body_of(f, "")).unwrap();
    }
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "base"]);
    for f in 0..FILES {
        std::fs::write(dir.path().join(format!("f{f:02}.txt")), body_of(f, " edited")).unwrap();
    }
    let first = render_page(dir.path());
    std::fs::write(dir.path().join("f03.txt"), body_of(3, " SWAPPED")).unwrap();
    (first, render_page(dir.path()))
}

/// Serves `first` until `/flip` is requested, then `second`, with `script` in the
/// head of the first page. Returns the address.
fn serve(first: String, second: String, script: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = format!("http://{}/", listener.local_addr().unwrap());
    let flipped = Arc::new(AtomicBool::new(false));
    let first = first.replacen("</head>", &format!("<script>{script}</script></head>"), 1);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let body = if request.starts_with("GET /flip") {
                flipped.store(true, Ordering::SeqCst);
                "ok".to_string()
            } else if request.starts_with("GET / ") && !flipped.load(Ordering::SeqCst) {
                first.clone()
            } else if request.starts_with("GET / ") {
                second.clone()
            } else {
                String::new()
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
        }
    });
    address
}

/// Loads `address` in headless Chrome and returns the text of `#bd-result`.
fn result_of(address: &str) -> String {
    let profile = TempDir::new().expect("profile dir");
    let out = Command::new(chrome())
        .args([
            "--headless",
            "--disable-gpu",
            "--no-sandbox",
            "--no-first-run",
            "--dump-dom",
            "--virtual-time-budget=40000",
            "--window-size=1000,800",
        ])
        .arg(format!("--user-data-dir={}", profile.path().display()))
        .arg(address)
        .output()
        .expect("run Chrome");
    let dom = String::from_utf8_lossy(&out.stdout).into_owned();
    let start = dom
        .find("<pre id=\"bd-result\">")
        .unwrap_or_else(|| panic!("no result in:\n{}", &dom[..dom.len().min(600)]));
    let rest = &dom[start + "<pre id=\"bd-result\">".len()..];
    rest[..rest.find("</pre>").expect("closed result")].replace("&quot;", "\"")
}

const READER: &str = "
addEventListener('load',async()=>{
  const sleep=ms=>new Promise(r=>setTimeout(r,ms));
  const out=o=>{const p=document.createElement('pre');p.id='bd-result';
    p.textContent=JSON.stringify(o);document.documentElement.appendChild(p)};
  try{
    const at=()=>document.getElementById('file-READER_AT');
    while(at().getBoundingClientRect().top>200){scrollBy(0,250);await sleep(30)}
    scrollBy(0,at().getBoundingClientRect().top+50);
    await sleep(200);
    const before=at().getBoundingClientRect().top;
    await fetch('/flip');
    for(let i=0;i<100&&!document.body.textContent.includes('SWAPPED');i++)await sleep(100);
    await sleep(300);
    out({before,after:at().getBoundingClientRect().top,swapped:document.body.textContent.includes('SWAPPED')});
  }catch(e){out({error:String(e)})}
});
";

fn number(json: &str, key: &str) -> f64 {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}: {json}"));
    v[key].as_f64().unwrap_or_else(|| panic!("no {key} in {json}"))
}

#[test]
fn a_live_refresh_keeps_the_section_the_reader_is_in() {
    let (first, second) = two_pages();
    let script = READER.replace("READER_AT", &READER_AT.to_string());
    let json = result_of(&serve(first, second, &script));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["swapped"], true, "the poll never swapped the body in: {json}");
    let (before, after) = (number(&json, "before"), number(&json, "after"));
    assert!(
        (after - before).abs() <= 4.0,
        "section {READER_AT} was {before}px from the top before the refresh and {after}px after"
    );
}

#[test]
fn the_theme_toggle_works_when_local_storage_throws() {
    let (first, second) = two_pages();
    let script = "
Object.defineProperty(window,'localStorage',{get(){throw new Error('blocked')}});
addEventListener('load',()=>{
  const before=document.documentElement.dataset.theme;
  document.querySelector('.theme-toggle').click();
  const p=document.createElement('pre');p.id='bd-result';
  p.textContent=JSON.stringify({before,after:document.documentElement.dataset.theme});
  document.documentElement.appendChild(p);
});";
    let json = result_of(&serve(first, second, script));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        (v["before"].as_str(), v["after"].as_str()),
        (Some("dark"), Some("light")),
        "{json}"
    );
}
