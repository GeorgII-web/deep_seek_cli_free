//! The multiline input box of the chat, and single-key questions.
//!
//! `Buffer` and the wrapping are pure and tested; `Editor` draws them with
//! crossterm in raw mode, on stderr, below the scrollback (no full-screen mode).

use super::scrollback::View;
use super::style::Style;
use super::{screen_lock, screen_write};
use crate::attach::{self, Attachment, MAX_FILES};
use crossterm::cursor::{MoveToColumn, MoveUp};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseButton, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{execute, queue};
use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

/// A second Ctrl-C within this time leaves the app.
pub const AGAIN: Duration = Duration::from_secs(2);
pub const AGAIN_HINT: &str = "Press Ctrl-C again to exit";
/// Entries kept in the history file.
const HISTORY_MAX: usize = 200;
/// Columns taken by the frame: `│ > ` on the left, ` │` on the right.
const FRAME_COLS: usize = 6;

fn width_of(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// Text with a cursor; positions count characters.
#[derive(Default, Debug, Clone)]
pub struct Buffer {
    chars: Vec<char>,
    cursor: usize,
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        let mut buffer = Self::default();
        buffer.insert_str(text);
        buffer
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn insert(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    /// Pasted text: line ends unified, tabs to spaces, control characters dropped.
    pub fn insert_str(&mut self, text: &str) {
        for c in text.replace("\r\n", "\n").replace('\r', "\n").chars() {
            match c {
                '\t' => (0..4).for_each(|_| self.insert(' ')),
                '\n' => self.insert('\n'),
                c if c.is_control() => {}
                c => self.insert(c),
            }
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.chars.len());
    }

    /// Deletes the word before the cursor.
    pub fn delete_word(&mut self) {
        while self.cursor > 0 && self.chars[self.cursor - 1].is_whitespace() {
            self.backspace();
        }
        while self.cursor > 0 && !self.chars[self.cursor - 1].is_whitespace() {
            self.backspace();
        }
    }

    fn line_start(&self) -> usize {
        self.chars[..self.cursor]
            .iter()
            .rposition(|&c| c == '\n')
            .map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.chars[self.cursor..]
            .iter()
            .position(|&c| c == '\n')
            .map_or(self.chars.len(), |i| self.cursor + i)
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start();
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end();
    }

    pub fn kill_to_start(&mut self) {
        let start = self.line_start();
        self.chars.drain(start..self.cursor);
        self.cursor = start;
    }

    pub fn kill_to_end(&mut self) {
        let end = self.line_end();
        self.chars.drain(self.cursor..end);
    }

    /// The character before the cursor, e.g. a `\` that asks for a new line.
    pub fn before_cursor(&self) -> Option<char> {
        self.cursor.checked_sub(1).map(|i| self.chars[i])
    }

    /// Screen rows as character ranges (end exclusive, without the line end):
    /// split at `\n` and wrapped at `width` columns.
    pub fn rows(&self, width: usize) -> Vec<(usize, usize)> {
        let width = width.max(1);
        let (mut rows, mut start, mut used) = (Vec::new(), 0, 0);
        for (i, &c) in self.chars.iter().enumerate() {
            if c == '\n' {
                rows.push((start, i));
                (start, used) = (i + 1, 0);
                continue;
            }
            if used + width_of(c) > width {
                rows.push((start, i));
                (start, used) = (i, 0);
            }
            used += width_of(c);
        }
        rows.push((start, self.chars.len()));
        rows
    }

    /// Row and column of the cursor in `rows`.
    pub fn locate(&self, rows: &[(usize, usize)]) -> (usize, usize) {
        let row = rows
            .iter()
            .rposition(|&(start, _)| start <= self.cursor)
            .unwrap_or(0);
        let column = self.chars[rows[row].0..self.cursor]
            .iter()
            .map(|&c| width_of(c))
            .sum();
        (row, column)
    }

    /// Moves one screen row up (`-1`) or down (`1`), keeping the column.
    /// Returns false at the edge, so the caller can go through the history.
    pub fn vertical(&mut self, delta: isize, width: usize) -> bool {
        let rows = self.rows(width);
        let (row, column) = self.locate(&rows);
        let Some(&(start, end)) = row
            .checked_add_signed(delta)
            .and_then(|target| rows.get(target))
        else {
            return false;
        };
        let (mut index, mut used) = (start, 0);
        while index < end && used + width_of(self.chars[index]) <= column {
            used += width_of(self.chars[index]);
            index += 1;
        }
        self.cursor = index;
        true
    }

    fn row_text(&self, (start, end): (usize, usize)) -> String {
        self.chars[start..end].iter().collect()
    }
}

/// Raw mode for the time a key is awaited; restored on drop, also on an error.
pub(super) struct RawMode;

impl RawMode {
    pub(super) fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        // The keyboard flags make terminals that know them report Shift+Enter
        // apart from Enter; the others ignore the request.
        let _ = execute!(
            io::stderr(),
            EnableBracketedPaste,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = execute!(
            io::stderr(),
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste
        );
        let _ = terminal::disable_raw_mode();
    }
}

/// A prompt sent while a task ran: it waits for its turn.
#[derive(Debug, Clone)]
pub struct Queued {
    pub text: String,
    pub files: Vec<Attachment>,
}

/// The prompts waiting for their turn, shared by the box and the chat loop.
pub type Queue = Arc<Mutex<VecDeque<Queued>>>;
/// Most prompts the queue holds.
pub const QUEUE_MAX: usize = 5;
/// Empty columns after the last label of the bottom row (`Shift + Tab`).
pub const LABELS_MARGIN: usize = 1;
/// Rows above the queued prompts: an empty one (apart from the output), the title.
const QUEUE_HEAD_ROWS: usize = 2;

/// What an event while a task runs did (`Editor::busy_event`).
#[derive(Debug, PartialEq)]
pub enum Busy {
    Nothing,
    /// A prompt went into the queue
    Queued,
    /// Enter, but the queue is full: the text stays in the box
    Full,
    /// A click on Think or Browse: they switch between tasks only
    Later,
}

/// The state of one reading of the box: the text, the place in the history,
/// the command menu and an armed Ctrl-C. The chat prompt's state lives on
/// between tasks, so what was typed while one ran is still there.
struct Typing {
    buffer: Buffer,
    /// Index into the history while going through it; its length = the draft
    position: usize,
    /// The text before going into the history
    draft: String,
    /// Time of a Ctrl-C that a second one within `AGAIN` turns into an exit
    armed: Option<Instant>,
    /// The command menu: the chosen row, the text for which Esc closed it, the text it was made for
    selected: usize,
    closed_for: Option<String>,
    menu_text: String,
}

impl Typing {
    fn new(initial: &str, history: usize) -> Self {
        Self {
            buffer: Buffer::new(initial),
            position: history,
            draft: String::new(),
            armed: None,
            selected: 0,
            closed_for: None,
            menu_text: String::new(),
        }
    }
}

/// A label of the bottom row that switches something when clicked, like its command.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Toggle {
    Think,
    Browse,
    Mode,
}

impl Toggle {
    /// The chat command that does the same.
    pub fn command(self) -> &'static str {
        match self {
            Self::Think => "/think",
            Self::Browse => "/browse",
            Self::Mode => "/permissions",
        }
    }
}

