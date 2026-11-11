//! PTY harness for the end-to-end TUI tests: spawns the real `dbtl` binary
//! inside a pseudo-terminal (portable-pty), interprets its output through a
//! vt100 screen model, and answers the terminal capability queries the binary
//! emits at startup.
//!
//! One [`TuiSession`] is one launch: `wait_for` polls the decoded screen for
//! expected text (never a fixed sleep), key presses and SGR mouse events go to
//! the master writer, and `Drop` always kills + reaps the child so a failed
//! assertion can never leak a process.

use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, ExitStatus, MasterPty, PtySize};
use vt100::Parser;

/// The committed jaffle_finance manifest every session loads.
pub const FIXTURE_MANIFEST: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/manifest.json");

/// The terminal size every session starts at (unless a test resizes away).
pub const COLS: u16 = 160;
pub const ROWS: u16 = 44;

/// How often `wait_*` re-reads the screen between deadline checks.
const POLL: Duration = Duration::from_millis(25);

// ---- terminal queries the binary emits, and our canned answers ------------
//
// crossterm's keyboard-enhancement probe writes `ESC [ ? u` + `ESC [ c` (kitty
// flags query + primary device attributes); the ambiguous-width probe writes
// `ESC [ 6 n` (device status report — cursor position). Each pattern gets
// exactly its own answer — `?u` → `?0u` (no kitty flags active), `c` → `?6c`
// (a DA response), `6n` → `1;2R` (cursor at row 1, col 2 — a 1-cell ambiguous
// width) — so a combined `?u`+`c` write still yields one reply per query. The
// queries are pure ASCII, so matching raw bytes keeps them detectable even
// when a read chunk splits a UTF-8 sequence mid-character.
const QUERIES: &[(&[u8], &[u8])] = &[
    (b"\x1b[?u", b"\x1b[?0u"),
    (b"\x1b[c", b"\x1b[?6c"),
    (b"\x1b[6n", b"\x1b[1;2R"),
];

/// The longest query pattern, minus one — the scan tail that must survive
/// between chunks so a split query still matches.
const SCAN_KEEP: usize = 3;

/// One launched `dbtl` inside its own PTY.
pub struct TuiSession {
    master: Box<dyn MasterPty>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    parser: Arc<Mutex<Parser>>,
    reader: Option<JoinHandle<()>>,
    cols: u16,
    /// Kept alive for the session's lifetime: XDG state/config isolation.
    _tmp: tempfile::TempDir,
}

