//! One status line that shows what is being waited for and for how long.

use super::style::Style;
use super::{screen_lock, screen_write};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(90);

struct State {
    label: Option<String>,
    since: Instant,
    frame: usize,
    /// The line is on the screen and must be erased
    drawn: bool,
}

pub struct Spinner {
    state: Arc<Mutex<State>>,
    /// Off without a terminal: nothing is ever drawn
    enabled: bool,
    /// Draw on the last screen row (below the docked box) instead of the current line
    docked: bool,
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
        };
        Self {
            state: Arc::new(Mutex::new(state)),
            enabled,
            docked: false,
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
        let (state, style, docked) = (self.state.clone(), self.style, self.docked);
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
                let line = format!(
                    "{}{}{} {label}… {}{secs}s{}",
                    style.accent(),
                    FRAMES[state.frame],
                    style.reset(),
                    style.gray(),
                    style.reset()
                );
                if docked {
                    // Save cursor, write the last row, restore: the output is not touched.
                    screen_write(&format!(
                        "\x1b7\x1b[{};1H\x1b[2K  {line}{} · Ctrl-C to stop{}\x1b8",
                        last_row(),
                        style.gray(),
                        style.reset()
                    ));
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
