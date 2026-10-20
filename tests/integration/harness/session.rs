//! TUI session control for integration tests.
//!
//! Uses portable-pty to spawn branchdiff in a real PTY, and vt100 to parse
//! the terminal output into a screen state we can assert against.

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};
use vt100::Parser;

const TERM_ROWS: u16 = 40;
const TERM_COLS: u16 = 120;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// The query every terminal answers, and the answer ("terminal OK").
const DEVICE_STATUS_REPORT_QUERY: &[u8] = b"\x1b[5n";
const DEVICE_STATUS_REPORT_OK: &[u8] = b"\x1b[0n";

/// A TUI session running branchdiff.
pub struct TuiSession {
    parser: Parser,
    output_rx: Receiver<Vec<u8>>,
    writer: Box<dyn std::io::Write + Send>,
    /// Every byte the app has written, before parsing. Lets tests assert on the
    /// escape sequences branchdiff emits, not just the screen they produce.
    raw_output: Vec<u8>,
    /// How much of `raw_output` has been searched for the status query.
    scanned_to: usize,
    /// Whether to reply to the app's status query, as every real terminal does.
    answers_queries: bool,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl TuiSession {
    /// Launch branchdiff in the given repository directory.
    /// Waits for initial render before returning.
    pub fn launch(repo_path: &Path) -> Self {
        Self::launch_with_env(repo_path, &[])
    }

    pub fn launch_with_env(repo_path: &Path, env: &[(&str, &str)]) -> Self {
        Self::launch_configured(repo_path, env, true)
    }

    /// A terminal that never answers the app's startup query (a dumb or broken one).
    /// The app sees only `PATH`, `HOME` and the variables given, so a multiplexer or
    /// `TERM` inherited from the developer's shell cannot decide the outcome; with no
    /// `TERM` given, none is set.
    pub fn launch_unanswering(repo_path: &Path, env: &[(&str, &str)]) -> Self {
        Self::launch_configured(repo_path, env, false)
    }

    fn launch_configured(repo_path: &Path, env: &[(&str, &str)], answers_queries: bool) -> Self {
        let pty_system = native_pty_system();

        let pair = pty_system
            .openpty(PtySize { rows: TERM_ROWS, cols: TERM_COLS, pixel_width: 0, pixel_height: 0 })
            .expect("failed to open pty");

        let mut cmd = CommandBuilder::new("branchdiff");
        cmd.cwd(repo_path);
        if answers_queries {
            cmd.env("TERM", "xterm-256color");
        } else {
            cmd.env_clear();
            cmd.env("PATH", std::env::var_os("PATH").expect("PATH is set"));
            cmd.env("HOME", repo_path);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }

        let child = pair.slave.spawn_command(cmd).expect("failed to spawn branchdiff");

        let mut reader = pair.master.try_clone_reader().expect("failed to get reader");
        let writer = pair.master.take_writer().expect("failed to get writer");

        // Spawn a thread to read output asynchronously
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break; // Receiver dropped
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let parser = Parser::new(TERM_ROWS, TERM_COLS, 0);

        let mut session = Self {
            parser,
            output_rx: rx,
            writer,
            raw_output: Vec::new(),
            scanned_to: 0,
            answers_queries,
            _child: child,
        };

        // Wait for initial render
        session
            .wait_for(|contents| !contents.trim().is_empty(), DEFAULT_TIMEOUT)
            .expect("timed out waiting for initial render");

        session
    }

    /// Read any available output and update the screen state.
    fn poll(&mut self) {
        loop {
            match self.output_rx.try_recv() {
                Ok(data) => {
                    self.raw_output.extend_from_slice(&data);
                    self.parser.process(&data);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        self.answer_status_reports();
    }

    /// Answer the app's "are you there" terminal query, as every real terminal does.
    ///
    /// At startup the image-protocol picker writes its capability queries
    /// ending in a device status report (`ESC [ 5 n`) and waits for the reply.
    /// A terminal that never answers leaves its reader thread blocked on
    /// stdin, and that thread then takes the first keys the test sends and
    /// switches raw mode off behind the app's back. Without the reply the
    /// keys of four tests were lost depending on timing.
    ///
    /// The query is searched for in all output so far, starting a few bytes
    /// before where the last search ended, so one split across two reads is
    /// found and each occurrence is answered once.
    fn answer_status_reports(&mut self) {
        let query = DEVICE_STATUS_REPORT_QUERY;
        let from = self.scanned_to.saturating_sub(query.len() - 1);
        let queries = self.raw_output[from..].windows(query.len()).filter(|w| *w == query).count();
        self.scanned_to = self.raw_output.len();
        if self.answers_queries && queries > 0 {
            for _ in 0..queries {
                self.writer.write_all(DEVICE_STATUS_REPORT_OK).expect("failed to answer the query");
            }
            self.writer.flush().expect("failed to flush");
        }
    }

    /// Whether the app has written the given byte sequence to the terminal.
    pub fn emitted(&mut self, bytes: &[u8]) -> bool {
        self.poll();
        self.raw_output.windows(bytes.len()).any(|w| w == bytes)
    }

    /// How many bytes the app has written so far. Lets a test tell "the app
    /// redrew and it didn't help" apart from "the app never redrew".
    pub fn bytes_written(&mut self) -> usize {
        self.poll();
        self.raw_output.len()
    }

    /// Get the current screen contents as plain text.
    pub fn text(&mut self) -> String {
        self.poll();
        self.parser.screen().contents()
    }

    /// Wipe the terminal's screen *behind the application's back*.
    ///
    /// This is the "came back to my desk and it's half painted" bug in a form
    /// that reproduces in milliseconds. Real terminals lose or rewrite their
    /// screen for reasons the app is never told about — display sleep, the
    /// terminal repainting itself, a reattach. ratatui keeps an in-memory copy
    /// of the previous frame and writes only the cells that differ from it, so
    /// once the real screen and that copy disagree, an ordinary draw repairs
    /// nothing: every stale cell is "already correct" as far as ratatui knows.
    ///
    /// Processing the erase into the parser (rather than sending it to the app)
    /// is what makes this faithful: the bytes never reach branchdiff, exactly
    /// like the real thing.
    pub fn simulate_terminal_wiped(&mut self) {
        self.poll(); // fold in anything already rendered
        self.parser.process(b"\x1b[2J");
    }

    /// Send a raw byte sequence, e.g. a terminal-generated event.
    pub fn send_raw(&mut self, bytes: &[u8]) {
        // Answer any pending terminal query before the bytes go out.
        self.poll();
        self.writer.write_all(bytes).expect("failed to send bytes");
        self.writer.flush().expect("failed to flush");
        thread::sleep(Duration::from_millis(50));
    }

    /// Send a key press (single character).
    pub fn press(&mut self, key: &str) {
        // Answer any pending terminal query before the key goes out.
        self.poll();
        self.writer.write_all(key.as_bytes()).expect("failed to send key");
        self.writer.flush().expect("failed to flush");
        // Give the app time to process
        thread::sleep(Duration::from_millis(50));
    }

    /// Wait for a condition to become true, polling the screen.
    pub fn wait_for<F>(&mut self, condition: F, timeout: Duration) -> Result<(), String>
    where
        F: Fn(&str) -> bool,
    {
        let start = Instant::now();
        loop {
            self.poll();
            let contents = self.parser.screen().contents();
            if condition(&contents) {
                return Ok(());
            }
            if start.elapsed() > timeout {
                return Err(format!("Timeout after {:?}. Screen contents:\n{}", timeout, contents));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    /// Wait for specific text to appear on screen.
    pub fn wait_for_text(&mut self, text: &str) {
        let text_owned = text.to_string();
        self.wait_for(|contents| contents.contains(&text_owned), DEFAULT_TIMEOUT)
            .unwrap_or_else(|e| panic!("Timeout waiting for '{}': {}", text, e));
    }

    /// Assert that the screen contains the given text.
    pub fn assert_contains(&mut self, text: &str) {
        let screen = self.text();
        assert!(screen.contains(text), "Expected screen to contain '{}', got:\n{}", text, screen);
    }

    /// Assert that the status bar (last non-empty line) contains the given text.
    pub fn assert_status_bar_contains(&mut self, pattern: &str) {
        let screen = self.text();
        let last_line = screen.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        assert!(
            last_line.contains(pattern),
            "Expected status bar to contain '{}', got: {}",
            pattern,
            last_line
        );
    }
}