/// What an event did to the box.
enum Step {
    Continue,
    Submit(String),
    /// A label of the bottom row was clicked
    Toggle(Toggle),
    Exit,
}

pub enum Input {
    Submit(String),
    /// Ctrl-C twice, or Ctrl-D on an empty box
    Exit,
}

/// The box fixed at the bottom of the screen: the rows above it are a scroll
/// region, so output scrolls there (and into the terminal's scrollback) while
/// the box stays.
struct Dock {
    /// Rows reserved at the bottom: borders, text rows, status line; 0 = none yet
    area: usize,
    /// Terminal height the layout was made for
    height: u16,
    /// Where output continues: row and column, 1-based
    out: (u16, u16),
}

/// Rows of the dock besides the text: top border, bottom border, status line.
const DOCK_EXTRA: usize = 3;
/// Most text rows the docked box shows; more scroll inside it.
const DOCK_MAX_ROWS: usize = 10;

pub struct Editor {
    style: Style,
    history: Vec<String>,
    history_file: Option<PathBuf>,
    /// Inline box: terminal rows between its top and the cursor, from the last draw
    cursor_line: u16,
    /// Set while the box is docked at the bottom (chat)
    dock: Option<Dock>,
    /// The approval mode (true = allow): shown in the hint line, Shift+Tab switches it
    mode: Option<Arc<AtomicBool>>,
    /// Who answers (true = the mock): shown in the hint line next to the mode
    model: Option<Arc<AtomicBool>>,
    /// Deep thinking on or off: shown after the model
    think: Option<Arc<AtomicBool>>,
    /// Browse (DeepSeek's web search) on or off: shown after Think
    browse: Option<Arc<AtomicBool>>,
    /// Slash commands offered in a menu while the input is the start of one: name, meaning
    commands: &'static [Command],
    /// Docked: the scrollable output above the dock; the box tells it the size of
    /// its area and passes it the scroll keys
    view: Option<Arc<Mutex<View>>>,
    /// Files dropped on the chat box, sent with the next prompt
    files: Vec<Attachment>,
    /// The files of the input just submitted: out of the box before it is drawn
    /// empty, until `take_files`
    sent: Vec<Attachment>,
    /// The chat prompt as typed so far, kept while a task runs and between reads
    typing: Option<Typing>,
    /// Prompts sent while a task ran (`busy_event`); the chat loop takes them first
    queue: Queue,
    /// A task runs: the hint row belongs to the progress line, the cursor is drawn by the box
    busy: bool,
    /// A message about the last drop (a file that could not be attached), shown
    /// in the hint line until the next key
    note: Option<String>,
}

/// The row text with the character at `column` (a space at the end) shown inverse:
/// the box's own cursor while the real one stays in the output.
fn with_cursor(text: &str, column: usize, s: Style) -> String {
    let mut out = String::new();
    let mut used = 0;
    let mut placed = false;
    for c in text.chars() {
        if !placed && used >= column {
            out.push_str(&format!("{}{c}{}", s.inverse(), s.reset()));
            placed = true;
        } else {
            out.push(c);
        }
        used += width_of(c);
    }
    if !placed {
        out.push_str(&format!("{} {}", s.inverse(), s.reset()));
    }
    out
}

/// A slash command: name with the slash, what it does.
pub type Command = (&'static str, &'static str);

/// The key that switches the mode: its hint stays on the left, next to the mode.
const MODE_KEY: &str = "Shift + Tab";
const MENU_HINTS: &[(&str, &str)] = &[
    ("↑↓", "choose"),
    ("Enter", "run"),
    ("Tab", "complete"),
    ("Esc", "close"),
];

/// The commands the menu offers for `text`: those starting with it, while `text`
/// is one word starting with `/`. A command typed out in full offers nothing,
/// so Enter sends it.
pub fn command_matches(commands: &'static [Command], text: &str) -> Vec<Command> {
    if !text.starts_with('/') || text.contains(char::is_whitespace) {
        return Vec::new();
    }
    let found: Vec<Command> = commands
        .iter()
        .copied()
        .filter(|(name, _)| name.starts_with(text))
        .collect();
    match found.as_slice() {
        [(name, _)] if *name == text => Vec::new(),
        _ => found,
    }
}

impl Editor {
    /// `history_file` = JSON list of earlier inputs; `None` keeps the history in memory.
    pub fn new(style: Style, history_file: Option<PathBuf>) -> Self {
        let history = history_file
            .as_ref()
            .and_then(|file| std::fs::read_to_string(file).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            style,
            history,
            history_file,
            cursor_line: 0,
            dock: None,
            mode: None,
            model: None,
            think: None,
            browse: None,
            commands: &[],
            view: None,
            files: Vec::new(),
            sent: Vec::new(),
            note: None,
            typing: None,
            queue: Arc::new(Mutex::new(VecDeque::new())),
            busy: false,
        }
    }

    /// The files dropped on the box for the input just submitted; the box is empty after.
    /// The queue of this box, for the chat loop.
    pub fn queue(&self) -> Queue {
        self.queue.clone()
    }

    pub fn take_files(&mut self) -> Vec<Attachment> {
        std::mem::take(&mut self.sent)
    }

    /// Adds dropped files (no file twice, at most `MAX_FILES`); what was left out goes to the note.
    fn add_files(&mut self, dropped: attach::Dropped) {
        let mut left_out = dropped.skipped;
        for file in dropped.files {
            if self.files.iter().any(|known| known.path == file.path) {
                continue;
            }
            if self.files.len() >= MAX_FILES {
                left_out.push(format!("{}: more than {MAX_FILES} files", file.name));
                continue;
            }
            self.files.push(file);
        }
        self.note =
            (!left_out.is_empty()).then(|| format!("Not attached: {}", left_out.join("; ")));
    }

    /// The top border's title: the attached files, flagged ones marked; a summary
    /// when they do not fit in `max` columns. Text and its width.
    fn files_title(&self, max: usize) -> (String, usize) {
        let s = self.style;
        if self.files.is_empty() {
            return (String::new(), 0);
        }
        let columns = |text: &str| text.chars().map(width_of).sum::<usize>();
        let mut text = String::new();
        let mut used = 0;
        for (i, file) in self.files.iter().enumerate() {
            let item = match file.warning {
                Some(_) => format!("⚠ {}", file.label()),
                None => format!("📎 {}", file.label()),
            };
            let separator = if i == 0 { 0 } else { 2 };
            used += separator + columns(&item);
            let color = if file.warning.is_some() {
                s.action()
            } else {
                s.key()
            };
            text.push_str(&format!(
                "{}{color}{item}{}",
                "  ".repeat(separator / 2),
                s.reset()
            ));
        }
        if used <= max {
            return (text, used);
        }
        let total: u64 = self.files.iter().map(|file| file.size).sum();
        let flagged = self
            .files
            .iter()
            .filter(|file| file.warning.is_some())
            .count();
        let mut summary = format!(
            "📎 {} files · {}",
            self.files.len(),
            attach::size_label(total)
        );
        if flagged > 0 {
            summary.push_str(&format!(" · ⚠ {flagged} private"));
        }
        let used = columns(&summary);
        match used <= max {
            true => (format!("{}{summary}{}", s.key(), s.reset()), used),
            false => (String::new(), 0),
        }
    }

