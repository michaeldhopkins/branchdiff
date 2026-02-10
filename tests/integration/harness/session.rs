//! TUI session control for integration tests.
//!
//! Uses portable-pty to spawn branchdiff in a real PTY, and vt100 to parse
//! the terminal output into a screen state we can assert against.

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::Read;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};
use vt100::Parser;

const TERM_ROWS: u16 = 40;
const TERM_COLS: u16 = 120;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A TUI session running branchdiff.
pub struct TuiSession {
    parser: Parser,
    output_rx: Receiver<Vec<u8>>,
    writer: Box<dyn std::io::Write + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl TuiSession {
    /// Launch branchdiff in the given repository directory.
    /// Waits for initial render before returning.
    pub fn launch(repo_path: &Path) -> Self {
        let pty_system = native_pty_system();

        let pair = pty_system
            .openpty(PtySize {
                rows: TERM_ROWS,
                cols: TERM_COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("failed to open pty");

        let mut cmd = CommandBuilder::new("branchdiff");
        cmd.cwd(repo_path);
        cmd.env("TERM", "xterm-256color");

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
                Ok(data) => self.parser.process(&data),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    /// Get the current screen contents as plain text.
    pub fn text(&mut self) -> String {
        self.poll();
        self.parser.screen().contents()
    }

    /// Send a key press (single character).
    pub fn press(&mut self, key: &str) {
        self.writer
            .write_all(key.as_bytes())
            .expect("failed to send key");
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
                return Err(format!(
                    "Timeout after {:?}. Screen contents:\n{}",
                    timeout, contents
                ));
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
        assert!(
            screen.contains(text),
            "Expected screen to contain '{}', got:\n{}",
            text,
            screen
        );
    }

    /// Assert that the status bar (last non-empty line) contains the given text.
    pub fn assert_status_bar_contains(&mut self, pattern: &str) {
        let screen = self.text();
        let last_line = screen
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        assert!(
            last_line.contains(pattern),
            "Expected status bar to contain '{}', got: {}",
            pattern,
            last_line
        );
    }
}
