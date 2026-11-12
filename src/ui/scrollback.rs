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
use serde::{Deserialize, Serialize};
use std::time::Instant;
use unicode_width::UnicodeWidthChar;

/// How long a message on the dock border stays.
const FLASH: std::time::Duration = std::time::Duration::from_secs(3);
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

/// The output as it is saved with a conversation and brought back on a switch:
/// the lines and the blocks, so they can still be folded.
#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    lines: Vec<String>,
    open: String,
    blocks: Vec<SavedBlock>,
}

#[derive(Serialize, Deserialize)]
struct SavedBlock {
    header: usize,
    end: usize,
    folded: bool,
    folded_text: String,
    open_text: String,
}

/// A block of lines that can be folded to its header: the output of an action.
struct Fold {
    /// Index of the header line in `closed`
    header: usize,
    /// One past the last body line; the body is `header + 1..end`
    end: usize,
    folded: bool,
    /// The header as it reads folded and unfolded (set when the block ends)
    texts: Option<(String, String)>,
}

/// The text of a row without its escape sequences.
pub fn plain_of(row: &str) -> String {
    let mut plain = String::new();
    let mut chars = row.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            plain.push(c);
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
    plain
}

/// Splits plain text at display columns `from` and `to`: before, inside, after.
fn cut(plain: &str, from: usize, to: usize) -> (String, String, String) {
    let (mut before, mut inside, mut after) = (String::new(), String::new(), String::new());
    let mut column = 0;
    for c in plain.chars() {
        let part = if column < from {
            &mut before
        } else if column < to {
            &mut inside
        } else {
            &mut after
        };
        part.push(c);
        column += c.width().unwrap_or(0);
    }
    (before, inside, after)
}

/// A place in the output: the row (counted from the first row of everything) and the column.
type Place = (usize, usize);