    pub fn set_view(&mut self, view: Arc<Mutex<View>>) {
        self.view = Some(view);
    }

    pub fn set_browse(&mut self, browse: Arc<AtomicBool>) {
        self.browse = Some(browse);
    }

    pub fn set_think(&mut self, think: Arc<AtomicBool>) {
        self.think = Some(think);
    }

    pub fn set_model(&mut self, model: Arc<AtomicBool>) {
        self.model = Some(model);
    }

    pub fn set_commands(&mut self, commands: &'static [Command]) {
        self.commands = commands;
    }

    /// The menu rows: the selected command marked and in the accent color.
    fn menu_lines(&self, items: &[Command], selected: usize, max: usize) -> Vec<String> {
        let s = self.style;
        let name_columns = items
            .iter()
            .map(|(name, _)| name.chars().count())
            .max()
            .unwrap_or(0)
            + 3;
        items
            .iter()
            .enumerate()
            .map(|(i, (name, what))| {
                let pad = " ".repeat(name_columns - name.chars().count());
                let what: String = what
                    .chars()
                    .take(max.saturating_sub(name_columns + 4))
                    .collect();
                // Every row has a background over the width of the box, so the list
                // stands out from the output even when it is a single row.
                let fill = " ".repeat(max.saturating_sub(4 + name_columns + what.chars().count()));
                match i == selected {
                    true => {
                        let bg = s.menu_selected_bg();
                        format!(
                            "{bg}  {}{}❯ {name}{}{bg}{pad}{}{what}{fill}{}",
                            s.bold(),
                            s.accent(),
                            s.reset(),
                            s.key(),
                            s.reset()
                        )
                    }
                    false => {
                        let bg = s.menu_bg();
                        format!(
                            "{bg}    {}{name}{}{bg}{pad}{}{what}{fill}{}",
                            s.key(),
                            s.reset(),
                            s.faint(),
                            s.reset()
                        )
                    }
                }
            })
            .collect()
    }

    pub fn set_mode(&mut self, mode: Arc<AtomicBool>) {
        self.mode = Some(mode);
    }

    /// The line under the box, `max` columns wide: on the left the mode, the model
    /// and the key that switches the mode, on the right (up to the box edge) as many of the other
    /// `(key, meaning)` hints as fit.
    /// The labels at the right of the bottom row, left to right: the model, deep
    /// thinking, Browse, the mode and (`with_key`) the key that switches the mode.
    /// Text, columns, and what a click on it switches. The progress line of a
    /// running task shows the same labels at the same place (`Spinner`).
    fn right_labels(&self, with_key: bool) -> Vec<(String, usize, Option<Toggle>)> {
        let s = self.style;
        let mut labels = Vec::new();
        if let Some(model) = &self.model {
            let mock = model.load(Ordering::Relaxed);
            labels.push((s.model_label(mock), if mock { 6 } else { 10 }, None));
        }
        if let Some(think) = &self.think {
            labels.push((
                s.think_label(think.load(Ordering::Relaxed)),
                7,
                Some(Toggle::Think),
            ));
        }
        if let Some(browse) = &self.browse {
            labels.push((
                s.browse_label(browse.load(Ordering::Relaxed)),
                8,
                Some(Toggle::Browse),
            ));
        }
        if let Some(mode) = &self.mode {
            let allow_all = mode.load(Ordering::Relaxed);
            labels.push((
                s.mode_label(allow_all),
                if allow_all { 8 } else { 5 },
                Some(Toggle::Mode),
            ));
            if with_key {
                labels.push((s.hint(MODE_KEY, ""), MODE_KEY.chars().count(), None));
            }
        }
        labels
    }

    /// The label of the bottom row under a click at `column` (0-based) on that row.
    /// The right labels end where the box ends: column `2 + max` of the row.
    fn label_at(&self, column: usize, with_key: bool) -> Option<Toggle> {
        let labels = self.right_labels(with_key);
        let max = Self::inner_width() + FRAME_COLS - 2;
        let total: usize = labels.iter().map(|(_, columns, _)| columns).sum::<usize>()
            + 3 * labels.len().saturating_sub(1);
        let mut at = (2 + max).checked_sub(total + LABELS_MARGIN)?;
        for (_, columns, toggle) in labels {
            if column >= at && column < at + columns {
                return toggle;
            }
            at += columns + 3;
        }
        None
    }

    fn hint_line(&self, hints: &[(&str, &str)], max: usize) -> String {
        let s = self.style;
        // The right labels stop `LABELS_MARGIN` columns before the box edge.
        let max = max.saturating_sub(LABELS_MARGIN);
        let dot = format!("{} · {}", s.faint(), s.reset());
        // Right: the model, deep thinking, Browse, then the mode with its key right
        // after it (the key belongs to the mode). Left: the other keys, as many as fit.
        let with_key = self.mode.is_some() && hints.iter().any(|(key, _)| *key == MODE_KEY);
        let mut right = String::new();
        let mut right_columns = 0;
        for (text, columns, _) in self.right_labels(with_key) {
            if !right.is_empty() {
                right.push_str(&dot);
                right_columns += 3;
            }
            right.push_str(&text);
            right_columns += columns;
        }
        let (mut left, mut left_columns) = (String::new(), 0);
        for (key, what) in hints {
            if *key == MODE_KEY && self.mode.is_some() {
                continue;
            }
            let item = key.chars().count() + what.chars().count() + usize::from(!what.is_empty());
            let separator = if left.is_empty() { 0 } else { 3 };
            // Two columns stay between the sides.
            if left_columns + separator + item + right_columns + 2 > max {
                break;
            }
            if separator > 0 {
                left.push_str(&dot);
            }
            left.push_str(&s.hint(key, what));
            left_columns += separator + item;
        }
        let gap = max.saturating_sub(left_columns + right_columns);
        format!("{left}{}{right}", " ".repeat(gap))
    }

    /// The cursor as row and column, 1-based; `None` when the terminal does not answer.
    fn cursor() -> Option<(u16, u16)> {
        crossterm::cursor::position()
            .ok()
            .map(|(column, row)| (row + 1, column + 1))
    }

    /// Docks the box at the bottom from now on; output continues at the cursor.
    pub fn open_dock(&mut self) -> io::Result<()> {
        let (_, height) = terminal::size()?;
        self.dock = Some(Dock {
            area: 0,
            height,
            out: Self::cursor().unwrap_or((1, 1)),
        });
        self.dock_idle()
    }

    /// Gives the whole screen back: no scroll region, no box. The cursor stays
    /// where the output ended.
    pub fn close_dock(&mut self) {
        let Some(dock) = self.dock.take() else {
            return;
        };
        // Resetting the region moves the cursor home, so it is saved around it.
        let mut frame = String::from("\x1b7\x1b[r");
        for row in (dock.height as usize - dock.area + 1)..=dock.height as usize {
            frame.push_str(&format!("\x1b[{row};1H\x1b[2K"));
        }
        frame.push_str("\x1b8");
        screen_write(&frame);
    }