impl TuiSession {
    /// Spawn `dbtl --manifest <manifest> --unicode` at `cols`×`rows` with the
    /// XDG dirs pointed at a fresh temp dir (no saved state → the home screen)
    /// and `TERM=xterm-256color`. `answer_queries` exists so a test can prove
    /// the harness (not the app) is what unblocks the startup probes.
    pub fn spawn_at(cols: u16, rows: u16, manifest: &str, answer_queries: bool) -> Self {
        let tmp = tempfile::tempdir().expect("temp dir for XDG isolation");
        let pty = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");

        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_dbtl"));
        cmd.arg("--manifest");
        cmd.arg(manifest);
        cmd.arg("--unicode");
        cmd.cwd(env!("CARGO_MANIFEST_DIR"));
        cmd.env("XDG_STATE_HOME", tmp.path().join("state"));
        cmd.env("XDG_CONFIG_HOME", tmp.path().join("config"));
        cmd.env("TERM", "xterm-256color");
        let child = pty
            .slave
            .spawn_command(cmd)
            .expect("spawn dbtl under the pty");
        drop(pty.slave);

        let writer = Arc::new(Mutex::new(pty.master.take_writer().expect("pty writer")));
        let parser = Arc::new(Mutex::new(Parser::new(
            NonZeroU16::new(rows).expect("rows"),
            NonZeroU16::new(cols).expect("cols"),
            0,
        )));

        let mut reader = pty.master.try_clone_reader().expect("pty reader");
        let (reader_parser, reader_writer) = (Arc::clone(&parser), Arc::clone(&writer));
        let reader = std::thread::spawn(move || {
            // The tail of undelivered output: query patterns can straddle two
            // reads, so up to SCAN_KEEP bytes are held back for the next pass.
            let mut scan: Vec<u8> = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        reader_parser.lock().expect("parser").process(chunk);
                        if answer_queries {
                            scan.extend_from_slice(chunk);
                            let responses = answer_pending(&mut scan);
                            if !responses.is_empty() {
                                let mut w = reader_writer.lock().expect("writer");
                                for r in responses {
                                    let _ = w.write_all(r);
                                }
                                let _ = w.flush();
                            }
                        }
                    }
                    Err(_) => break, // child died or the pty went away
                }
            }
        });

        TuiSession {
            master: pty.master,
            child: Mutex::new(child),
            writer,
            parser,
            reader: Some(reader),
            cols,
            _tmp: tmp,
        }
    }

    pub fn spawn() -> Self {
        Self::spawn_at(COLS, ROWS, FIXTURE_MANIFEST, true)
    }

    /// The visible screen as text: one line per row, trailing blanks kept by
    /// whatever the emulator leaves (substring matching never needs them).
    pub fn screen_text(&self) -> String {
        self.parser
            .lock()
            .expect("parser")
            .screen()
            .rows(0, self.cols)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Poll the screen until `needle` shows or `timeout` elapses. Returns the
    /// screen on success; on failure the error carries the last screen dump
    /// plus whether the child had already exited.
    pub fn wait_for(&mut self, needle: &str, timeout: Duration) -> Result<String, String> {
        self.wait_until(format!("text {needle:?}"), timeout, |s| s.contains(needle))
    }

    /// Poll the screen until `needle` disappears or `timeout` elapses.
    pub fn wait_gone(&mut self, needle: &str, timeout: Duration) -> Result<String, String> {
        self.wait_until(format!("text {needle:?} gone"), timeout, |s| {
            !s.contains(needle)
        })
    }

    /// The deadline-bounded poll loop every wait shares. Bails early when the
    /// child has already exited (the screen can only go stale from there).
    pub fn wait_until(
        &mut self,
        what: String,
        timeout: Duration,
        pred: impl Fn(&str) -> bool,
    ) -> Result<String, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let screen = self.screen_text();
            if pred(&screen) {
                return Ok(screen);
            }
            if self.exited() {
                return Err(format!(
                    "child exited before {what} appeared\n--- screen ---\n{screen}"
                ));
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for {what} after {timeout:?}\n--- screen ---\n{screen}"
                ));
            }
            std::thread::sleep(POLL);
        }
    }

    /// Send raw bytes to the child's stdin (keys are just byte sequences).
    pub fn send(&mut self, bytes: &[u8]) {
        let mut w = self.writer.lock().expect("writer");
        w.write_all(bytes).expect("write to pty");
        w.flush().expect("flush to pty");
    }

    /// Send a string of keystrokes.
    pub fn send_str(&mut self, s: &str) {
        self.send(s.as_bytes());
    }

    /// An SGR mouse click (press + release, left button) at a 1-based cell.
    /// Clicks confirm on release, so both events go out.
    pub fn click(&mut self, col: u16, row: u16) {
        self.send(format!("\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m").as_bytes());
    }

    /// Resize the terminal (SIGWINCH to the child) and the screen model.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("pty resize");
        self.parser.lock().expect("parser").set_size(
            NonZeroU16::new(rows).expect("rows"),
            NonZeroU16::new(cols).expect("cols"),
        );
        self.cols = cols;
    }

    /// The child's process id (for an outside-in liveness check).
    pub fn child_pid(&self) -> Option<u32> {
        self.child.lock().expect("child").process_id()
    }

    /// Whether the child has already exited (reaped or not).
    pub fn exited(&mut self) -> bool {
        matches!(self.child.lock().expect("child").try_wait(), Ok(Some(_)))
    }

    /// Wait for the child to exit within `timeout`, returning its status.
    pub fn wait_exit(&mut self, timeout: Duration) -> Result<ExitStatus, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(Some(status)) = self.child.lock().expect("child").try_wait() {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(format!("child did not exit within {timeout:?}"));
            }
            std::thread::sleep(POLL);
        }
    }
}

impl Drop for TuiSession {
    fn drop(&mut self) {
        {
            let mut child = self.child.lock().expect("child");
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(h) = self.reader.take() {
            match h.join() {
                Ok(()) => {}
                // A panicked reader must not vanish into the JoinHandle —
                // re-throw it unless we're already unwinding a test panic
                // (a second panic in Drop would abort the process).
                Err(e) if !std::thread::panicking() => std::panic::resume_unwind(e),
                Err(_) => {}
            }
        }
    }
}

/// Drain answered prefixes out of `scan`, returning the responses to write.
/// A pattern is answered once per occurrence; the tail is kept short so a
/// split query survives to the next chunk.
fn answer_pending(scan: &mut Vec<u8>) -> Vec<&'static [u8]> {
    let mut out = Vec::new();
    loop {
        let mut earliest: Option<(usize, usize, &'static [u8])> = None;
        for (pat, resp) in QUERIES {
            if let Some(i) = find_subslice(scan, pat) {
                if earliest.is_none_or(|(ei, _, _)| i < ei) {
                    earliest = Some((i, pat.len(), resp));
                }
            }
        }
        let Some((i, len, resp)) = earliest else {
            break;
        };
        out.push(resp);
        scan.drain(..i + len);
    }
    if scan.len() > SCAN_KEEP {
        let drop = scan.len() - SCAN_KEEP;
        scan.drain(..drop);
    }
    out
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