pub struct View {
    style: Style,
    /// Complete lines, without their line end
    closed: Vec<String>,
    /// The line being written
    open: String,
    /// The visible `closed` lines wrapped to `columns`: the line each row comes from, and the row
    wrapped: Vec<(usize, String)>,
    /// Foldable blocks, in the order of their headers
    folds: Vec<Fold>,
    /// The next complete line starts a block
    fold_pending: bool,
    /// A block is being written: new lines join it
    fold_active: bool,
    /// When that block started: its header shows the seconds
    fold_started: Instant,
    /// The next complete line is the status line
    status_pending: bool,
    /// The status line: one line at the start of the answer being awaited or
    /// written, with the spinner; it is removed when that step ends
    status: Option<usize>,
    /// When the status line was put up: it shows the seconds
    status_started: Instant,
    /// Text selected with the mouse: where the button went down and where it is now
    selection: Option<(Place, Place)>,
    /// A short message on the dock border ("copied"), with the time it was set
    flash: Option<(String, Instant)>,
    /// The text copied last
    copied: String,
    columns: usize,
    /// Rows of the output area (the screen above the dock)
    rows: usize,
    /// Rows of the dock above the box's top border (the prompt queue); the
    /// indicator sits on that border
    above_border: usize,
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
            folds: Vec::new(),
            fold_pending: false,
            fold_active: false,
            fold_started: Instant::now(),
            status_pending: false,
            status: None,
            status_started: Instant::now(),
            selection: None,
            flash: None,
            copied: String::new(),
            columns: columns.max(1),
            rows: rows.max(1),
            above_border: 0,
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
            let index = self.closed.len();
            self.wrapped.extend(
                wrap(&line, self.columns)
                    .into_iter()
                    .map(|row| (index, row)),
            );
            self.closed.push(line);
            if std::mem::take(&mut self.status_pending) {
                self.status = Some(index);
                self.status_started = Instant::now();
            } else if std::mem::take(&mut self.fold_pending) {
                self.folds.push(Fold {
                    header: index,
                    end: index + 1,
                    folded: false,
                    texts: None,
                });
                self.fold_active = true;
                self.fold_started = Instant::now();
            } else if self.fold_active
                && let Some(fold) = self.folds.last_mut()
            {
                fold.end = index + 1;
            }
        }
        if self.closed.len() > KEEP_LINES {
            self.forget(self.closed.len() - KEEP_LINES);
        }
        if self.offset > 0 {
            // Scrolled up: the view keeps showing the same rows.
            self.offset += self.total().saturating_sub(before);
            self.indicator();
        }
        self.offset == 0
    }

    /// Drops the oldest `count` lines and the blocks that started in them.
    fn forget(&mut self, count: usize) {
        self.closed.drain(..count);
        self.status = self.status.and_then(|index| index.checked_sub(count));
        self.folds.retain(|fold| fold.header >= count);
        for fold in &mut self.folds {
            fold.header -= count;
            fold.end -= count;
        }
        self.rewrap();
    }

    /// The next complete line is the header of a block; the lines after it are
    /// its body until `end_fold`.
    pub fn begin_fold(&mut self) {
        self.fold_pending = true;
    }

    /// The step in progress: while a block is being written its header shows
    /// the spinner `frame` and the seconds since it started. Only that row is drawn.
    pub fn animate(&mut self, frame: &str) {
        if !self.fold_active {
            return;
        }
        let Some(header) = self.folds.last().map(|fold| fold.header) else {
            return;
        };
        let s = self.style;
        let secs = self.fold_started.elapsed().as_secs();
        let text = format!(
            "{}  ⎿ {}{}{frame} running… {secs}s{}",
            s.gray(),
            s.reset(),
            s.accent(),
            s.reset()
        );
        self.rewrite_line(header, text);
    }

    /// Replaces the text of line `index` (one row) and draws just that row when it is in view.
    fn rewrite_line(&mut self, index: usize, text: String) {
        self.closed[index] = text.clone();
        let Some(position) = self.wrapped.iter().position(|(line, _)| *line == index) else {
            return;
        };
        self.wrapped[position].1 = text.clone();
        let first_row = self.first_row();
        if (first_row..first_row + self.rows).contains(&position) {
            // Save cursor, write the row, restore: output that arrives meanwhile is not disturbed.
            screen_write(&format!(
                "\x1b7\x1b[{};1H\x1b[2K{text}\x1b[0m\x1b8",
                position - first_row + 1
            ));
        }
    }

    /// The next complete line becomes the status line: the place of the progress
    /// icon while the model is awaited and while it writes, at the start of its answer.
    pub fn begin_status(&mut self) {
        self.status_pending = true;
    }

    /// The step in progress outside a block: the status line shows the spinner
    /// `frame`, what is going on and the seconds.
    pub fn animate_status(&mut self, frame: &str, label: &str) {
        let Some(index) = self.status else {
            return;
        };
        let s = self.style;
        let secs = self.status_started.elapsed().as_secs();
        // Icon and text in one color, so the line reads as one thing.
        let text = format!("{}{frame} {label}… {secs}s{}", s.accent(), s.reset());
        self.rewrite_line(index, text);
    }

    /// The step ended: the status line is taken out and the rest moves up.
    /// Returns true when there was one (the view was drawn again).
    pub fn end_status(&mut self) -> bool {
        self.status_pending = false;
        let Some(index) = self.status.take() else {
            return false;
        };
        self.closed.remove(index);
        for fold in &mut self.folds {
            if fold.header > index {
                fold.header -= 1;
                fold.end -= 1;
            }
        }
        self.selection = None;
        self.rewrap();
        self.offset = self.offset.min(self.max_offset());
        self.repaint();
        true
    }

    /// Lines in the body of the block being written.
    #[cfg(test)]
    pub fn fold_lines(&self) -> usize {
        match (self.fold_active, self.folds.last()) {
            (true, Some(fold)) => fold.end - fold.header - 1,
            _ => 0,
        }
    }

    /// Ends the block: its header becomes `folded_text` or `open_text`, and
    /// with `folded` the body is hidden. The caller repaints.
    pub fn end_fold(&mut self, folded_text: String, open_text: String, folded: bool) {
        self.fold_pending = false;
        if !std::mem::take(&mut self.fold_active) {
            return;
        }
        if let Some(fold) = self.folds.last_mut() {
            fold.folded = folded && fold.end > fold.header + 1;
            self.closed[fold.header] = if fold.folded {
                folded_text.clone()
            } else {
                open_text.clone()
            };
            fold.texts = Some((folded_text, open_text));
        }
        self.rewrap();
    }

    /// Folds or unfolds block `index`, keeping the rows above it where they are, and draws.
    fn toggle(&mut self, index: usize) {
        let first_row = self.total().saturating_sub(self.rows + self.offset);
        let Some(fold) = self.folds.get_mut(index) else {
            return;
        };
        let Some((folded_text, open_text)) = fold.texts.clone() else {
            return;
        };
        if fold.end <= fold.header + 1 {
            return;
        }
        fold.folded = !fold.folded;
        self.closed[fold.header] = if fold.folded { folded_text } else { open_text };
        self.rewrap();
        // The same row stays at the top of the view.
        self.offset = self
            .total()
            .saturating_sub(self.rows + first_row)
            .min(self.max_offset());
        self.repaint();
    }

    /// A click on screen row `row` (0-based) of the output area: on the header
    /// of a block it folds or unfolds it. Returns true when it did.
    pub fn click(&mut self, row: usize) -> bool {
        let first_row = self.total().saturating_sub(self.rows + self.offset);
        let Some(&(line, _)) = self.wrapped.get(first_row + row) else {
            return false;
        };
        match self.folds.iter().position(|fold| {
            fold.header == line && fold.texts.is_some() && fold.end > fold.header + 1
        }) {
            Some(index) => {
                self.toggle(index);
                true
            }
            None => false,
        }
    }

    /// Folds or unfolds the last finished block (the key for it, and "the output is the answer").
    pub fn toggle_last(&mut self) {
        if let Some(index) = self.folds.iter().rposition(|fold| fold.texts.is_some()) {
            self.toggle(index);
        }
    }

    /// Shows the body of the last finished block, when it is hidden.
    pub fn unfold_last(&mut self) {
        if let Some(index) = self.folds.iter().rposition(|fold| fold.texts.is_some())
            && self.folds[index].folded
        {
            // It is shown because it is the answer: the view stays at the end of the output.
            let at_bottom = self.offset == 0;
            self.toggle(index);
            if at_bottom {
                self.bottom();
            }
        }
    }

    /// Drops what was written on the open line (a question that got its answer).
    /// Returns true when the terminal line must be erased too.
    pub fn erase_open(&mut self) -> bool {
        self.open.clear();
        self.offset == 0
    }

    /// Wraps again: after a new width, a fold change, or dropped lines.
    fn rewrap(&mut self) {
        self.wrapped.clear();
        let mut folds = self.folds.iter().peekable();
        let mut hidden_until = 0;
        for (index, line) in self.closed.iter().enumerate() {
            let fold = folds.next_if(|fold| fold.header == index);
            if index < hidden_until {
                continue;
            }
            self.wrapped
                .extend(wrap(line, self.columns).into_iter().map(|row| (index, row)));
            if let Some(fold) = fold
                && fold.folded
            {
                hidden_until = fold.end;
            }
        }
    }

    fn max_offset(&self) -> usize {
        self.total().saturating_sub(self.rows)
    }

    /// Every row of the output with the line it comes from; the open line counts as one more line.
    fn all_rows(&self) -> Vec<(usize, String)> {
        let mut all = self.wrapped.clone();
        all.extend(
            self.open_rows()
                .into_iter()
                .map(|row| (self.closed.len(), row)),
        );
        all
    }

    /// Index of the first row the view shows.
    fn first_row(&self) -> usize {
        self.total().saturating_sub(self.rows + self.offset)
    }

    /// The rows the view shows now, top to bottom.
    fn visible(&self) -> Vec<String> {
        let all = self.all_rows();
        let end = all.len().saturating_sub(self.offset);
        let start = end.saturating_sub(self.rows);
        all[start..end].iter().map(|(_, row)| row.clone()).collect()
    }

    /// The selection in reading order, when there is one with any extent.
    fn selected(&self) -> Option<(Place, Place)> {
        let (anchor, head) = self.selection?;
        match anchor.cmp(&head) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Less => Some((anchor, head)),
            std::cmp::Ordering::Greater => Some((head, anchor)),
        }
    }

    /// The columns of row `index` that are selected, when any.
    fn selected_columns(&self, index: usize) -> Option<(usize, usize)> {
        let ((first, from), (last, to)) = self.selected()?;
        if index < first || index > last {
            return None;
        }
        let start = if index == first { from } else { 0 };
        let end = if index == last { to } else { usize::MAX };
        Some((start, end))
    }

    /// Everything needed to draw the output area: each row cleared and written
    /// (the selected part inverted), then, at the bottom, the cursor where output continues.
    fn frame(&self) -> String {
        let visible = self.visible();
        let first = self.first_row();
        let mut frame = String::new();
        for row in 0..self.rows {
            frame.push_str(&format!("\x1b[{};1H\x1b[2K", row + 1));
            let Some(text) = visible.get(row) else {
                continue;
            };
            match self.selected_columns(first + row) {
                // A selected row is shown without its colors, the selected part inverted.
                Some((from, to)) => {
                    let (before, inside, after) = cut(&plain_of(text), from, to);
                    frame.push_str(&format!("{before}\x1b[7m{inside}\x1b[0m{after}"));
                }
                None => frame.push_str(text),
            }
            frame.push_str("\x1b[0m");
        }
        let (row, column) = self.out();
        frame.push_str(&format!("\x1b[{row};{column}H"));
        frame
    }

    /// The mouse button went down on screen row `row`, column `column` (0-based).
    pub fn select_start(&mut self, row: usize, column: usize) {
        let had = self.selected().is_some();
        let place = (self.first_row() + row.min(self.rows - 1), column);
        self.selection = Some((place, place));
        if had {
            self.repaint();
        }
    }

    /// The mouse moved with the button down: the selection reaches to there.
    pub fn select_to(&mut self, row: usize, column: usize) {
        let place = (self.first_row() + row.min(self.rows - 1), column);
        if let Some((_, head)) = &mut self.selection
            && *head != place
        {
            *head = place;
            self.repaint();
        }
    }

    /// The selected text, when there is a selection. Rows of one wrapped line are joined.
    pub fn select_end(&self) -> Option<String> {
        let ((first, from), (last, to)) = self.selected()?;
        let all = self.all_rows();
        let mut text = String::new();
        let mut previous_line = None;
        for (index, (line, row)) in all
            .iter()
            .enumerate()
            .skip(first)
            .take(last.saturating_sub(first) + 1)
        {
            let start = if index == first { from } else { 0 };
            let end = if index == last { to } else { usize::MAX };
            if previous_line.is_some_and(|previous| previous != *line) {
                // The line before ended here: its trailing blanks are not text.
                let trimmed = text.trim_end_matches(' ').len();
                text.truncate(trimmed);
                text.push('\n');
            }
            text.push_str(&cut(&plain_of(row), start, end).1);
            previous_line = Some(*line);
        }
        let trimmed = text.trim_end_matches(' ').len();
        text.truncate(trimmed);
        (!text.is_empty()).then_some(text)
    }

    /// Ends the selection: it is no longer shown.
    pub fn select_clear(&mut self) {
        if std::mem::take(&mut self.selection).is_some_and(|(anchor, head)| anchor != head) {
            self.repaint();
        }
    }

    /// Whether text is selected.
    #[cfg(test)]
    pub fn has_selection(&self) -> bool {
        self.selected().is_some()
    }

    /// Takes the selected text as the copied text and ends the selection.
    pub fn copy_selection(&mut self) -> Option<String> {
        let text = self.select_end()?;
        self.copied = text.clone();
        self.select_clear();
        Some(text)
    }

    /// What was copied last (for a paste when the terminal does not paste itself).
    pub fn copied(&self) -> &str {
        &self.copied
    }

    /// Whether the last press and release were on the same cell: a click, not a selection.
    pub fn selection_is_click(&self) -> bool {
        self.selection.is_some_and(|(anchor, head)| anchor == head)
    }

    /// Shows `text` on the dock border for a moment (drawn now; the next draw of the border removes it when it is old).
    pub fn flash(&mut self, text: &str) {
        self.flash = Some((text.to_string(), Instant::now()));
        self.indicator();
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

    /// How many dock rows come before the box's top border (the prompt queue).
    pub fn set_above_border(&mut self, rows: usize) {
        self.above_border = rows;
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
        let text = match (&self.flash, self.offset) {
            (Some((text, at)), _) if at.elapsed() < FLASH => format!(" {text} "),
            (_, 0) => String::new(),
            (_, n) => format!(" ↑ scrolled up · {n} rows below · PgDn "),
        };
        let fill = "─".repeat(SPAN.saturating_sub(text.chars().count()));
        let column = self.columns - SPAN - 2;
        screen_write(&format!(
            "\x1b7\x1b[{};{column}H{}{fill}{}{}{text}{}\x1b8",
            self.rows + 1 + self.above_border,
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
        self.folds.clear();
        self.fold_pending = false;
        self.fold_active = false;
        self.status = None;
        self.status_pending = false;
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

    /// The output with its blocks, to save it (see `Snapshot`). A block that is
    /// still being written is saved as plain lines.
    pub fn snapshot(&self) -> Snapshot {
        let blocks = self
            .folds
            .iter()
            .filter_map(|fold| {
                let (folded_text, open_text) = fold.texts.clone()?;
                Some(SavedBlock {
                    header: fold.header,
                    end: fold.end,
                    folded: fold.folded,
                    folded_text,
                    open_text,
                })
            })
            .collect();
        Snapshot {
            lines: self.closed.clone(),
            open: self.open.clone(),
            blocks,
        }
    }

    /// Shows a saved output instead of the current one, blocks included.
    pub fn restore(&mut self, snapshot: Snapshot) {
        let lines = snapshot.lines.len();
        self.closed = snapshot.lines;
        self.open = snapshot.open;
        // Only blocks that fit the lines: a damaged file must not break the view.
        self.folds = snapshot
            .blocks
            .into_iter()
            .filter(|block| block.header < block.end && block.end <= lines)
            .map(|block| Fold {
                header: block.header,
                end: block.end,
                folded: block.folded,
                texts: Some((block.folded_text, block.open_text)),
            })
            .collect();
        self.folds.sort_by_key(|fold| fold.header);
        self.fold_pending = false;
        self.fold_active = false;
        self.status = None;
        self.status_pending = false;
        self.selection = None;
        self.offset = 0;
        // What comes next starts on a line of its own.
        if !self.open.is_empty() {
            let line = std::mem::take(&mut self.open);
            self.closed.push(line);
        }
        self.rewrap();
        self.repaint();
    }

    /// The whole output as text, for the terminal after the chat closes. Blocks
    /// are written out in full, with the header of their unfolded form.
    pub fn transcript(&self) -> String {
        let mut lines: Vec<&str> = self.closed.iter().map(String::as_str).collect();
        for fold in &self.folds {
            if let Some((_, open_text)) = &fold.texts {
                lines[fold.header] = open_text;
            }
        }
        let mut text = lines.join("\n");
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
    fn erase_open_drops_the_unfinished_line() {
        let mut v = view(10, 5);
        v.push("a\nquestion? ");
        assert!(v.erase_open());
        assert_eq!(v.open, "");
        assert_eq!(v.visible(), ["a", ""]);
    }

    /// A view with a finished block: header, three body lines, a line after it.
    fn with_block(folded: bool) -> View {
        let mut v = view(20, 10);
        v.push("before\n");
        v.begin_fold();
        v.push("running\n");
        assert_eq!(v.fold_lines(), 0);
        v.push("b1\nb2\nb3\n");
        assert_eq!(v.fold_lines(), 3);
        v.end_fold("3 lines [show]".into(), "3 lines [hide]".into(), folded);
        v.push("after\n");
        v
    }

    #[test]
    fn a_block_shows_its_body_live_and_folds_at_the_end() {
        let v = with_block(true);
        assert_eq!(v.visible(), ["before", "3 lines [show]", "after", ""]);
        let open = with_block(false);
        assert_eq!(
            open.visible(),
            ["before", "3 lines [hide]", "b1", "b2", "b3", "after", ""]
        );
        // The saved text always has the whole block.
        assert_eq!(v.transcript(), "before\n3 lines [hide]\nb1\nb2\nb3\nafter");
    }

    #[test]
    fn the_header_of_a_running_block_is_animated() {
        let mut v = view(40, 10);
        v.push("before\n");
        // No block yet: nothing to animate.
        v.animate("x");
        v.begin_fold();
        v.push("running\nb1\n");
        v.animate("⠹");
        assert_eq!(v.visible()[1], "  ⎿ ⠹ running… 0s");
        assert_eq!(v.visible()[2], "b1");
        // A finished block keeps its summary.
        v.end_fold("1 line [show]".into(), "1 line [hide]".into(), false);
        v.animate("⠸");
        assert_eq!(v.visible()[1], "1 line [hide]");
    }

    #[test]
    fn the_status_line_sits_at_the_start_of_the_answer_and_goes_away() {
        let mut v = view(40, 10);
        v.push("before\n");
        v.begin_status();
        v.push("...\n");
        v.push("the answer ");
        v.animate_status("⠹", "Answering");
        assert_eq!(v.visible(), ["before", "⠹ Answering… 0s", "the answer "]);
        v.push("goes on\n");
        // A block after it keeps working when the line is removed.
        v.begin_fold();
        v.push("running\nb1\nb2\n");
        v.end_fold("2 [show]".into(), "2 [hide]".into(), true);
        assert!(v.end_status());
        assert_eq!(
            v.visible(),
            ["before", "the answer goes on", "2 [show]", ""]
        );
        assert!(v.click(2));
        assert_eq!(v.visible().len(), 6);
        // Nothing to animate or to end any more; it never reaches the saved text.
        v.animate_status("⠸", "x");
        assert!(!v.end_status());
        assert!(!v.transcript().contains('⠹'));
    }

    #[test]
    fn mouse_selection_gives_the_text_and_joins_wrapped_rows() {
        let mut v = view(10, 6);
        v.push("\x1b[31mhello\x1b[0m world, this wraps\nsecond line\n");
        // Rows: "hello worl", "d, this wr", "aps", "second lin", "e", "".
        assert_eq!(v.visible().len(), 6);
        v.select_start(0, 6);
        assert!(v.selection_is_click());
        assert_eq!(v.select_end(), None);
        v.select_to(1, 7);
        assert!(!v.selection_is_click());
        assert_eq!(v.select_end().as_deref(), Some("world, this"));
        // Over a line end, and dragged backwards.
        v.select_start(3, 6);
        v.select_to(2, 1);
        assert_eq!(v.select_end().as_deref(), Some("ps\nsecond"));
        // The selected part is drawn inverted, without the row's own colors.
        v.select_start(0, 0);
        v.select_to(0, 5);
        assert!(
            v.frame()
                .starts_with("\x1b[1;1H\x1b[2K\x1b[7mhello\x1b[0m worl\x1b[0m")
        );
        assert_eq!(
            plain_of("\x1b[1mab\x1b[0m\x1b]8;;u\x1b\\c\x1b]8;;\x1b\\"),
            "abc"
        );
    }

    #[test]
    fn a_saved_output_comes_back_with_blocks_that_still_fold() {
        let saved = serde_json::to_string(&with_block(true).snapshot()).unwrap();
        let mut v = view(20, 10);
        v.push("something else\n");
        v.restore(serde_json::from_str(&saved).unwrap());
        assert_eq!(v.visible(), ["before", "3 lines [show]", "after", ""]);
        assert!(v.click(1));
        assert_eq!(
            v.visible(),
            ["before", "3 lines [hide]", "b1", "b2", "b3", "after", ""]
        );
        assert!(v.click(1));
        assert_eq!(v.visible().len(), 4);
        // A block that points outside the lines is dropped, the lines stay.
        let broken = r#"{"lines":["a","b"],"open":"","blocks":[{"header":1,"end":9,"folded":true,"folded_text":"x","open_text":"y"}]}"#;
        v.restore(serde_json::from_str(broken).unwrap());
        assert_eq!(v.visible(), ["a", "b", ""]);
        assert!(!v.click(1));
    }

    #[test]
    fn selecting_copies_nothing_until_asked() {
        let mut v = view(20, 4);
        v.push("hello world\n");
        v.select_start(0, 0);
        v.select_to(0, 5);
        assert!(v.has_selection());
        assert_eq!(v.copied(), "");
        // Copying takes the text and ends the selection; the copied text stays for a paste.
        assert_eq!(v.copy_selection().as_deref(), Some("hello"));
        assert_eq!(v.copied(), "hello");
        assert!(!v.has_selection());
        assert_eq!(v.copy_selection(), None);
        assert_eq!(v.copied(), "hello");
        // A click (no drag) is not a selection.
        v.select_start(0, 3);
        assert!(v.selection_is_click() && !v.has_selection());
        v.select_clear();
        assert!(!v.selection_is_click());
    }

    #[test]
    fn click_on_the_header_toggles_and_other_rows_do_nothing() {
        let mut v = with_block(true);
        assert!(!v.click(0));
        assert!(v.click(1));
        assert_eq!(
            v.visible(),
            ["before", "3 lines [hide]", "b1", "b2", "b3", "after", ""]
        );
        // A body row is not a switch; the header is, again.
        assert!(!v.click(2));
        assert!(v.click(1));
        assert_eq!(v.visible(), ["before", "3 lines [show]", "after", ""]);
        v.unfold_last();
        assert_eq!(v.visible().len(), 7);
        v.toggle_last();
        assert_eq!(v.visible().len(), 4);
    }

    #[test]
    fn a_block_without_body_is_not_folded_and_old_lines_take_their_blocks_along() {
        let mut v = view(20, 10);
        v.begin_fold();
        v.push("running\n");
        v.end_fold("x [show]".into(), "done".into(), true);
        assert_eq!(v.visible(), ["done", ""]);
        assert!(!v.click(0));

        let mut v = with_block(true);
        v.forget(1);
        assert_eq!(v.visible(), ["3 lines [show]", "after", ""]);
        assert!(v.click(0));
        v.forget(2);
        assert!(v.folds.is_empty());
        assert_eq!(v.visible(), ["b2", "b3", "after", ""]);
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