    /// Reserves `area` rows at the bottom of a terminal `height` rows high and
    /// clears them. With `push`, growing moves the output up first, so nothing is
    /// covered; without it the caller draws the output again (`sync_view`).
    fn set_area(dock: &mut Dock, area: usize, height: u16, push: bool, frame: &mut String) {
        let rows = height as usize;
        let area = area.min(rows.saturating_sub(1));
        let bottom = (rows - area) as u16;
        let mut clear_from = bottom;
        if height != dock.height {
            // Resized: the old layout means nothing; output continues inside the new region.
            dock.out.0 = dock.out.0.min(bottom);
        } else {
            let old_bottom = (rows - dock.area) as u16;
            if push && dock.out.0 > bottom {
                let lines = (dock.out.0 - bottom) as usize;
                frame.push_str(&format!("\x1b[{old_bottom};1H{}", "\n".repeat(lines)));
                dock.out.0 = bottom;
            }
            clear_from = bottom.min(old_bottom);
        }
        frame.push_str(&format!("\x1b[1;{bottom}r"));
        for row in clear_from + 1..=height {
            frame.push_str(&format!("\x1b[{row};1H\x1b[2K"));
        }
        dock.area = area;
        dock.height = height;
    }

    /// Draws the dock again from nothing, after the screen was cleared.
    pub fn redraw_dock(&mut self) {
        if let Some(dock) = &mut self.dock {
            // An unknown height makes the layout start over: region, cleared rows, box.
            dock.height = 0;
        }
        let _ = self.dock_idle();
    }

    /// Tells the scrollable output the size of its area (the screen above the
    /// dock), draws it again when that changed, and takes from it where output continues.
    fn sync_view(&mut self) {
        let (Some(view), Some(dock)) = (&self.view, &mut self.dock) else {
            return;
        };
        let columns = terminal::size().map_or(80, |(columns, _)| columns as usize);
        let queued = match self.queue.lock().unwrap_or_else(|p| p.into_inner()).len() {
            0 => 0,
            n => QUEUE_HEAD_ROWS + n,
        };
        let mut view = view.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        view.set_above_border(queued);
        if view.resize(columns, dock.height as usize - dock.area) {
            view.repaint();
        }
        dock.out = view.out();
    }

    /// The docked box at rest, with the cursor back in the output: the queue, the
    /// text typed so far (or nothing) and the files; no hint row (the progress
    /// line uses it). While a task runs the box shows its own cursor.
    fn dock_idle(&mut self) -> io::Result<()> {
        let width = Self::inner_width();
        let buffer = self
            .typing
            .as_ref()
            .map(|typing| typing.buffer.clone())
            .unwrap_or_default();
        self.draw_docked(&buffer, width, &[], None)
    }

    /// The dock: queue rows, the box, the command menu, the hint row. With a hint the
    /// cursor goes into the box (the box reads keys); without one the hint row is
    /// left to the progress line and the cursor goes back to the output.
    fn draw_docked(
        &mut self,
        buffer: &Buffer,
        width: usize,
        menu: &[String],
        hint: Option<&str>,
    ) -> io::Result<()> {
        let (_, height) = terminal::size()?;
        let rows = buffer.rows(width);
        let (cursor_row, cursor_column) = buffer.locate(&rows);
        let visible = rows
            .len()
            .min((height as usize / 3).clamp(1, DOCK_MAX_ROWS));
        let first = (cursor_row + 1).saturating_sub(visible);
        // A running task keeps the real cursor in the output: the box draws its own.
        let fake = (self.busy && hint.is_none()).then_some((cursor_row, cursor_column));
        let lines = self.box_lines(buffer, width, first, visible, fake);
        let queue = self.queue_lines(width + FRAME_COLS - 1);
        let push = self.view.is_none();
        let Some(dock) = &mut self.dock else {
            return Ok(());
        };
        let mut frame = String::new();
        Self::set_area(
            dock,
            queue.len() + visible + DOCK_EXTRA + menu.len(),
            height,
            push,
            &mut frame,
        );
        screen_write(&frame);
        self.sync_view();
        let Some(dock) = &self.dock else {
            return Ok(());
        };
        let mut frame = String::new();
        let top = height as usize - dock.area + 1;
        // The queue, the box, then the command menu, then the hint on the last row.
        for (i, line) in queue.iter().chain(&lines).chain(menu).enumerate() {
            frame.push_str(&format!("\x1b[{};1H{line}", top + i));
        }
        match hint {
            Some(hint) => {
                frame.push_str(&format!("\x1b[{height};1H  {hint}"));
                frame.push_str(&format!(
                    "\x1b[{};{}H",
                    top + queue.len() + 1 + cursor_row - first,
                    5 + cursor_column
                ));
            }
            None => frame.push_str(&format!("\x1b[{};{}H", dock.out.0, dock.out.1)),
        }
        screen_write(&frame);
        if let Some(view) = &self.view {
            view.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .indicator();
        }
        Ok(())
    }

    /// Top border, `count` text rows from row `first`, bottom border. `cursor` =
    /// row and column where the box draws a cursor of its own (inverse).
    fn box_lines(
        &self,
        buffer: &Buffer,
        width: usize,
        first: usize,
        count: usize,
        cursor: Option<(usize, usize)>,
    ) -> Vec<String> {
        let s = self.style;
        let rows = buffer.rows(width);
        let border = "─".repeat(width + FRAME_COLS - 2);
        // Attached files sit in the top border: `╭─ 📎 a.pdf · 12 KB ───╮`.
        let (title, used) = self.files_title((width + FRAME_COLS - 2).saturating_sub(4));
        let top = match used {
            0 => border.clone(),
            _ => format!(
                "─ {title}{} {}",
                s.gray(),
                "─".repeat(width + FRAME_COLS - 2 - used - 3)
            ),
        };
        let mut lines = vec![format!("{}╭{top}{}╮{}", s.gray(), s.gray(), s.reset())];
        for (i, &row) in rows.iter().enumerate().skip(first).take(count) {
            let mut text = buffer.row_text(row);
            let used: usize = text.chars().map(width_of).sum();
            if let Some((_, column)) = cursor.filter(|(r, _)| *r == i) {
                text = with_cursor(&text, column, s);
            }
            let mark = if i == 0 { ">" } else { " " };
            lines.push(format!(
                "{gray}│{reset} {accent}{mark}{reset} {text}{pad} {gray}│{reset}",
                gray = s.gray(),
                reset = s.reset(),
                accent = s.accent(),
                pad = " ".repeat(width.saturating_sub(
                    used + usize::from(cursor.is_some_and(|(r, c)| r == i && c >= used))
                )),
            ));
        }
        lines.push(format!("{}╰{border}╯{}", s.gray(), s.reset()));
        lines
    }

    fn inner_width() -> usize {
        let columns = terminal::size().map_or(80, |(columns, _)| columns as usize);
        // One column stays free: writing into the last one makes some terminals wrap.
        columns.saturating_sub(1).max(FRAME_COLS + 10) - FRAME_COLS
    }

