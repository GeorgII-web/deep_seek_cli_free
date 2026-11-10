//! One status line that shows what is being waited for and for how long.

use super::style::Style;
use super::{screen_lock, screen_write};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(90);
/// How long a notice stays.
const NOTICE: Duration = Duration::from_secs(2);

struct State {
    label: Option<String>,
    since: Instant,
    frame: usize,
    /// The line is on the screen and must be erased
    drawn: bool,
    /// A short message shown instead of the key hint, with the time it was set
    notice: Option<(String, Instant)>,
}

/// Lets another thread put a notice on the progress line.
#[derive(Clone)]
pub struct Notifier(Arc<Mutex<State>>);

impl Notifier {
    /// Returns false when no progress line is shown, so the caller prints the text itself.
    pub fn show(&self, text: &str) -> bool {
        let mut state = self.0.lock().unwrap();
        state.notice = Some((text.to_string(), Instant::now()));
        state.label.is_some()
    }
}

pub struct Spinner {
    state: Arc<Mutex<State>>,
    /// Off without a terminal: nothing is ever drawn
    enabled: bool,
    /// Draw on the last screen row (below the docked box) instead of the current line
    docked: bool,
    /// Key hint after the clock: key and what it does, e.g. ("Esc", "to stop")
    hint: Option<(&'static str, &'static str)>,
    /// The approval mode shown at the end of the line (true = allow all)
    mode: Option<Arc<AtomicBool>>,
    /// Who answers, shown after the mode (true = the mock)
    model: Option<Arc<AtomicBool>>,
    style: Style,
    started: bool,
}

fn last_row() -> u16 {
    crossterm::terminal::size().map_or(24, |(_, rows)| rows)
}

impl Spinner {
    pub fn new(enabled: bool, style: Style) -> Self {
        let state = State {
            label: None,
            since: Instant::now(),
            frame: 0,
            drawn: false,
            notice: None,
        };
        Self {
            state: Arc::new(Mutex::new(state)),
            enabled,
            docked: false,
            hint: None,
            mode: None,
            model: None,
            style,
            started: false,
        }
    }

    /// From now on the line lives on the last screen row and never touches the output.
    /// Call before the first `start`.
    pub fn dock(&mut self) {
        self.docked = true;
    }

    pub fn docked(&self) -> bool {
        self.docked
    }

    /// Call before the first `start`.
    pub fn set_hint(&mut self, key: &'static str, what: &'static str) {
        self.hint = Some((key, what));
    }

    /// Call before the first `start`.
    pub fn set_mode(&mut self, mode: Arc<AtomicBool>) {
        self.mode = Some(mode);
    }

    pub fn notifier(&self) -> Notifier {
        Notifier(self.state.clone())
    }

    /// Call before the first `start`.
    pub fn set_model(&mut self, model: Arc<AtomicBool>) {
        self.model = Some(model);
    }

    /// Shows `label` with a running clock. A new label keeps the clock when one runs.
    pub fn start(&mut self, label: &str) {
        if !self.enabled {
            return;
        }
        {
            let mut state = self.state.lock().unwrap();
            if state.label.is_none() {
                state.since = Instant::now();
            }
            state.label = Some(label.to_string());
        }
        if std::mem::replace(&mut self.started, true) {
            return;
        }
        let (state, style, docked, hint) = (self.state.clone(), self.style, self.docked, self.hint);
        let mode = self.mode.clone();
        let model = self.model.clone();
        // A plain thread: it must tick also while the async runtime is busy.
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(TICK);
                let mut state = state.lock().unwrap();
                let Some(label) = state.label.clone() else {
                    continue;
                };
                state.frame = (state.frame + 1) % FRAMES.len();
                state.drawn = true;
                let secs = state.since.elapsed().as_secs();
                let dot = format!("{} · {}", style.faint(), style.reset());
                let mut suffix = match (&state.notice, hint) {
                    (Some((text, at)), _) if at.elapsed() < NOTICE => {
                        format!("{dot}{}{text}{}", style.action(), style.reset())
                    }
                    (_, Some((key, what))) => format!("{dot}{}", style.hint(key, what)),
                    _ => String::new(),
                };
                if let Some(mode) = &mode {
                    suffix.push_str(&format!(
                        "{dot}{}",
                        style.mode_label(mode.load(Ordering::Relaxed))
                    ));
                }
                if let Some(model) = &model {
                    suffix.push_str(&format!(
                        "{dot}{}",
                        style.model_label(model.load(Ordering::Relaxed))
                    ));
                }
                let line = format!(
                    "{}{}{} {label}… {}{secs}s{}{suffix}",
                    style.accent(),
                    FRAMES[state.frame],
                    style.reset(),
                    style.gray(),
                    style.reset()
                );
                if docked {
                    // Save cursor, write the last row, restore: the output is not touched.
                    screen_write(&format!("\x1b7\x1b[{};1H\x1b[2K  {line}\x1b8", last_row()));
                } else {
                    let _screen = screen_lock();
                    let mut err = std::io::stderr().lock();
                    let _ = write!(err, "\r\x1b[2K{line}");
                    let _ = err.flush();
                }
            }
        });
    }

    /// Removes the line. Inline: call before any other output.
    pub fn stop(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.label = None;
        if !std::mem::take(&mut state.drawn) {
            return;
        }
        if self.docked {
            screen_write(&format!("\x1b7\x1b[{};1H\x1b[2K\x1b8", last_row()));
        } else {
            let _screen = screen_lock();
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r\x1b[2K");
            let _ = err.flush();
        }
    }
}
