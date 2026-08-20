//! Visible progress for the checks that run before the TUI opens.
//!
//! Loading config, fetching the company config repo, reading tokens out of the
//! keyring and asking Grafana whether we're on call all talk to the disk, the
//! network or a secret service — together they can hold the launch for
//! seconds. Silent, that reads as a hung binary. Every such check gets a
//! [`Step`]: a spinner line on stderr while it runs, replaced by a one-line
//! verdict with the time it took.
//!
//! Checks that don't depend on each other run at the same time; those share a
//! [`Group`], which renders one line per check as a block redrawn in place, so
//! a slow lookup is visible as the one line still spinning.
//!
//! Output goes to stderr and the TUI opens on the alternate screen, so the
//! whole report disappears the moment the app starts and comes back when it
//! exits.

use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
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

/// Verdict markers: the check ran and is fine / ran and found something worth
/// saying / had nothing to check.
const OK: (&str, &str) = ("✓", GREEN);
const WARN: (&str, &str) = ("!", YELLOW);
const SKIP: (&str, &str) = ("·", DIM);

/// `" (1.2s)"`, or nothing when the duration is too short to explain anything.
fn elapsed_suffix(elapsed: Duration) -> String {
    if elapsed < REPORT_ELAPSED_FROM {
        String::new()
    } else {
        format!(" ({:.1}s)", elapsed.as_secs_f32())
    }
}

