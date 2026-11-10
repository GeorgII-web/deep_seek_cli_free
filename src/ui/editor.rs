//! The multiline input box of the chat, and single-key questions.
//!
//! `Buffer` and the wrapping are pure and tested; `Editor` draws them with
//! crossterm in raw mode, on stderr, below the scrollback (no full-screen mode).

use super::style::Style;
use super::{screen_lock, screen_write};
use crossterm::cursor::{MoveToColumn, MoveUp};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{execute, queue};
use std::io::{self, Write};
use std::path::PathBuf;
use unicode_width::UnicodeWidthChar;

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
struct RawMode;

impl RawMode {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let _ = execute!(io::stderr(), EnableBracketedPaste);
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = execute!(io::stderr(), DisableBracketedPaste);
        let _ = terminal::disable_raw_mode();
    }
}

pub enum Input {
    Submit(String),
    /// Ctrl-D or Ctrl-C on an empty box
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
        }
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

    /// Gives the whole screen back: no scroll region, no box.
    pub fn close_dock(&mut self) {
        let Some(dock) = self.dock.take() else {
            return;
        };
        let mut frame = String::from("\x1b[r");
        for row in (dock.height as usize - dock.area + 1)..=dock.height as usize {
            frame.push_str(&format!("\x1b[{row};1H\x1b[2K"));
        }
        frame.push_str(&format!("\x1b[{};{}H", dock.out.0, dock.out.1));
        screen_write(&frame);
    }

    /// Reserves `area` rows at the bottom of a terminal `height` rows high and
    /// clears them. Growing pushes the output up first, so nothing is covered.
    fn set_area(dock: &mut Dock, area: usize, height: u16, frame: &mut String) {
        let rows = height as usize;
        let area = area.min(rows.saturating_sub(1));
        let bottom = (rows - area) as u16;
        let mut clear_from = bottom;
        if height != dock.height {
            // Resized: the old layout means nothing; output continues inside the new region.
            dock.out.0 = dock.out.0.min(bottom);
        } else {
            let old_bottom = (rows - dock.area) as u16;
            if dock.out.0 > bottom {
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

    /// The docked box, empty, with the cursor back in the output.
    fn dock_idle(&mut self) -> io::Result<()> {
        let width = Self::inner_width();
        let lines = self.box_lines(&Buffer::default(), width, 0, 1);
        let (_, height) = terminal::size()?;
        let Some(dock) = &mut self.dock else {
            return Ok(());
        };
        let mut frame = String::new();
        Self::set_area(dock, 1 + DOCK_EXTRA, height, &mut frame);
        let top = height as usize - dock.area + 1;
        for (i, line) in lines.iter().enumerate() {
            frame.push_str(&format!("\x1b[{};1H{line}", top + i));
        }
        frame.push_str(&format!("\x1b[{};{}H", dock.out.0, dock.out.1));
        screen_write(&frame);
        Ok(())
    }

    fn draw_docked(&mut self, buffer: &Buffer, width: usize, hint: &str) -> io::Result<()> {
        let s = self.style;
        let (_, height) = terminal::size()?;
        let rows = buffer.rows(width);
        let (cursor_row, cursor_column) = buffer.locate(&rows);
        let visible = rows
            .len()
            .min((height as usize / 3).clamp(1, DOCK_MAX_ROWS));
        let first = (cursor_row + 1).saturating_sub(visible);
        let lines = self.box_lines(buffer, width, first, visible);
        let Some(dock) = &mut self.dock else {
            return Ok(());
        };
        let mut frame = String::new();
        Self::set_area(dock, visible + DOCK_EXTRA, height, &mut frame);
        let top = height as usize - dock.area + 1;
        for (i, line) in lines.iter().enumerate() {
            frame.push_str(&format!("\x1b[{};1H{line}", top + i));
        }
        let hint: String = hint.chars().take(width + FRAME_COLS - 2).collect();
        frame.push_str(&format!(
            "\x1b[{height};1H{}  {hint}{}",
            s.gray(),
            s.reset()
        ));
        frame.push_str(&format!(
            "\x1b[{};{}H",
            top + 1 + cursor_row - first,
            5 + cursor_column
        ));
        screen_write(&frame);
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
    pub fn read(&mut self, initial: &str, hint: &str, remember: bool) -> io::Result<Input> {
        let _raw = RawMode::enter()?;
        let mut buffer = Buffer::new(initial);
        let mut position = self.history.len();
        let mut draft = String::new();
        self.cursor_line = 0;
        if let Some(dock) = &mut self.dock {
            // Output moved the cursor since the last draw.
            dock.out = Self::cursor().unwrap_or(dock.out);
        }
        loop {
            let width = Self::inner_width();
            self.draw(&buffer, width, hint)?;
            let key = match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => key,
                Event::Paste(text) => {
                    buffer.insert_str(&text);
                    continue;
                }
                _ => continue,
            };
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            let alt = key.modifiers.contains(KeyModifiers::ALT);
            let shift = key.modifiers.contains(KeyModifiers::SHIFT);
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
                KeyCode::Char('c') if ctrl && !buffer.is_empty() => buffer = Buffer::default(),
                KeyCode::Char('c') | KeyCode::Char('d') if ctrl && buffer.is_empty() => {
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
                KeyCode::Tab => buffer.insert_str("\t"),
                KeyCode::Char(c) if !ctrl && !alt => buffer.insert(c),
                _ => {}
            }
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
            return self.dock_idle();
        }
        let _screen = screen_lock();
        let mut out = io::stderr().lock();
        self.move_to_top(&mut out)?;
        out.flush()
    }

    fn draw(&mut self, buffer: &Buffer, width: usize, hint: &str) -> io::Result<()> {
        if self.dock.is_some() {
            return self.draw_docked(buffer, width, hint);
        }
        let s = self.style;
        let rows = buffer.rows(width);
        let (cursor_row, cursor_column) = buffer.locate(&rows);
        // A box taller than the screen shows the rows around the cursor.
        let screen = terminal::size().map_or(24, |(_, lines)| lines as usize);
        let visible = rows.len().min(screen.saturating_sub(4).max(1));
        let first = (cursor_row + 1).saturating_sub(visible);
        let mut frame = self.box_lines(buffer, width, first, visible).join("\r\n");
        let hint: String = hint.chars().take(width + FRAME_COLS - 2).collect();
        frame.push_str(&format!("\r\n{}  {hint}{}", s.gray(), s.reset()));

        let _screen = screen_lock();
        let mut out = io::stderr().lock();
        self.move_to_top(&mut out)?;
        out.write_all(frame.as_bytes())?;
        // The cursor is on the hint line now: top border + rows + bottom border above it.
        let cursor_line = 1 + (cursor_row - first);
        let up = (visible + 2 - cursor_line) as u16;
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
