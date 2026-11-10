//! The output of the chat, kept by the app so it can be scrolled while the input
//! box stays at the bottom. The terminal's own scrollback is not used: the chat
//! runs on the alternate screen.
//!
//! The model: complete lines plus one open line, wrapped to the screen width into
//! rows. The view shows the last rows of that; `offset` rows are below it when
//! the user scrolled up. At the bottom, new output is also written straight to
//! the terminal (which wraps and scrolls the same way); scrolled up, it is only
//! recorded and the view stays where it is.

use super::screen_write;
use super::style::Style;
use unicode_width::UnicodeWidthChar;

/// Lines kept; older ones are dropped.
const KEEP_LINES: usize = 20_000;

/// Splits a line with escape sequences into rows of at most `columns` visible
/// columns. Colors that are on at a break continue on the next row.
pub fn wrap(line: &str, columns: usize) -> Vec<String> {
    let columns = columns.max(1);
    let mut rows = Vec::new();
    let (mut row, mut used) = (String::new(), 0usize);
    // SGR sequences since the last reset: repeated at the start of the next row.
    let mut active = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            let mut sequence = String::from(c);
            match chars.peek() {
                Some('[') => {
                    // CSI: parameters, then one final byte.
                    sequence.push(chars.next().unwrap());
                    for next in chars.by_ref() {
                        sequence.push(next);
                        if ('\x40'..='\x7e').contains(&next) {
                            break;
                        }
                    }
                    if sequence.ends_with('m') {
                        match sequence.as_str() {
                            "\x1b[0m" | "\x1b[m" => active.clear(),
                            _ => active.push_str(&sequence),
                        }
                    }
                }
                Some(']') => {
                    // OSC (hyperlink): up to BEL or ESC \.
                    sequence.push(chars.next().unwrap());
                    while let Some(next) = chars.next() {
                        sequence.push(next);
                        if next == '\x07' {
                            break;
                        }
                        if next == '\x1b' && chars.peek() == Some(&'\\') {
                            sequence.push(chars.next().unwrap());
                            break;
                        }
                    }
                }
                Some(_) => sequence.push(chars.next().unwrap()),
                None => {}
            }
            row.push_str(&sequence);
            continue;
        }
        let width = c.width().unwrap_or(0);
        if used + width > columns {
            if !active.is_empty() {
                row.push_str("\x1b[0m");
            }
            rows.push(std::mem::replace(&mut row, active.clone()));
            used = 0;
        }
        row.push(c);
        used += width;
    }
    rows.push(row);
    rows
}

