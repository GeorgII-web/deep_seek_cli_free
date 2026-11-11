//! One status line that shows what is being waited for and for how long.

use super::scrollback::View;
use super::style::Style;
use super::{screen_lock, screen_write};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(90);
/// Short words for the bottom line while work goes on.
const WORDS: [&str; 20] = [
    "Thinking",
    "Pondering",
    "Brewing",
    "Crunching",
    "Digging",
    "Weaving",
    "Tinkering",
    "Musing",
    "Cooking",
    "Scribbling",
    "Hunting",
    "Polishing",
    "Untangling",
    "Noodling",
    "Conjuring",
    "Mulling",
    "Sifting",
    "Plotting",
    "Wrangling",
    "Simmering",
];
/// Seconds one word stays before the next one.
const WORD_SECS: u64 = 6;

/// The word for the bottom line: it starts at `seed` and moves on every `WORD_SECS`.
fn busy_word(seed: usize, secs: u64) -> &'static str {
    WORDS[(seed + (secs / WORD_SECS) as usize) % WORDS.len()]
}

/// The key that switches the mode, shown next to it (the same text as under the idle box).
const MODE_KEY: &str = "Shift + Tab";
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
    /// Where in `WORDS` the bottom line starts for this task
    seed: usize,
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
    /// Key hint after the clock: key and what it does, e.g. ("Esc", "interrupt")
    hint: Option<(&'static str, &'static str)>,
    /// The approval mode shown at the end of the line (true = allow all)
    mode: Option<Arc<AtomicBool>>,
    /// Who answers, shown after the mode (true = the mock)
    model: Option<Arc<AtomicBool>>,
    /// Screen layout: the output, whose running block gets the same spinner frame
    view: Option<Arc<Mutex<View>>>,
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
            seed: 0,
        };
        Self {
            state: Arc::new(Mutex::new(state)),
            enabled,
            docked: false,
            hint: None,
            mode: None,
            model: None,
            view: None,
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

    /// Call before the first `start`.
    pub fn set_view(&mut self, view: Arc<Mutex<View>>) {
        self.view = Some(view);
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
                // A new word for a new piece of work; the clock is random enough.
                state.seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.subsec_nanos() as usize);
            }
            state.label = Some(label.to_string());
        }
        if std::mem::replace(&mut self.started, true) {
            return;
        }
        let (state, style, docked, hint) = (self.state.clone(), self.style, self.docked, self.hint);
        let mode = self.mode.clone();
        let model = self.model.clone();
        let view = self.view.clone();
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
                // What follows the progress text: a notice for a moment, else the key hint.
                let (after, after_columns) = match (&state.notice, hint) {
                    (Some((text, at)), _) if at.elapsed() < NOTICE => (
                        format!("{dot}{}{text}{}", style.action(), style.reset()),
                        3 + text.chars().count(),
                    ),
                    (_, Some((key, what))) => (
                        format!("{dot}{}", style.hint(key, what)),
                        3 + key.chars().count() + 1 + what.chars().count(),
                    ),
                    _ => (String::new(), 0),
                };
                // Model, then mode: the same as under the idle box, with their columns.
                let mut labels = String::new();
                let mut label_columns = 0;
                if let Some(model) = &model {
                    let mock = model.load(Ordering::Relaxed);
                    labels.push_str(&style.model_label(mock));
                    label_columns += if mock { 6 } else { 10 };
                }
                if let Some(mode) = &mode {
                    let allow_all = mode.load(Ordering::Relaxed);
                    if !labels.is_empty() {
                        labels.push_str(&dot);
                        label_columns += 3;
                    }
                    labels.push_str(&style.mode_label(allow_all));
                    label_columns += if allow_all { 12 } else { 5 };
                    // The key that switches the mode works while a task runs too.
                    if docked {
                        labels.push_str(&format!("{dot}{}", style.hint(MODE_KEY, "")));
                        label_columns += 3 + MODE_KEY.chars().count();
                    }
                }
                let frame_text = FRAMES[state.frame];
                if docked {
                    // The row under the box keeps its shape while work goes on: model and
                    // mode stay at the left, where they are when the box is idle; the icon
                    // with a word that changes now and then, and the key to interrupt, are
                    // at the right edge. The step and its seconds are in the output itself.
                    let word = busy_word(state.seed, secs);
                    let progress_columns = 1 + 1 + word.chars().count() + 1 + after_columns;
                    let columns =
                        crossterm::terminal::size().map_or(80, |(columns, _)| columns as usize);
                    let room = columns.saturating_sub(3);
                    let gap =
                        " ".repeat(room.saturating_sub(label_columns + progress_columns).max(2));
                    let line = format!(
                        "{labels}{gap}{}{frame_text} {word}…{}{after}",
                        style.accent(),
                        style.reset()
                    );
                    // Save cursor, write the last row, restore: the output is not touched.
                    screen_write(&format!("\x1b7\x1b[{};1H\x1b[2K  {line}\x1b8", last_row()));
                } else {
                    // Inline it is the only progress line: the real step and its clock.
                    let labels = if labels.is_empty() {
                        String::new()
                    } else {
                        format!("{dot}{labels}")
                    };
                    let line = format!(
                        "{}{frame_text} {label}… {secs}s{}{after}{labels}",
                        style.accent(),
                        style.reset()
                    );
                    let _screen = screen_lock();
                    let mut err = std::io::stderr().lock();
                    let _ = write!(err, "\r\x1b[2K{line}");
                    let _ = err.flush();
                }
                // The step in progress shows the same frame in the output. The state
                // lock is released first: the view has its own lock.
                let frame = FRAMES[state.frame];
                drop(state);
                if let Some(view) = &view {
                    let mut view = view.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    // A running action: its header. Else the status line at the start of the answer.
                    view.animate(frame);
                    view.animate_status(frame, &label);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_busy_word_changes_with_time_and_with_the_seed() {
        assert_eq!(busy_word(0, 0), "Thinking");
        assert_eq!(busy_word(0, WORD_SECS - 1), "Thinking");
        assert_eq!(busy_word(0, WORD_SECS), "Pondering");
        assert_eq!(busy_word(3, 0), "Crunching");
        // It wraps around.
        assert_eq!(busy_word(WORDS.len() - 1, WORD_SECS), "Thinking");
        assert!(WORDS.iter().all(|word| word.len() <= 12));
    }
}