    /// Shows the box with `initial` in it and reads until Enter. `remember` = the
    /// chat prompt: the input goes into the history, files and the queue belong
    /// to it, and it goes on with what was typed while the last task ran.
    pub fn read(
        &mut self,
        initial: &str,
        hints: &[(&str, &str)],
        remember: bool,
    ) -> io::Result<Input> {
        let _raw = RawMode::enter()?;
        let mut typing = match remember {
            true => self
                .typing
                .take()
                .unwrap_or_else(|| Typing::new(initial, self.history.len())),
            false => Typing::new(initial, self.history.len()),
        };
        self.cursor_line = 0;
        if let Some(dock) = &mut self.dock
            && self.view.is_none()
        {
            // Output moved the cursor since the last draw (with a view, `sync_view` knows it).
            dock.out = Self::cursor().unwrap_or(dock.out);
        }
        loop {
            let width = Self::inner_width();
            let (items, menu, hint) = self.parts(&mut typing, hints, remember, width);
            self.draw(&typing.buffer, width, &menu, &hint)?;
            // The "again" hint disappears by itself.
            if let Some(at) = typing.armed.filter(|at| at.elapsed() < AGAIN)
                && !event::poll(AGAIN.saturating_sub(at.elapsed()))?
            {
                typing.armed = None;
                continue;
            }
            let event = event::read()?;
            match self.handle(&mut typing, event, &items, remember, width) {
                Step::Continue => {}
                Step::Submit(text) => {
                    // The empty box drawn by `erase` shows no files any more.
                    self.sent = std::mem::take(&mut self.files);
                    self.erase()?;
                    if remember {
                        self.remember(&text);
                    }
                    return Ok(Input::Submit(text));
                }
                // As if the command was typed; the text and the files stay in the box.
                Step::Toggle(toggle) => {
                    self.typing = Some(typing);
                    self.erase()?;
                    return Ok(Input::Submit(toggle.command().to_string()));
                }
                Step::Exit => {
                    self.erase()?;
                    return Ok(Input::Exit);
                }
            }
        }
    }