/// Set once the invocation is known: machine-readable commands (shell
/// completions) must keep stderr clean.
static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// A pre-start check in progress.
///
/// Finish it with [`Step::done`], [`Step::warn`] or [`Step::skip`] to leave a
/// verdict. Dropping it without one erases the line instead, so a `?` on the
/// way out never leaves a spinner stranded above the error.
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
        self.finish(OK, message.as_ref());
    }

    /// The check ran and found something the user should know about.
    pub fn warn(self, message: impl AsRef<str>) {
        self.finish(WARN, message.as_ref());
    }

    /// Nothing to check — reported so the absence is visible too.
    pub fn skip(self, message: impl AsRef<str>) {
        self.finish(SKIP, message.as_ref());
    }

    fn finish(mut self, (symbol, color): (&str, &str), message: &str) {
        self.stop_spinner();
        if self.enabled {
            let elapsed = elapsed_suffix(self.started.elapsed());
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

/// A block of checks that run at the same time.
///
/// One line per check, redrawn in place every tick: still-running checks show
/// a spinner, finished ones their verdict, so the block reads as a live list of
/// what the launch is waiting on. Give [`Group::start`] one label per check —
/// `None` for a check that doesn't apply to this config, which gets no line and
/// a no-op [`Slot`], so callers can keep the array shape while the block only
/// shows what actually runs.
///
/// The work itself is expected to be blocking (keyring reads, credential
/// commands) and to borrow from the caller, so [`std::thread::scope`] is the
/// natural driver: `Slot` is `Send`, and the scope's exit is the join.
///
/// Dropping the group stops the animation and collapses the block to just the
/// lines that reached a verdict — a check whose slot was dropped unfinished
/// (an early `?`, a panic) leaves no stranded spinner behind.
pub struct Group {
    state: Arc<State>,
    renderer: Option<JoinHandle<()>>,
}

/// One check's line in the group.
struct Line {
    label: String,
    started: Instant,
    /// `None` while the check is still running.
    verdict: Option<Verdict>,
}

/// What a finished check left behind. `None` message means the check had
/// nothing to report and its line should disappear (see [`Slot::clear`]).
struct Verdict {
    marker: Option<(&'static str, &'static str)>,
    message: String,
    elapsed: Duration,
}

/// Shared between the renderer thread and every [`Slot`].
struct State {
    lines: Mutex<Vec<Line>>,
    stop: AtomicBool,
    /// Number of block lines currently on screen, so each repaint knows how far
    /// up to reach (0 before the first paint).
    on_screen: AtomicUsize,
    enabled: bool,
    interactive: bool,
}

/// A single check's handle on its line in a [`Group`].
pub struct Slot {
    state: Arc<State>,
    /// `None` for a check the config skipped entirely — every method is a no-op.
    index: Option<usize>,
}

impl Group {
    /// Open a block for `labels`, one line per `Some`, and hand back a slot per
    /// entry in the same order.
    pub fn start<const N: usize>(labels: [Option<&str>; N]) -> (Self, [Slot; N]) {
        let enabled = ENABLED.load(Ordering::Relaxed);
        let interactive = enabled && std::io::stderr().is_terminal();
        let started = Instant::now();

        // Slot i addresses line j only for the labels that are present, so the
        // block has no gaps for checks this config skips.
        let mut indices = [None; N];
        let mut lines = Vec::new();
        for (slot, label) in indices.iter_mut().zip(labels) {
            if let Some(label) = label {
                *slot = Some(lines.len());
                lines.push(Line {
                    label: label.to_string(),
                    started,
                    verdict: None,
                });
            }
        }

        let state = Arc::new(State {
            lines: Mutex::new(lines),
            stop: AtomicBool::new(false),
            on_screen: AtomicUsize::new(0),
            enabled,
            interactive,
        });
        // Nothing to show: no lines, or a non-terminal stderr where each
        // verdict is written out as it lands instead of animated.
        let animate = interactive && !state.lock_lines().is_empty();
        let renderer = animate.then(|| {
            let state = Arc::clone(&state);
            std::thread::spawn(move || state.animate())
        });

        let slots = std::array::from_fn(|i| Slot {
            state: Arc::clone(&state),
            index: indices[i],
        });
        (Self { state, renderer }, slots)
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if let Some(renderer) = self.renderer.take() {
            self.state.stop.store(true, Ordering::Relaxed);
            renderer.thread().unpark();
            let _ = renderer.join();
        }
        self.state.collapse();
    }
}

impl Slot {
    /// The check ran and everything is in order.
    pub fn done(self, message: impl AsRef<str>) {
        self.finish(Some(OK), message.as_ref());
    }

    /// The check ran and found something the user should know about.
    pub fn warn(self, message: impl AsRef<str>) {
        self.finish(Some(WARN), message.as_ref());
    }

    /// Drop the line without a verdict: nothing happened, so say nothing.
    pub fn clear(self) {
        self.finish(None, "");
    }

    fn finish(self, marker: Option<(&'static str, &'static str)>, message: &str) {
        let Some(index) = self.index else { return };
        let mut lines = self.state.lock_lines();
        let Some(line) = lines.get_mut(index) else {
            return;
        };
        let elapsed = line.started.elapsed();
        line.verdict = Some(Verdict {
            marker,
            message: message.to_string(),
            elapsed,
        });
        drop(lines);
        // Animated blocks are painted by the renderer; without one, the verdict
        // is a plain line written the moment it lands (so ordering follows
        // completion, not declaration).
        if self.state.enabled
            && !self.state.interactive
            && let Some((symbol, _)) = marker
        {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "{symbol} {message}{}", elapsed_suffix(elapsed));
            let _ = err.flush();
        }
    }
}

impl State {
    fn lock_lines(&self) -> std::sync::MutexGuard<'_, Vec<Line>> {
        self.lines.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn animate(&self) {
        for frame in FRAMES.iter().cycle() {
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            self.paint(frame);
            // Same reason as `Spinner`: stopping must not wait out the tick.
            std::thread::park_timeout(TICK);
        }
    }

    /// Redraw the whole block in place. The cursor starts and ends just below
    /// it, so a repaint reaches back up over the lines it wrote last time.
    fn paint(&self, frame: &str) {
        let (block, painted) = {
            let lines = self.lock_lines();
            let mut block = String::new();
            rewind(&mut block, self.on_screen.load(Ordering::Relaxed));
            for line in lines.iter() {
                match &line.verdict {
                    None => {
                        let _ = writeln!(block, "\r{DIM}{frame}{RESET} {}{CLEAR_EOL}", line.label);
                    }
                    // A cleared line renders blank and holds its place while the
                    // block is live; the collapse on drop is what removes it.
                    Some(verdict) => block.push_str(&verdict.render()),
                }
            }
            (block, lines.len())
        };
        self.flush(&block, painted);
    }

    /// Erase the block and reprint only the lines that reached a verdict, so
    /// the report left behind has no blanks and no stranded spinners.
    fn collapse(&self) {
        let above = self.on_screen.load(Ordering::Relaxed);
        if !self.interactive || above == 0 {
            return;
        }
        let keepers: Vec<String> = {
            let lines = self.lock_lines();
            lines
                .iter()
                .filter_map(|line| line.verdict.as_ref())
                .filter(|verdict| verdict.marker.is_some())
                .map(Verdict::render)
                .collect()
        };
        // Wipe what is on screen, rewind over the blanks, then lay down the
        // keepers: a block that loses lines must not leave gaps behind.
        let mut block = String::new();
        rewind(&mut block, above);
        for _ in 0..above {
            let _ = writeln!(block, "\r{CLEAR_EOL}");
        }
        rewind(&mut block, above);
        let painted = keepers.len();
        block.extend(keepers);
        self.flush(&block, painted);
    }

    fn flush(&self, block: &str, painted: usize) {
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(block.as_bytes());
        let _ = err.flush();
        self.on_screen.store(painted, Ordering::Relaxed);
    }
}

impl Verdict {
    fn render(&self) -> String {
        let Some((symbol, color)) = self.marker else {
            return format!("\r{CLEAR_EOL}\n");
        };
        let suffix = elapsed_suffix(self.elapsed);
        format!(
            "\r{color}{symbol}{RESET} {}{DIM}{suffix}{RESET}{CLEAR_EOL}\n",
            self.message
        )
    }
}

/// Move the cursor up `lines` rows. `CSI 0 A` means "up one" to most
/// terminals, so an empty block must not emit it at all.
fn rewind(block: &mut String, lines: usize) {
    if lines > 0 {
        let _ = write!(block, "\x1b[{lines}A");
    }
}
