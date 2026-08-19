//! Visible progress for the checks that run before the TUI opens.
//!
//! Loading config, fetching the company config repo, reading tokens out of the
//! keyring and asking Grafana whether we're on call all talk to the disk, the
//! network or a secret service — together they can hold the launch for
//! seconds. Silent, that reads as a hung binary. Every such check gets a
//! [`Step`]: a spinner line on stderr while it runs, replaced by a one-line
//! verdict with the time it took.
//!
//! Output goes to stderr and the TUI opens on the alternate screen, so the
//! whole report disappears the moment the app starts and comes back when it
//! exits.

use std::io::{IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(80);
/// Under this, the elapsed time is noise rather than an explanation.
const REPORT_ELAPSED_FROM: Duration = Duration::from_millis(200);

const RESET: &str = "\x1b[0m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const DIM: &str = "\x1b[2m";
const CLEAR_EOL: &str = "\x1b[K";

/// Set once the invocation is known: machine-readable commands (shell
/// completions) must keep stderr clean.
static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// A pre-start check in progress.
///
/// Finish it with [`Step::done`], [`Step::warn`] or [`Step::skip`] to leave a
/// verdict, or [`Step::clear`] to erase the line (an interactive prompt is
/// about to take over). Dropping it clears the line too, so a `?` on the way
/// out never leaves a spinner stranded above the error.
pub struct Step {
    started: Instant,
    spinner: Option<Spinner>,
    enabled: bool,
    interactive: bool,
    finished: bool,
}

impl Step {
    pub fn start(label: impl Into<String>) -> Self {
        let enabled = ENABLED.load(Ordering::Relaxed);
        let interactive = enabled && std::io::stderr().is_terminal();
        Self {
            started: Instant::now(),
            spinner: interactive.then(|| Spinner::start(label.into())),
            enabled,
            interactive,
            finished: false,
        }
    }

    /// The check ran and everything is in order.
    pub fn done(self, message: impl AsRef<str>) {
        self.finish("✓", GREEN, message.as_ref());
    }

    /// The check ran and found something the user should know about.
    pub fn warn(self, message: impl AsRef<str>) {
        self.finish("!", YELLOW, message.as_ref());
    }

    /// Nothing to check — reported so the absence is visible too.
    pub fn skip(self, message: impl AsRef<str>) {
        self.finish("·", DIM, message.as_ref());
    }

    /// Erase the line without a verdict.
    pub fn clear(self) {
        drop(self);
    }

    fn finish(mut self, symbol: &str, color: &str, message: &str) {
        self.stop_spinner();
        if self.enabled {
            let elapsed = self.elapsed_suffix();
            let mut err = std::io::stderr().lock();
            if self.interactive {
                let _ = write!(
                    err,
                    "\r{color}{symbol}{RESET} {message}{DIM}{elapsed}{RESET}{CLEAR_EOL}\n"
                );
            } else {
                let _ = writeln!(err, "{symbol} {message}{elapsed}");
            }
            let _ = err.flush();
        }
        self.finished = true;
    }

    fn elapsed_suffix(&self) -> String {
        let elapsed = self.started.elapsed();
        if elapsed < REPORT_ELAPSED_FROM {
            String::new()
        } else {
            format!(" ({:.1}s)", elapsed.as_secs_f32())
        }
    }

    fn stop_spinner(&mut self) {
        if let Some(spinner) = self.spinner.take() {
            spinner.stop();
        }
    }
}

impl Drop for Step {
    fn drop(&mut self) {
        self.stop_spinner();
        if !self.finished && self.interactive {
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r{CLEAR_EOL}");
            let _ = err.flush();
        }
    }
}

/// Animates one line on stderr until told to stop. The checks it covers are
/// blocking calls (`git fetch`, keyring reads) or awaits on a runtime that has
/// nothing else to do, so the animation lives on its own thread.
struct Spinner {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl Spinner {
    fn start(label: String) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            for frame in FRAMES.iter().cycle() {
                if flag.load(Ordering::Relaxed) {
                    return;
                }
                let mut err = std::io::stderr().lock();
                let _ = write!(err, "\r{DIM}{frame}{RESET} {label}{CLEAR_EOL}");
                let _ = err.flush();
                drop(err);
                // park/unpark instead of a plain sleep: stopping the spinner is
                // on the critical path of every step, so it must not wait out
                // the remaining tick.
                std::thread::park_timeout(TICK);
            }
        });
        Self { stop, handle }
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.thread().unpark();
        let _ = self.handle.join();
    }
}
