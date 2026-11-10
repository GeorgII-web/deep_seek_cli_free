//! One status line that shows what is being waited for and for how long.

use super::style::Style;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(90);

struct State {
    label: Option<String>,
    since: Instant,
    frame: usize,
    /// The line is on the screen and must be erased before other output
    drawn: bool,
}

pub struct Spinner {
    state: Arc<Mutex<State>>,
    /// Off without a terminal: nothing is ever drawn
    enabled: bool,
    style: Style,
    started: bool,
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
            style,
            started: false,
        }
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
        if !std::mem::replace(&mut self.started, true) {
            let (state, style) = (self.state.clone(), self.style);
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
                    let mut err = std::io::stderr().lock();
                    let _ = write!(
                        err,
                        "\r\x1b[2K{}{}{} {label}… {}{secs}s{}",
                        style.accent(),
                        FRAMES[state.frame],
                        style.reset(),
                        style.gray(),
                        style.reset()
                    );
                    let _ = err.flush();
                }
            });
        }
    }

    /// Removes the line; call before any other output.
    pub fn stop(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.label = None;
        if std::mem::take(&mut state.drawn) {
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r\x1b[2K");
            let _ = err.flush();
        }
    }
}