    /// A task is running (`true`) or not. While it runs, `busy_event` gets the keys:
    /// the box can be filled and Enter puts the prompt into the queue.
    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }

    /// An event while a task runs (the keys Esc, Ctrl-C and Shift+Tab stay with the
    /// task's key watcher). Draws the box again.
    pub fn busy_event(&mut self, event: Event) -> Busy {
        let mut typing = self
            .typing
            .take()
            .unwrap_or_else(|| Typing::new("", self.history.len()));
        let width = Self::inner_width();
        let (items, _, _) = self.parts(&mut typing, &[], true, width);
        let mut outcome = Busy::Nothing;
        let step = self.handle(&mut typing, event, &items, true, width);
        // The mode switches at once, like Shift+Tab; the model's switches wait for the task's end.
        if let Step::Toggle(toggle) = step {
            match toggle {
                Toggle::Mode => {
                    if let Some(mode) = &self.mode {
                        mode.fetch_xor(true, Ordering::Relaxed);
                    }
                }
                Toggle::Think | Toggle::Browse => outcome = Busy::Later,
            }
        }
        if let Step::Submit(text) = step {
            let mut queue = self.queue.lock().unwrap_or_else(|p| p.into_inner());
            if queue.len() >= QUEUE_MAX {
                // The text stays in the box until a place is free.
                outcome = Busy::Full;
            } else {
                queue.push_back(Queued {
                    text: text.clone(),
                    files: std::mem::take(&mut self.files),
                });
                drop(queue);
                self.remember(&text);
                typing = Typing::new("", self.history.len());
                outcome = Busy::Queued;
            }
        }
        self.typing = Some(typing);
        let _ = self.dock_idle();
        outcome
    }

    /// Draws the box at rest again (the queue changed outside the box).
    pub fn redraw_idle(&mut self) -> io::Result<()> {
        self.dock_idle()
    }

    /// The command menu items, its rows and the hint line for the box as it is now.
    fn parts(
        &self,
        typing: &mut Typing,
        hints: &[(&str, &str)],
        remember: bool,
        width: usize,
    ) -> (Vec<Command>, Vec<String>, String) {
        let armed_now = typing.armed.is_some_and(|at| at.elapsed() < AGAIN);
        let text = typing.buffer.text();
        if text != typing.menu_text {
            typing.selected = 0;
            typing.menu_text = text.clone();
        }
        // Only the chat prompt has commands, not the box that edits a shell command.
        let items = match remember && typing.closed_for.as_deref() != Some(text.as_str()) {
            true => command_matches(self.commands, &text),
            false => Vec::new(),
        };
        typing.selected = typing.selected.min(items.len().saturating_sub(1));
        // With files in the box and no text, Backspace takes the last one out.
        let mut more_hints: Vec<(&str, &str)> = Vec::new();
        if remember && !self.files.is_empty() && typing.buffer.is_empty() {
            more_hints.push(("Backspace", "drops a file"));
        }
        if remember && !self.queue_items().is_empty() {
            more_hints.push(("Alt + 1…5", "drops a queued prompt"));
        }
        more_hints.extend_from_slice(hints);
        let max = width + FRAME_COLS - 2;
        let hint = match (armed_now, items.is_empty(), &self.note) {
            (true, _, _) => format!("{}{AGAIN_HINT}{}", self.style.action(), self.style.reset()),
            (false, true, Some(note)) => {
                let mut note: String = note.clone();
                if note.chars().count() > max {
                    note = note.chars().take(max.saturating_sub(1)).collect::<String>() + "…";
                }
                format!("{}{note}{}", self.style.action(), self.style.reset())
            }
            (false, false, _) => self.hint_line(MENU_HINTS, max),
            (false, true, None) => self.hint_line(&more_hints, max),
        };
        let menu = self.menu_lines(&items, typing.selected, max);
        (items, menu, hint)
    }

    /// One event for the box: keys edit it, a paste adds text or dropped files,
    /// the mouse scrolls or selects the output.
    fn handle(
        &mut self,
        typing: &mut Typing,
        event: Event,
        items: &[Command],
        remember: bool,
        width: usize,
    ) -> Step {
        let armed_now = typing.armed.is_some_and(|at| at.elapsed() < AGAIN);
        let buffer = &mut typing.buffer;
        let key = match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => key,
            // Files dropped on the terminal arrive as a paste of their paths.
            Event::Paste(text) => {
                match attach::from_paste(&text).filter(|_| remember) {
                    Some(dropped) => self.add_files(dropped),
                    None => buffer.insert_str(&text),
                }
                return Step::Continue;
            }
            Event::Mouse(mouse) => {
                // A click on Think, Browse or the mode in the bottom row. Between tasks the
                // row shows them unless a note or the Ctrl-C hint stands there instead.
                let labels_shown =
                    self.busy || (!armed_now && (self.note.is_none() || !items.is_empty()));
                if remember
                    && labels_shown
                    && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
                    && terminal::size().is_ok_and(|(_, height)| mouse.row + 1 == height)
                    && let Some(toggle) =
                        self.label_at(mouse.column as usize, self.busy || items.is_empty())
                {
                    return Step::Toggle(toggle);
                }
                // The wheel scrolls the output; the box is drawn again by the caller.
                if let Some(view) = &self.view
                    && let Some(paste) = super::mouse_event(view, mouse)
                {
                    // Right click without a selection: what was copied goes into the box.
                    buffer.insert_str(&paste);
                }
                return Step::Continue;
            }
            _ => return Step::Continue,
        };
        self.note = None;
        if let Some(view) = &self.view
            && super::scroll_by_key(view, key.code, key.modifiers)
        {
            return Step::Continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        // Alt+N (or Ctrl+N where the terminal reports it) drops queued prompt N.
        if remember
            && (alt || ctrl)
            && let KeyCode::Char(digit @ '1'..='9') = key.code
        {
            self.drop_queued(digit as usize - '1' as usize);
            return Step::Continue;
        }
        // With the menu open the arrows choose and Enter runs the command.
        if !items.is_empty() {
            match key.code {
                KeyCode::Up => {
                    typing.selected = (typing.selected + items.len() - 1) % items.len();
                    return Step::Continue;
                }
                KeyCode::Down => {
                    typing.selected = (typing.selected + 1) % items.len();
                    return Step::Continue;
                }
                // Enter runs the chosen command at once; Tab only completes it in the box.
                KeyCode::Enter if !alt && !shift => {
                    let command = items[typing.selected].0.to_string();
                    *buffer = Buffer::default();
                    return Step::Submit(command);
                }
                KeyCode::Tab => {
                    *buffer = Buffer::new(items[typing.selected].0);
                    return Step::Continue;
                }
                KeyCode::Esc => {
                    typing.closed_for = Some(buffer.text());
                    return Step::Continue;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Enter if alt || shift => buffer.insert('\n'),
            KeyCode::Char('j') if ctrl => buffer.insert('\n'),
            KeyCode::Enter if buffer.before_cursor() == Some('\\') => {
                buffer.backspace();
                buffer.insert('\n');
            }
            KeyCode::Enter => {
                let text = buffer.text().trim().to_string();
                // Files alone are a prompt too.
                if text.is_empty() && (self.files.is_empty() || !remember) {
                    return Step::Continue;
                }
                return Step::Submit(text);
            }
            // With text selected in the output, Ctrl-C copies it.
            KeyCode::Char('c')
                if ctrl && self.view.as_deref().is_some_and(super::copy_selection) =>
            {
                return Step::Continue;
            }
            // Ctrl-V: most terminals paste themselves (that arrives as a paste);
            // where the key reaches the app, the clipboard is read here.
            KeyCode::Char('v') if ctrl => {
                if let Some(view) = &self.view {
                    buffer.insert_str(&super::paste_text(view));
                }
            }
            KeyCode::Char('c') if ctrl && !armed_now => {
                *buffer = Buffer::default();
                self.files.clear();
                typing.armed = Some(Instant::now());
                return Step::Continue;
            }
            KeyCode::Char('c') if ctrl => return Step::Exit,
            KeyCode::Esc => {
                *buffer = Buffer::default();
                self.files.clear();
            }
            KeyCode::Char('d') if ctrl && buffer.is_empty() && !self.busy => return Step::Exit,
            KeyCode::Char('d') if ctrl => buffer.delete(),
            KeyCode::Char('a') if ctrl => buffer.home(),
            KeyCode::Char('e') if ctrl => buffer.end(),
            KeyCode::Char('u') if ctrl => buffer.kill_to_start(),
            KeyCode::Char('k') if ctrl => buffer.kill_to_end(),
            KeyCode::Char('w') if ctrl => buffer.delete_word(),
            KeyCode::Backspace if alt || ctrl => buffer.delete_word(),
            KeyCode::Backspace if buffer.is_empty() && remember => {
                self.files.pop();
            }
            KeyCode::Backspace => buffer.backspace(),
            KeyCode::Delete => buffer.delete(),
            KeyCode::Left => buffer.left(),
            KeyCode::Right => buffer.right(),
            KeyCode::Home => buffer.home(),
            KeyCode::End => buffer.end(),
            KeyCode::Up => {
                if !buffer.vertical(-1, width) && typing.position > 0 {
                    if typing.position == self.history.len() {
                        typing.draft = buffer.text();
                    }
                    typing.position -= 1;
                    *buffer = Buffer::new(&self.history[typing.position]);
                }
            }
            KeyCode::Down => {
                if !buffer.vertical(1, width) && typing.position < self.history.len() {
                    typing.position += 1;
                    *buffer =
                        Buffer::new(self.history.get(typing.position).unwrap_or(&typing.draft));
                }
            }
            KeyCode::BackTab => {
                if let Some(mode) = &self.mode {
                    mode.fetch_xor(true, Ordering::Relaxed);
                }
            }
            KeyCode::Tab => buffer.insert_str("\t"),
            KeyCode::Char(c) if !ctrl && !alt => buffer.insert(c),
            _ => {}
        }
        typing.armed = None;
        Step::Continue
    }

    /// The prompts waiting in the queue, oldest first.
    fn queue_items(&self) -> Vec<Queued> {
        self.queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// Takes queued prompt `index` (0 = number 1) out; nothing when there is none.
    fn drop_queued(&mut self, index: usize) {
        let mut queue = self.queue.lock().unwrap_or_else(|p| p.into_inner());
        if index < queue.len() {
            queue.remove(index);
        }
    }

    /// The queue above the box: an empty row that keeps it apart from the output,
    /// the title `Queued prompts:` with the keys that drop one, then one row per
    /// prompt: its number, the first line of its text and the files it carries, on a
    /// bar of its own color. Nothing when the queue is empty.
    fn queue_lines(&self, total: usize) -> Vec<String> {
        let s = self.style;
        let columns = |text: &str| text.chars().map(width_of).sum::<usize>();
        let items = self.queue_items();
        if items.is_empty() {
            return Vec::new();
        }
        let title = " Queued prompts:";
        let keys = format!("{} ", s.hint("Alt + 1…5", "drops one"));
        let keys_columns = columns("Alt + 1…5 drops one ");
        let gap = total.saturating_sub(columns(title) + keys_columns);
        let title_row = match gap {
            0 => format!("{}{}{title}{}", s.bold(), s.accent(), s.reset()),
            gap => format!(
                "{}{}{title}{}{}{keys}",
                s.bold(),
                s.accent(),
                s.reset(),
                " ".repeat(gap)
            ),
        };
        let mut lines = vec![String::new(), title_row];
        lines.extend(items.iter().enumerate().map(|(i, item)| {
            let number = format!(" {} ", i + 1);
            let files = match item.files.len() {
                0 => String::new(),
                n => format!("  📎 {n}"),
            };
            let room = total.saturating_sub(columns(&number) + 1 + columns(&files) + 1);
            let flat = item.text.replace('\n', " ⏎ ");
            let mut text = String::new();
            let mut used = 0;
            for c in flat.chars() {
                if used + width_of(c) > room {
                    text.pop();
                    text.push('…');
                    break;
                }
                used += width_of(c);
                text.push(c);
            }
            let used = columns(&text);
            format!(
                "{bg}{accent}{bold}{number}{reset}{bg}{key} {text}{pad}{faint}{files} {reset}",
                bg = s.queue_bg(),
                accent = s.accent(),
                bold = s.bold(),
                key = s.key(),
                faint = s.faint(),
                reset = s.reset(),
                pad = " ".repeat(room.saturating_sub(used)),
            )
        }));
        lines
    }

    fn remember(&mut self, text: &str) {
        if self.history.last().is_some_and(|last| last == text) {
            return;
        }
        self.history.push(text.to_string());
        let excess = self.history.len().saturating_sub(HISTORY_MAX);
        self.history.drain(..excess);
        if let Some(file) = &self.history_file {
            let _ = std::fs::write(
                file,
                serde_json::to_string(&self.history).unwrap_or_default(),
            );
        }
    }

    /// Puts the cursor on the first line of the box and clears everything below.
    fn move_to_top(&mut self, out: &mut impl Write) -> io::Result<()> {
        if self.cursor_line > 0 {
            queue!(out, MoveUp(self.cursor_line))?;
        }
        queue!(out, MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
        self.cursor_line = 0;
        Ok(())
    }

    /// Removes the box (inline) or empties it (docked) when the input ends.
    fn erase(&mut self) -> io::Result<()> {
        if self.dock.is_some() {
            // What follows the input is printed at the end of the output.
            if let Some(view) = &self.view {
                view.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .bottom();
            }
            return self.dock_idle();
        }
        let _screen = screen_lock();
        let mut out = io::stderr().lock();
        self.move_to_top(&mut out)?;
        out.flush()
    }

    fn draw(
        &mut self,
        buffer: &Buffer,
        width: usize,
        menu: &[String],
        hint: &str,
    ) -> io::Result<()> {
        if self.dock.is_some() {
            return self.draw_docked(buffer, width, menu, Some(hint));
        }
        let rows = buffer.rows(width);
        let (cursor_row, cursor_column) = buffer.locate(&rows);
        // A box taller than the screen shows the rows around the cursor.
        let screen = terminal::size().map_or(24, |(_, lines)| lines as usize);
        let visible = rows.len().min(screen.saturating_sub(4).max(1));
        let first = (cursor_row + 1).saturating_sub(visible);
        let mut frame = self
            .box_lines(buffer, width, first, visible, None)
            .join("\r\n");
        for line in menu {
            frame.push_str(&format!("\r\n{line}"));
        }
        frame.push_str(&format!("\r\n  {hint}"));

        let _screen = screen_lock();
        let mut out = io::stderr().lock();
        self.move_to_top(&mut out)?;
        out.write_all(frame.as_bytes())?;
        // The cursor is on the hint line now: top border, rows, bottom border and menu above it.
        let cursor_line = 1 + (cursor_row - first);
        let up = (visible + 2 + menu.len() - cursor_line) as u16;
        queue!(out, MoveUp(up), MoveToColumn((4 + cursor_column) as u16))?;
        self.cursor_line = cursor_line as u16;
        out.flush()
    }
}

/// Waits for one of `keys` (lowercase letters). Esc, Ctrl-C and Ctrl-D give `None`.
pub fn read_choice(keys: &str) -> io::Result<Option<char>> {
    let _raw = RawMode::enter()?;
    loop {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Ok(None),
            KeyCode::Char('c') | KeyCode::Char('d') if ctrl => return Ok(None),
            KeyCode::Char(c) if keys.contains(c.to_ascii_lowercase()) => {
                return Ok(Some(c.to_ascii_lowercase()));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(crossterm::event::KeyEvent::new(code, modifiers))
    }

    fn type_text(editor: &mut Editor, text: &str) -> Busy {
        for c in text.chars() {
            editor.busy_event(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        editor.busy_event(key(KeyCode::Enter, KeyModifiers::NONE))
    }

    #[test]
    fn a_click_on_a_label_of_the_bottom_row_finds_it() {
        let mut editor = Editor::new(Style::new(false), None);
        editor.set_model(Arc::new(AtomicBool::new(false)));
        editor.set_think(Arc::new(AtomicBool::new(false)));
        editor.set_browse(Arc::new(AtomicBool::new(true)));
        editor.set_mode(Arc::new(AtomicBool::new(false)));
        let max = Editor::inner_width() + FRAME_COLS - 2;
        let hints = [("Ctrl + C twice", "quit"), (MODE_KEY, "")];
        // The row as drawn: two columns, then the hint line.
        let row: Vec<char> = format!("  {}", editor.hint_line(&hints, max))
            .chars()
            .collect();
        let column = |label: &str| {
            let text: String = row.iter().collect();
            text[..text.find(label).unwrap()]
                .chars()
                .map(width_of)
                .sum::<usize>()
        };
        for (label, toggle) in [
            ("✦ Think", Toggle::Think),
            ("◉ Browse", Toggle::Browse),
            ("⏵ ask", Toggle::Mode),
        ] {
            let at = column(label);
            assert_eq!(
                editor.label_at(at, true),
                Some(toggle),
                "{label} first column"
            );
            let last = at + label.chars().map(width_of).sum::<usize>() - 1;
            assert_eq!(
                editor.label_at(last, true),
                Some(toggle),
                "{label} last column"
            );
        }
        // The model, the separators, the key and the left side switch nothing.
        assert_eq!(editor.label_at(column("◆ DeepSeek"), true), None);
        assert_eq!(editor.label_at(column("✦ Think") - 2, true), None);
        assert_eq!(editor.label_at(column("Shift + Tab"), true), None);
        assert_eq!(editor.label_at(2, true), None);
        assert_eq!(Toggle::Browse.command(), "/browse");
    }

    #[test]
    fn prompts_typed_while_a_task_runs_are_queued() {
        let mut editor = Editor::new(Style::new(false), None);
        editor.set_busy(true);
        for (i, text) in ["one", "two", "three", "four", "five"].iter().enumerate() {
            assert_eq!(
                type_text(&mut editor, text),
                Busy::Queued,
                "prompt {}",
                i + 1
            );
        }
        // The sixth waits in the box.
        assert_eq!(type_text(&mut editor, "six"), Busy::Full);
        assert_eq!(editor.typing.as_ref().unwrap().buffer.text(), "six");
        // Alt+2 drops the second, Ctrl+9 (no such item) nothing.
        editor.busy_event(key(KeyCode::Char('2'), KeyModifiers::ALT));
        editor.busy_event(key(KeyCode::Char('9'), KeyModifiers::CONTROL));
        let texts: Vec<String> = editor.queue_items().into_iter().map(|q| q.text).collect();
        assert_eq!(texts, ["one", "three", "four", "five"]);
        // Now there is room for it.
        assert_eq!(
            editor.busy_event(key(KeyCode::Enter, KeyModifiers::NONE)),
            Busy::Queued
        );
        assert_eq!(editor.queue().lock().unwrap().back().unwrap().text, "six");
        // An empty Enter queues nothing.
        editor.busy_event(key(KeyCode::Char('1'), KeyModifiers::ALT));
        assert_eq!(
            editor.busy_event(key(KeyCode::Enter, KeyModifiers::NONE)),
            Busy::Nothing
        );
        assert_eq!(editor.queue_items().len(), 4);
    }

    #[test]
    fn queue_rows_are_numbered_cut_and_closable() {
        let editor = Editor::new(Style::new(false), None);
        editor.queue().lock().unwrap().extend([
            Queued {
                text: "short".into(),
                files: Vec::new(),
            },
            Queued {
                text: "a very long prompt\nwith two lines that does not fit".into(),
                files: vec![Attachment {
                    path: PathBuf::from("/x/a.txt"),
                    name: "a.txt".into(),
                    size: 1,
                    warning: None,
                }],
            },
        ]);
        let rows = editor.queue_lines(50);
        let columns = |text: &str| text.chars().map(width_of).sum::<usize>();
        // An empty row apart from the output, the title with the keys, then the prompts.
        assert_eq!(rows[0], "");
        assert!(rows[1].starts_with(" Queued prompts: "));
        assert!(rows[1].ends_with("Alt + 1…5 drops one "));
        assert_eq!(columns(&rows[1]), 50);
        // Too narrow for the keys: the title alone.
        assert_eq!(editor.queue_lines(30)[1], " Queued prompts:");
        assert!(rows[2].starts_with(" 1  short "));
        assert!(rows[2].ends_with("  "));
        assert!(rows[3].starts_with(" 2  a very long prompt ⏎ with"));
        assert!(rows[3].ends_with("…  📎 1 "));
        assert_eq!(columns(&rows[2]), columns(&rows[3]));
        assert!(
            Editor::new(Style::new(false), None)
                .queue_lines(40)
                .is_empty()
        );
    }

    #[test]
    fn the_box_draws_its_own_cursor_while_busy() {
        let s = Style::new(true);
        assert_eq!(with_cursor("abc", 1, s), "a\x1b[7mb\x1b[0mc");
        assert_eq!(with_cursor("ab", 2, s), "ab\x1b[7m \x1b[0m");
    }

    #[test]
    fn dropped_files_sit_in_the_top_border() {
        let mut editor = Editor::new(Style::new(false), None);
        let file = |name: &str, size: u64, warning: bool| Attachment {
            path: PathBuf::from(format!("/x/{name}")),
            name: name.into(),
            size,
            warning: warning.then(|| "private".to_string()),
        };
        editor.files = vec![
            file("my report.txt", 24, false),
            file("id_ed25519", 40, true),
        ];
        let width = 60;
        let lines = editor.box_lines(&Buffer::default(), width, 0, 1, None);
        let columns = |text: &str| text.chars().map(width_of).sum::<usize>();
        assert!(lines[0].starts_with("╭─ 📎 my report.txt · 24 B  ⚠ id_ed25519 · 40 B ──"));
        assert_eq!(columns(&lines[0]), columns(&lines[2]));
        // Too many to list: a summary.
        editor.files = (0..9)
            .map(|i| file(&format!("file-{i}.txt"), 1024, i == 3))
            .collect();
        let lines = editor.box_lines(&Buffer::default(), width, 0, 1, None);
        assert!(lines[0].starts_with("╭─ 📎 9 files · 9 KB · ⚠ 1 private ─"));
        assert_eq!(columns(&lines[0]), columns(&lines[2]));
    }

    fn at(text: &str, cursor: usize) -> Buffer {
        let mut buffer = Buffer::new(text);
        buffer.cursor = cursor;
        buffer
    }

    #[test]
    fn editing_keys() {
        let mut buffer = Buffer::new("ab");
        buffer.left();
        buffer.insert('X');
        assert_eq!(buffer.text(), "aXb");
        buffer.backspace();
        buffer.delete();
        assert_eq!((buffer.text().as_str(), buffer.cursor), ("a", 1));
        buffer.right();
        buffer.backspace();
        buffer.backspace();
        assert!(buffer.is_empty());

        let mut buffer = Buffer::new("one two  ");
        buffer.delete_word();
        assert_eq!(buffer.text(), "one ");
        assert_eq!(buffer.before_cursor(), Some(' '));
    }

    #[test]
    fn command_menu_matches() {
        const COMMANDS: &[Command] = &[("/new", "a"), ("/permissions", "b"), ("/help", "c")];
        let names = |text: &str| -> Vec<&str> {
            command_matches(COMMANDS, text)
                .iter()
                .map(|c| c.0)
                .collect()
        };
        assert_eq!(names("/"), ["/new", "/permissions", "/help"]);
        assert_eq!(names("/p"), ["/permissions"]);
        // Typed out in full: no menu, Enter sends it.
        assert!(names("/permissions").is_empty());
        assert!(names("/x").is_empty());
        // A path or a sentence is not a command; neither is text without the slash.
        assert!(names("/new now").is_empty());
        assert!(names("new").is_empty());
        assert!(names("").is_empty());
    }

    #[test]
    fn paste_is_cleaned() {
        assert_eq!(Buffer::new("a\r\nb\rc\td\u{7}").text(), "a\nb\nc    d");
    }

    #[test]
    fn home_end_and_kills_work_on_the_logical_line() {
        let mut buffer = at("ab\ncdef\ng", 5);
        buffer.home();
        assert_eq!(buffer.cursor, 3);
        buffer.end();
        assert_eq!(buffer.cursor, 7);
        buffer.left();
        buffer.left();
        buffer.kill_to_end();
        assert_eq!(buffer.text(), "ab\ncd\ng");
        buffer.kill_to_start();
        assert_eq!((buffer.text().as_str(), buffer.cursor), ("ab\n\ng", 3));
    }

    #[test]
    fn rows_split_at_line_ends_and_wrap() {
        let buffer = Buffer::new("abcdef\ngh\n");
        assert_eq!(buffer.rows(4), [(0, 4), (4, 6), (7, 9), (10, 10)]);
        // A wide character takes two columns.
        assert_eq!(Buffer::new("a日本b").rows(3), [(0, 2), (2, 4)]);
        assert_eq!(Buffer::default().rows(10), [(0, 0)]);
    }

    #[test]
    fn locate_gives_row_and_column() {
        let buffer = at("abcdef\ngh", 5);
        let rows = buffer.rows(4);
        assert_eq!(buffer.locate(&rows), (1, 1));
        // At a wrap point the cursor belongs to the next row; at a line end to its own.
        assert_eq!(at("abcdef\ngh", 4).locate(&rows), (1, 0));
        assert_eq!(at("abcdef\ngh", 6).locate(&rows), (1, 2));
        assert_eq!(at("abcdef\ngh", 9).locate(&rows), (2, 2));
    }

    #[test]
    fn vertical_keeps_the_column_and_stops_at_the_edges() {
        let mut buffer = at("abcdef\ngh\nijkl", 13);
        assert!(buffer.vertical(-1, 10));
        assert_eq!(buffer.cursor, 9);
        assert!(buffer.vertical(-1, 10));
        assert_eq!(buffer.cursor, 2);
        assert!(!buffer.vertical(-1, 10));
        assert!(buffer.vertical(1, 10) && buffer.vertical(1, 10));
        assert_eq!(buffer.cursor, 12);
        assert!(!buffer.vertical(1, 10));
    }
}