/// Visible columns of a row (escape sequences count nothing).
pub fn columns_of(row: &str) -> usize {
    let mut used = 0;
    let mut chars = row.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            used += c.width().unwrap_or(0);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for next in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(next) = chars.next() {
                    if next == '\x07' || (next == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    used
}

pub struct View {
    style: Style,
    /// Complete lines, without their line end
    closed: Vec<String>,
    /// The line being written
    open: String,
    /// `closed` wrapped to `columns`
    wrapped: Vec<String>,
    columns: usize,
    /// Rows of the output area (the screen above the dock)
    rows: usize,
    /// Rows hidden below the view; 0 = at the bottom
    offset: usize,
}

impl View {
    pub fn new(style: Style, columns: usize, rows: usize) -> Self {
        Self {
            style,
            closed: Vec::new(),
            open: String::new(),
            wrapped: Vec::new(),
            columns: columns.max(1),
            rows: rows.max(1),
            offset: 0,
        }
    }

    fn open_rows(&self) -> Vec<String> {
        wrap(&self.open, self.columns)
    }

    fn total(&self) -> usize {
        self.wrapped.len() + self.open_rows().len()
    }

    /// Records output (`\n` ends a line). Returns true when the caller must also
    /// write it to the terminal: the view is at the bottom.
    pub fn push(&mut self, text: &str) -> bool {
        let before = self.total();
        let mut parts = text.split('\n');
        self.open.push_str(parts.next().unwrap_or(""));
        for part in parts {
            let line = std::mem::replace(&mut self.open, part.to_string());
            self.wrapped.extend(wrap(&line, self.columns));
            self.closed.push(line);
        }
        if self.closed.len() > KEEP_LINES {
            self.closed.drain(..self.closed.len() - KEEP_LINES);
            self.rewrap();
        }
        if self.offset > 0 {
            // Scrolled up: the view keeps showing the same rows.
            self.offset += self.total().saturating_sub(before);
            self.indicator();
        }
        self.offset == 0
    }

    /// Drops what was written on the open line (a question that got its answer).
    /// Returns true when the terminal line must be erased too.
    pub fn erase_open(&mut self) -> bool {
        self.open.clear();
        self.offset == 0
    }

    /// Removes the last `count` complete lines; the open line must be empty.
    /// Returns true when the terminal must drop them too.
    pub fn drop_lines(&mut self, count: usize) -> bool {
        for _ in 0..count {
            if let Some(line) = self.closed.pop() {
                let rows = wrap(&line, self.columns).len();
                self.wrapped
                    .truncate(self.wrapped.len().saturating_sub(rows));
            }
        }
        self.offset == 0
    }

    fn rewrap(&mut self) {
        self.wrapped = self
            .closed
            .iter()
            .flat_map(|line| wrap(line, self.columns))
            .collect();
    }

    fn max_offset(&self) -> usize {
        self.total().saturating_sub(self.rows)
    }

    /// The rows the view shows now, top to bottom.
    fn visible(&self) -> Vec<String> {
        let mut all = self.wrapped.clone();
        all.extend(self.open_rows());
        let end = all.len().saturating_sub(self.offset);
        let start = end.saturating_sub(self.rows);
        all[start..end].to_vec()
    }

    /// Everything needed to draw the output area: each row cleared and written,
    /// then, at the bottom, the cursor where output continues.
    fn frame(&self) -> String {
        let visible = self.visible();
        let mut frame = String::new();
        for row in 0..self.rows {
            frame.push_str(&format!("\x1b[{};1H\x1b[2K", row + 1));
            if let Some(text) = visible.get(row) {
                frame.push_str(text);
                frame.push_str("\x1b[0m");
            }
        }
        let (row, column) = self.out();
        frame.push_str(&format!("\x1b[{row};{column}H"));
        frame
    }

    /// Where output continues on the screen when the view is at the bottom: row and column, 1-based.
    pub fn out(&self) -> (u16, u16) {
        let visible = self.visible();
        let row = visible.len().max(1);
        let column = visible
            .last()
            .map_or(0, |last| columns_of(last))
            .min(self.columns - 1)
            + 1;
        (row as u16, column as u16)
    }

    /// Draws the output area and the scroll mark.
    pub fn repaint(&self) {
        screen_write(&self.frame());
        self.indicator();
    }

    /// Moves the view `delta` rows up (positive) or down and draws it.
    pub fn scroll(&mut self, delta: isize) {
        let offset = self
            .offset
            .saturating_add_signed(delta)
            .min(self.max_offset());
        if offset != self.offset {
            self.offset = offset;
            self.repaint();
        }
    }

    /// A page is the output area less one row of overlap.
    pub fn page(&self) -> isize {
        self.rows.saturating_sub(1).max(1) as isize
    }

    pub fn top(&mut self) {
        self.scroll(self.max_offset() as isize);
    }

    pub fn bottom(&mut self) {
        if self.offset > 0 {
            self.offset = 0;
            self.repaint();
        }
    }

    /// Adapts to a new size of the output area. Returns true when it changed (the caller repaints).
    pub fn resize(&mut self, columns: usize, rows: usize) -> bool {
        let (columns, rows) = (columns.max(1), rows.max(1));
        if columns == self.columns && rows == self.rows {
            return false;
        }
        if columns != self.columns {
            self.columns = columns;
            self.rewrap();
        }
        self.rows = rows;
        self.offset = self.offset.min(self.max_offset());
        true
    }

    /// The mark on the top border of the dock (the row below the output area):
    /// how far the view is scrolled up, or the plain border at the bottom.
    pub fn indicator(&self) {
        const SPAN: usize = 44;
        if self.columns < SPAN + 6 {
            return;
        }
        let s = self.style;
        let text = match self.offset {
            0 => String::new(),
            n => format!(" ↑ scrolled up · {n} rows below · PgDn "),
        };
        let fill = "─".repeat(SPAN.saturating_sub(text.chars().count()));
        let column = self.columns - SPAN - 2;
        screen_write(&format!(
            "\x1b7\x1b[{};{column}H{}{fill}{}{}{text}{}\x1b8",
            self.rows + 1,
            s.gray(),
            s.reset(),
            s.action(),
            s.reset()
        ));
    }

    /// Forgets everything and shows `text` instead (the saved output of another conversation).
    pub fn replace(&mut self, text: &str) {
        self.closed.clear();
        self.wrapped.clear();
        self.open.clear();
        self.offset = 0;
        if !text.is_empty() {
            self.push(text);
            // What comes next starts on a line of its own.
            if !self.open.is_empty() {
                self.push("\n");
            }
        }
        self.repaint();
    }

    /// The whole output as text, for the terminal after the chat closes.
    pub fn transcript(&self) -> String {
        let mut text = self.closed.join("\n");
        if !self.open.is_empty() {
            text.push('\n');
            text.push_str(&self.open);
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(columns: usize, rows: usize) -> View {
        View::new(Style::new(false), columns, rows)
    }

    #[test]
    fn wrap_breaks_at_the_width_and_carries_colors() {
        assert_eq!(wrap("abcdefg", 3), ["abc", "def", "g"]);
        assert_eq!(wrap("", 3), [""]);
        assert_eq!(wrap("日本語", 4), ["日本", "語"]);
        // The color continues on the next row; a reset ends it.
        assert_eq!(
            wrap("\x1b[31mabcd\x1b[0mef", 3),
            ["\x1b[31mabc\x1b[0m", "\x1b[31md\x1b[0mef"]
        );
        // A hyperlink sequence takes no room.
        let link = "\x1b]8;;http://a\x1b\\ab\x1b]8;;\x1b\\c";
        assert_eq!(wrap(link, 3), [link]);
        assert_eq!(columns_of(link), 3);
        assert_eq!(columns_of("\x1b[1;38;5;75mab\x1b[0m日"), 4);
    }

    #[test]
    fn push_builds_lines_and_the_view_shows_the_last_rows() {
        let mut v = view(10, 3);
        assert!(v.push("one\ntwo\nthr"));
        assert!(v.push("ee\nfour"));
        assert_eq!(v.closed, ["one", "two", "three"]);
        assert_eq!(v.open, "four");
        assert_eq!(v.visible(), ["two", "three", "four"]);
        assert_eq!(v.out(), (3, 5));
        // A closed last line leaves the cursor on a fresh row.
        v.push("\n");
        assert_eq!(v.visible(), ["three", "four", ""]);
        assert_eq!(v.out(), (3, 1));
        assert_eq!(v.transcript(), "one\ntwo\nthree\nfour");
    }

    #[test]
    fn scrolled_up_the_view_stays_while_output_arrives() {
        let mut v = view(10, 2);
        v.push("1\n2\n3\n4\n5");
        v.offset = 2;
        assert_eq!(v.visible(), ["2", "3"]);
        // New output is recorded but must not be written to the terminal.
        assert!(!v.push("\n6\n7"));
        assert_eq!(v.visible(), ["2", "3"]);
        assert_eq!(v.offset, 4);
        assert_eq!(v.max_offset(), 5);
        v.offset = 0;
        assert_eq!(v.visible(), ["6", "7"]);
    }

    #[test]
    fn resize_rewraps_and_keeps_the_offset_in_range() {
        let mut v = view(10, 2);
        v.push("abcdefgh\nxy");
        assert_eq!(v.total(), 2);
        assert!(v.resize(4, 2));
        assert_eq!(v.visible(), ["efgh", "xy"]);
        v.offset = 1;
        assert!(v.resize(4, 3));
        assert_eq!(v.offset, 0);
        assert!(!v.resize(4, 3));
    }

    #[test]
    fn erase_open_and_drop_lines() {
        let mut v = view(10, 5);
        v.push("a\nb\n\nquestion? ");
        assert!(v.erase_open());
        assert_eq!(v.open, "");
        assert!(v.drop_lines(2));
        assert_eq!(v.closed, ["a"]);
        assert_eq!(v.visible(), ["a", ""]);
    }

    #[test]
    fn replace_swaps_the_whole_output() {
        let mut v = view(10, 3);
        v.push("old 1\nold 2\nold");
        v.offset = 1;
        v.replace("new a\nnew b");
        assert_eq!(v.closed, ["new a", "new b"]);
        assert_eq!((v.open.as_str(), v.offset), ("", 0));
        assert_eq!(v.transcript(), "new a\nnew b");
        v.replace("");
        assert_eq!(v.visible(), [""]);
    }

    #[test]
    fn frame_clears_every_row_and_places_the_cursor() {
        let mut v = view(10, 3);
        v.push("a\nbc");
        let frame = v.frame();
        assert!(
            frame.starts_with("\x1b[1;1H\x1b[2Ka\x1b[0m\x1b[2;1H\x1b[2Kbc\x1b[0m\x1b[3;1H\x1b[2K")
        );
        assert!(frame.ends_with("\x1b[2;3H"));
    }
}
