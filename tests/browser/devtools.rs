//! A headless Chrome driven over the DevTools protocol, in real time.
//!
//! Every wait is bounded by one deadline set at launch. When it passes, or the test
//! panics for any reason, `Drop` kills the Chrome this struct spawned (that child only)
//! so a stuck browser fails the test instead of hanging it.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use tungstenite::{Message, WebSocket};

use super::find::find_chrome;

const POLL: Duration = Duration::from_millis(50);
const SOCKET_SLICE: Duration = Duration::from_millis(250);

pub struct Browser {
    child: Child,
    socket: WebSocket<TcpStream>,
    next_id: u64,
    events: Vec<Value>,
    deadline: Instant,
    _profile: TempDir,
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Browser {
    /// Starts Chrome and attaches to its first page. Fails once `budget` has passed.
    pub fn launch(budget: Duration) -> Self {
        Self::launch_binary(&find_chrome(), budget)
    }

    /// Like `launch`, with the browser binary named.
    pub fn launch_binary(binary: &Path, budget: Duration) -> Self {
        let deadline = Instant::now() + budget;
        let profile = TempDir::new().expect("profile dir");
        let child = Command::new(binary)
            .args([
                "--headless",
                "--disable-gpu",
                "--no-sandbox",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-timer-throttling",
                "--disable-renderer-backgrounding",
                "--remote-debugging-port=0",
                "--window-size=1000,800",
            ])
            .arg(format!("--user-data-dir={}", profile.path().display()))
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start Chrome");
        // From here the guard owns the child, so a panic below still kills it.
        let mut guard = Guard(Some(child));
        let port = wait_for_port(profile.path(), deadline);
        let socket = attach(port, deadline);
        let child = guard.0.take().expect("child held");
        let mut browser =
            Self { child, socket, next_id: 0, events: Vec::new(), deadline, _profile: profile };
        browser.call("Page.enable", json!({}));
        browser.call("Runtime.enable", json!({}));
        browser
    }

    /// Runs `script` in every new document before the page's own scripts.
    pub fn before_each_document(&mut self, script: &str) {
        self.call("Page.addScriptToEvaluateOnNewDocument", json!({ "source": script }));
    }

    /// Loads `url` and waits for its `load` event.
    pub fn navigate(&mut self, url: &str) {
        self.events.clear();
        self.call("Page.navigate", json!({ "url": url }));
        self.wait_for_event("Page.loadEventFired");
    }

    /// Evaluates `expression`, awaiting it when it is a promise, and returns its value.
    pub fn eval(&mut self, expression: &str) -> Value {
        let reply = self.call(
            "Runtime.evaluate",
            json!({ "expression": expression, "awaitPromise": true, "returnByValue": true }),
        );
        if let Some(problem) = reply.get("exceptionDetails") {
            panic!("script failed: {problem}\nin: {expression}");
        }
        reply["result"]["value"].clone()
    }

    /// Slows the page's CPU by `rate`, as a phone or tablet would be next to this Mac.
    pub fn throttle_cpu(&mut self, rate: u32) {
        self.call("Emulation.setCPUThrottlingRate", json!({ "rate": rate }));
    }

    /// Gives the page a `width` by `height` window at `scale` device pixels per CSS pixel.
    pub fn set_screen(&mut self, width: u32, height: u32, scale: u32) {
        self.call(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": width,
                "height": height,
                "deviceScaleFactor": scale,
                "mobile": false,
            }),
        );
    }

    /// Two animation frames, so layout and paint have really run.
    pub fn two_frames(&mut self) {
        self.eval("new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))");
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let request = json!({ "id": id, "method": method, "params": params });
        self.socket.send(Message::text(request.to_string())).expect("send to Chrome");
        loop {
            let message = self.read();
            if message["id"] == id {
                if let Some(error) = message.get("error") {
                    panic!("{method} failed: {error}");
                }
                return message["result"].clone();
            }
            if message.get("method").is_some() {
                self.events.push(message);
            }
        }
    }

    fn wait_for_event(&mut self, name: &str) {
        loop {
            if let Some(at) = self.events.iter().position(|e| e["method"] == name) {
                self.events.remove(at);
                return;
            }
            let message = self.read();
            if message.get("method").is_some() {
                self.events.push(message);
            }
        }
    }

    /// The next message, or a panic once the deadline has passed.
    fn read(&mut self) -> Value {
        loop {
            assert!(Instant::now() < self.deadline, "Chrome did not answer before the deadline");
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    return serde_json::from_str(text.as_str()).expect("JSON from Chrome");
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => panic!("DevTools connection failed: {e}"),
            }
        }
    }
}

