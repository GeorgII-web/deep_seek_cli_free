//! The multiline input box of the chat, and single-key questions.
//!
//! `Buffer` and the wrapping are pure and tested; `Editor` draws them with
//! crossterm in raw mode, on stderr, below the scrollback (no full-screen mode).

use super::scrollback::View;
use super::style::Style;
use super::{screen_lock, screen_write};
use crossterm::cursor::{MoveToColumn, MoveUp};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{execute, queue};
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
#[derive(Default, Debug)]
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
    /// The approval mode (true = allow all): shown in the hint line, Shift+Tab switches it
    mode: Option<Arc<AtomicBool>>,
    /// Who answers (true = the mock): shown in the hint line next to the mode
    model: Option<Arc<AtomicBool>>,
    /// Slash commands offered in a menu while the input is the start of one: name, meaning
    commands: &'static [Command],
    /// Docked: the scrollable output above the dock; the box tells it the size of
    /// its area and passes it the scroll keys
    view: Option<Arc<Mutex<View>>>,
}

/// A slash command: name with the slash, what it does.
pub type Command = (&'static str, &'static str);

/// The key that switches the mode: its hint stays on the left, next to the mode.
const MODE_KEY: &str = "Shift+Tab";
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
            commands: &[],
            view: None,
        }
    }

    pub fn set_view(&mut self, view: Arc<Mutex<View>>) {
        self.view = Some(view);
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
                match i == selected {
                    true => format!(
                        "  {}{}❯ {name}{}{pad}{}{what}{}",
                        s.bold(),
                        s.accent(),
                        s.reset(),
                        s.key(),
                        s.reset()
                    ),
                    false => format!(
                        "    {}{name}{}{pad}{}{what}{}",
                        s.key(),
                        s.reset(),
                        s.faint(),
                        s.reset()
                    ),
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
    fn hint_line(&self, hints: &[(&str, &str)], max: usize) -> String {
        let s = self.style;
        let dot = format!("{} · {}", s.faint(), s.reset());
        let (mut left, mut left_columns) = (String::new(), 0);
        if let Some(mode) = &self.mode {
            let allow_all = mode.load(Ordering::Relaxed);
            left.push_str(&s.mode_label(allow_all));
            left_columns += if allow_all { 12 } else { 5 };
        }
        if let Some(model) = &self.model {
            let mock = model.load(Ordering::Relaxed);
            if !left.is_empty() {
                left.push_str(&dot);
                left_columns += 3;
            }
            left.push_str(&s.model_label(mock));
            left_columns += if mock { 6 } else { 10 };
        }
        let (mut right, mut right_columns) = (String::new(), 0);
        for (key, what) in hints {
            let item = key.chars().count() + what.chars().count() + usize::from(!what.is_empty());
            let on_left = *key == MODE_KEY && self.mode.is_some();
            let other = if on_left { right_columns } else { left_columns };
            let (side, used) = match on_left {
                true => (&mut left, &mut left_columns),
                false => (&mut right, &mut right_columns),
            };
            let separator = if side.is_empty() { 0 } else { 3 };
            // Two columns stay between the sides.
            if *used + separator + item + other + 2 > max {
                break;
            }
            if separator > 0 {
                side.push_str(&dot);
            }
            side.push_str(&s.hint(key, what));
            *used += separator + item;
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
        let mut view = view.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if view.resize(columns, dock.height as usize - dock.area) {
            view.repaint();
        }
        dock.out = view.out();
    }

    /// The docked box, empty, with the cursor back in the output.
    fn dock_idle(&mut self) -> io::Result<()> {
        let width = Self::inner_width();
        let lines = self.box_lines(&Buffer::default(), width, 0, 1);
        let (_, height) = terminal::size()?;
        let push = self.view.is_none();
        let Some(dock) = &mut self.dock else {
            return Ok(());
        };
        // The area first, then the output above it, then the box: each step may
        // clear rows the next one draws.
        let mut frame = String::new();
        Self::set_area(dock, 1 + DOCK_EXTRA, height, push, &mut frame);
        screen_write(&frame);
        self.sync_view();
        let Some(dock) = &self.dock else {
            return Ok(());
        };
        let top = height as usize - dock.area + 1;
        let mut frame = String::new();
        for (i, line) in lines.iter().enumerate() {
            frame.push_str(&format!("\x1b[{};1H{line}", top + i));
        }
        frame.push_str(&format!("\x1b[{};{}H", dock.out.0, dock.out.1));
        screen_write(&frame);
        if let Some(view) = &self.view {
            view.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .indicator();
        }
        Ok(())
    }

    fn draw_docked(
        &mut self,
        buffer: &Buffer,
        width: usize,
        menu: &[String],
        hint: &str,
    ) -> io::Result<()> {
        let (_, height) = terminal::size()?;
        let rows = buffer.rows(width);
        let (cursor_row, cursor_column) = buffer.locate(&rows);
        let visible = rows
            .len()
            .min((height as usize / 3).clamp(1, DOCK_MAX_ROWS));
        let first = (cursor_row + 1).saturating_sub(visible);
        let lines = self.box_lines(buffer, width, first, visible);
        let push = self.view.is_none();
        let Some(dock) = &mut self.dock else {
            return Ok(());
        };
        let mut frame = String::new();
        Self::set_area(
            dock,
            visible + DOCK_EXTRA + menu.len(),
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
        // Box, then the command menu, then the hint on the last row.
        for (i, line) in lines.iter().chain(menu).enumerate() {
            frame.push_str(&format!("\x1b[{};1H{line}", top + i));
        }
        frame.push_str(&format!("\x1b[{height};1H  {hint}"));
        frame.push_str(&format!(
            "\x1b[{};{}H",
            top + 1 + cursor_row - first,
            5 + cursor_column
        ));
        screen_write(&frame);
        if let Some(view) = &self.view {
            view.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .indicator();
        }
        Ok(())
    }

    /// Top border, `count` text rows from row `first`, bottom border.
    fn box_lines(&self, buffer: &Buffer, width: usize, first: usize, count: usize) -> Vec<String> {
        let s = self.style;
        let rows = buffer.rows(width);
        let border = "─".repeat(width + FRAME_COLS - 2);
        let mut lines = vec![format!("{}╭{border}╮{}", s.gray(), s.reset())];
        for (i, &row) in rows.iter().enumerate().skip(first).take(count) {
            let text = buffer.row_text(row);
            let used: usize = text.chars().map(width_of).sum();
            let mark = if i == 0 { ">" } else { " " };
            lines.push(format!(
                "{gray}│{reset} {accent}{mark}{reset} {text}{pad} {gray}│{reset}",
                gray = s.gray(),
                reset = s.reset(),
                accent = s.accent(),
                pad = " ".repeat(width.saturating_sub(used)),
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
    /// input goes into the history (not for an edited command).
    pub fn read(
        &mut self,
        initial: &str,
        hints: &[(&str, &str)],
        remember: bool,
    ) -> io::Result<Input> {
        let _raw = RawMode::enter()?;
        let mut buffer = Buffer::new(initial);
        let mut position = self.history.len();
        let mut draft = String::new();
        // Time of a Ctrl-C that a second one within `AGAIN` turns into an exit
        let mut armed: Option<Instant> = None;
        // The command menu: the chosen row, and the text for which Esc closed it
        let mut selected = 0usize;
        let mut closed_for: Option<String> = None;
        let mut menu_text = String::new();
        self.cursor_line = 0;
        if let Some(dock) = &mut self.dock
            && self.view.is_none()
        {
            // Output moved the cursor since the last draw (with a view, `sync_view` knows it).
            dock.out = Self::cursor().unwrap_or(dock.out);
        }
        loop {
            let width = Self::inner_width();
            let armed_now = armed.is_some_and(|at| at.elapsed() < AGAIN);
            let text = buffer.text();
            if text != menu_text {
                selected = 0;
                menu_text = text.clone();
            }
            // Only the chat prompt has commands, not the box that edits a shell command.
            let items = match remember && closed_for.as_deref() != Some(text.as_str()) {
                true => command_matches(self.commands, &text),
                false => Vec::new(),
            };
            selected = selected.min(items.len().saturating_sub(1));
            let hint = match (armed_now, items.is_empty()) {
                (true, _) => format!("{}{AGAIN_HINT}{}", self.style.action(), self.style.reset()),
                (false, false) => self.hint_line(MENU_HINTS, width + FRAME_COLS - 2),
                (false, true) => self.hint_line(hints, width + FRAME_COLS - 2),
            };
            let menu = self.menu_lines(&items, selected, width + FRAME_COLS - 2);
            self.draw(&buffer, width, &menu, &hint)?;
            // The "again" hint disappears by itself.
            if let Some(at) = armed.filter(|_| armed_now)
                && !event::poll(AGAIN.saturating_sub(at.elapsed()))?
            {
                armed = None;
                continue;
            }
            let key = match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => key,
                Event::Paste(text) => {
                    buffer.insert_str(&text);
                    continue;
                }
                // The wheel scrolls the output; the box is drawn again by the loop.
                Event::Mouse(mouse) => {
                    if let Some(view) = &self.view
                        && let Some(paste) = super::mouse_event(view, mouse)
                    {
                        // Right click without a selection: what was copied goes into the box.
                        buffer.insert_str(&paste);
                    }
                    continue;
                }
                _ => continue,
            };
            if let Some(view) = &self.view
                && super::scroll_by_key(view, key.code, key.modifiers)
            {
                continue;
            }
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            let alt = key.modifiers.contains(KeyModifiers::ALT);
            let shift = key.modifiers.contains(KeyModifiers::SHIFT);
            // With the menu open the arrows choose and Enter runs the command.
            if !items.is_empty() {
                match key.code {
                    KeyCode::Up => {
                        selected = (selected + items.len() - 1) % items.len();
                        continue;
                    }
                    KeyCode::Down => {
                        selected = (selected + 1) % items.len();
                        continue;
                    }
                    // Enter runs the chosen command at once; Tab only completes it in the box.
                    KeyCode::Enter if !alt && !shift => {
                        self.erase()?;
                        return Ok(Input::Submit(items[selected].0.to_string()));
                    }
                    KeyCode::Tab => {
                        buffer = Buffer::new(items[selected].0);
                        continue;
                    }
                    KeyCode::Esc => {
                        closed_for = Some(text);
                        continue;
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
                    if text.is_empty() {
                        continue;
                    }
                    self.erase()?;
                    if remember {
                        self.remember(&text);
                    }
                    return Ok(Input::Submit(text));
                }
                // With text selected in the output, Ctrl-C copies it.
                KeyCode::Char('c')
                    if ctrl && self.view.as_deref().is_some_and(super::copy_selection) =>
                {
                    continue;
                }
                // Ctrl-V: most terminals paste themselves (that arrives as a paste);
                // where the key reaches the app, the text copied here is inserted.
                KeyCode::Char('v') if ctrl => {
                    if let Some(view) = &self.view {
                        let copied = view
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .copied()
                            .to_string();
                        buffer.insert_str(&copied);
                    }
                }
                KeyCode::Char('c') if ctrl && !armed_now => {
                    buffer = Buffer::default();
                    armed = Some(Instant::now());
                    continue;
                }
                KeyCode::Char('c') if ctrl => {
                    self.erase()?;
                    return Ok(Input::Exit);
                }
                KeyCode::Esc => buffer = Buffer::default(),
                KeyCode::Char('d') if ctrl && buffer.is_empty() => {
                    self.erase()?;
                    return Ok(Input::Exit);
                }
                KeyCode::Char('d') if ctrl => buffer.delete(),
                KeyCode::Char('a') if ctrl => buffer.home(),
                KeyCode::Char('e') if ctrl => buffer.end(),
                KeyCode::Char('u') if ctrl => buffer.kill_to_start(),
                KeyCode::Char('k') if ctrl => buffer.kill_to_end(),
                KeyCode::Char('w') if ctrl => buffer.delete_word(),
                KeyCode::Backspace if alt || ctrl => buffer.delete_word(),
                KeyCode::Backspace => buffer.backspace(),
                KeyCode::Delete => buffer.delete(),
                KeyCode::Left => buffer.left(),
                KeyCode::Right => buffer.right(),
                KeyCode::Home => buffer.home(),
                KeyCode::End => buffer.end(),
                KeyCode::Up => {
                    if !buffer.vertical(-1, width) && position > 0 {
                        if position == self.history.len() {
                            draft = buffer.text();
                        }
                        position -= 1;
                        buffer = Buffer::new(&self.history[position]);
                    }
                }
                KeyCode::Down => {
                    if !buffer.vertical(1, width) && position < self.history.len() {
                        position += 1;
                        buffer = Buffer::new(self.history.get(position).unwrap_or(&draft));
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
            armed = None;
        }
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
            return self.draw_docked(buffer, width, menu, hint);
        }
        let rows = buffer.rows(width);
        let (cursor_row, cursor_column) = buffer.locate(&rows);
        // A box taller than the screen shows the rows around the cursor.
        let screen = terminal::size().map_or(24, |(_, lines)| lines as usize);
        let visible = rows.len().min(screen.saturating_sub(4).max(1));
        let first = (cursor_row + 1).saturating_sub(visible);
        let mut frame = self.box_lines(buffer, width, first, visible).join("\r\n");
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
        const COMMANDS: &[Command] = &[("/new", "a"), ("/mode", "b"), ("/help", "c")];
        let names = |text: &str| -> Vec<&str> {
            command_matches(COMMANDS, text)
                .iter()
                .map(|c| c.0)
                .collect()
        };
        assert_eq!(names("/"), ["/new", "/mode", "/help"]);
        assert_eq!(names("/m"), ["/mode"]);
        // Typed out in full: no menu, Enter sends it.
        assert!(names("/mode").is_empty());
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