/// Kills a child that has not yet been handed to a `Browser`.
struct Guard(Option<Child>);

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Chrome writes the debugging port as the first line of `DevToolsActivePort`.
fn wait_for_port(profile: &Path, deadline: Instant) -> u16 {
    loop {
        if let Ok(text) = std::fs::read_to_string(profile.join("DevToolsActivePort"))
            && let Some(port) = text.lines().next().and_then(|l| l.trim().parse().ok())
        {
            return port;
        }
        assert!(Instant::now() < deadline, "Chrome never opened its debugging port");
        std::thread::sleep(POLL);
    }
}

fn http_get(port: u16, path: &str, deadline: Instant) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to Chrome");
    stream.set_read_timeout(Some(SOCKET_SLICE)).expect("timeout");
    write!(stream, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
        .expect("request");
    let mut reply = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        assert!(Instant::now() < deadline, "Chrome did not answer {path}");
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                reply.extend_from_slice(&chunk[..n]);
                if body_complete(&reply) {
                    break;
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => panic!("read {path}: {e}"),
        }
    }
    let text = String::from_utf8_lossy(&reply).into_owned();
    text.split_once("\r\n\r\n").map(|(_, body)| body.to_owned()).unwrap_or_default()
}

/// Whether `reply` holds the whole body its `Content-Length` promises; Chrome keeps the
/// connection open, so waiting for the end of the stream would never finish.
fn body_complete(reply: &[u8]) -> bool {
    let text = String::from_utf8_lossy(reply);
    let Some((head, body)) = text.split_once("\r\n\r\n") else { return false };
    head.lines()
        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:")?.trim().parse().ok())
        .is_some_and(|length: usize| body.len() >= length)
}

/// Connects to the first page target's DevTools socket.
fn attach(port: u16, deadline: Instant) -> WebSocket<TcpStream> {
    let url = loop {
        let body = http_get(port, "/json/list", deadline);
        let targets: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let page = targets
            .as_array()
            .and_then(|all| all.iter().find(|t| t["type"] == "page"))
            .and_then(|t| t["webSocketDebuggerUrl"].as_str());
        if let Some(url) = page {
            break url.to_owned();
        }
        assert!(Instant::now() < deadline, "Chrome listed no page to drive");
        std::thread::sleep(POLL);
    };
    let stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to page socket");
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
    let (socket, _) = tungstenite::client(url.as_str(), stream).expect("DevTools handshake");
    socket.get_ref().set_read_timeout(Some(SOCKET_SLICE)).expect("timeout");
    socket
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    fn alive(pid: &str) -> bool {
        Command::new("kill").args(["-0", pid]).stderr(Stdio::null()).status().unwrap().success()
    }

    #[test]
    fn a_browser_that_never_answers_fails_at_the_deadline_and_is_killed() {
        let dir = TempDir::new().unwrap();
        let stub = dir.path().join("hung-chrome");
        std::fs::write(&stub, "#!/bin/sh\necho $$ > \"$(dirname \"$0\")/pid\"\nexec sleep 300\n")
            .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let started = Instant::now();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            Browser::launch_binary(&stub, Duration::from_secs(2));
        }));

        assert!(outcome.is_err(), "a browser that never opens its port must fail the launch");
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        let pid = std::fs::read_to_string(dir.path().join("pid")).unwrap();
        assert!(!alive(pid.trim()), "the hung browser was left running");
    }
}
