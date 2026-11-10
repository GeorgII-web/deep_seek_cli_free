//! The terminal side of the agent: input box, streamed answer, progress line,
//! action lines, approval question, result preview, finish line.
//!
//! Two layouts. Inline (a single task, or no terminal): output follows the
//! shell prompt, answer text on stdout, everything else on stderr. Screen (the
//! chat on a terminal): the screen is cleared, the input box is docked at the
//! bottom, output scrolls above it; everything goes to stdout.
//! Without a terminal there are no colors, no spinner and no box.

pub mod banner;
pub mod dialog;
pub mod editor;
pub mod render;
pub mod scrollback;
pub mod spinner;
pub mod style;

use crate::agent::interrupt::{Interrupt, Interrupts};
use crate::agent::protocol::Action;
use crate::agent::run::{Decision, Ui};
use crate::backend::Event;
use crossterm::event::{
    self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
};
use editor::{AGAIN, AGAIN_HINT, Editor, Input, RawMode};
use render::Markdown;
use scrollback::View;
use spinner::Spinner;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use style::Style;
use tokio::sync::mpsc::UnboundedSender;

/// Lines of a result shown to the user (the model gets the capped full text).
const RESULT_LINES: usize = 8;
/// Hints under the input box: key, what it does. As many as fit are shown.
const INPUT_HINTS: &[(&str, &str)] = &[
    ("Shift+Tab", "mode"),
    ("Enter", "send"),
    ("Shift+Enter", "new line"),
    ("Ctrl-C twice", "quit"),
    ("↑↓", "history"),
    ("/help", ""),
];
const EDIT_HINTS: &[(&str, &str)] = &[("Enter", "run"), ("Ctrl-D", "cancel")];
/// The slash commands of the chat: offered in the menu when the input starts with `/`.
/// `src/main.rs` (`chat`) runs them.
const COMMANDS: &[editor::Command] = &[
    (
        "/new",
        "start a fresh conversation (the model forgets the earlier tasks)",
    ),
    ("/mode", "switch between ask and allow all (also Shift+Tab)"),
    (
        "/model",
        "choose the real DeepSeek account or the built-in mock",
    ),
    ("/sessions", "list earlier conversations and switch to one"),
    ("/login", "enter a new DeepSeek token"),
    ("/help", "commands and keys"),
    ("/exit", "quit"),
];

static SCREEN: Mutex<()> = Mutex::new(());

/// Held while something is written to the terminal, so the progress thread
/// never writes into the middle of other output.
pub(crate) fn screen_lock() -> MutexGuard<'static, ()> {
    SCREEN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Writes control sequences and text to the terminal in one piece.
pub(crate) fn screen_write(text: &str) {
    let _screen = screen_lock();
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// Reads keys while a task runs: Esc stops the task, Ctrl-C twice leaves the app.
/// The terminal is in raw mode for that time, so lines end with `\r\n`.
struct Watch {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    _raw: RawMode,
}

/// Rows the mouse wheel moves the output per notch.
const WHEEL_ROWS: isize = 3;

/// Scrolls the output for a wheel event. Shared by the key watcher and the input box.
pub(crate) fn scroll_by_wheel(view: &Mutex<View>, kind: MouseEventKind) {
    let mut view = view.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match kind {
        MouseEventKind::ScrollUp => view.scroll(WHEEL_ROWS),
        MouseEventKind::ScrollDown => view.scroll(-WHEEL_ROWS),
        _ => {}
    }
}

/// Scrolls the output for PgUp, PgDn, Ctrl+Home and Ctrl+End. Returns true when the key was one of them.
pub(crate) fn scroll_by_key(view: &Mutex<View>, code: KeyCode, modifiers: KeyModifiers) -> bool {
    let mut view = view.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::PageUp => {
            let page = view.page();
            view.scroll(page);
        }
        KeyCode::PageDown => {
            let page = view.page();
            view.scroll(-page);
        }
        KeyCode::Home if ctrl => view.top(),
        KeyCode::End if ctrl => view.bottom(),
        _ => return false,
    }
    true
}

pub struct Terminal {
    style: Style,
    /// Keys can be read one by one: stdin and stderr are a terminal
    interactive: bool,
    /// Screen layout is on: box docked at the bottom
    screen: bool,
    spinner: Spinner,
    markdown: Markdown,
    editor: Editor,
    /// The answer printed something and its line is still open
    text_open: bool,
    thinking_open: bool,
    /// The last thing printed was a blank line
    gap: bool,
    started: Instant,
    answers: u32,
    actions: u32,
    /// Where the key watcher sends what the keys mean; set by `interrupts`
    interrupt_tx: Option<UnboundedSender<Interrupt>>,
    watch: Option<Watch>,
    /// Screen layout: the output kept for scrolling; shared with the box and the key watcher
    view: Option<Arc<Mutex<View>>>,
    /// Screen layout: the mouse wheel scrolls the output (the terminal reports the mouse to the app)
    mouse: bool,
    /// Lines of the last result that the preview left out; printed when the model
    /// says the result is the answer
    hidden_result: Vec<String>,
    /// The approval mode: true = allow all (run without asking), false = ask.
    /// Shared with the box, the progress line, the key watcher and the agent loop.
    allow_all: Arc<AtomicBool>,
}

impl Terminal {
    /// `history_file` keeps the chat inputs between runs. `allow_all` = the approval
    /// mode at the start (`--yes`).
    pub fn new(history_file: Option<PathBuf>, allow_all: bool) -> Self {
        let style = Style::detect();
        let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
        let allow_all = Arc::new(AtomicBool::new(allow_all));
        let mut spinner = Spinner::new(std::io::stderr().is_terminal(), style);
        let mut editor = Editor::new(style, history_file);
        if interactive {
            // The mode can only be switched with keys, so it is only shown with a terminal.
            spinner.set_mode(allow_all.clone());
            editor.set_mode(allow_all.clone());
            editor.set_commands(COMMANDS);
        }
        Self {
            style,
            interactive,
            screen: false,
            spinner,
            markdown: Markdown::new(style),
            editor,
            text_open: false,
            thinking_open: false,
            gap: false,
            started: Instant::now(),
            answers: 0,
            actions: 0,
            interrupt_tx: None,
            watch: None,
            view: None,
            mouse: true,
            hidden_result: Vec::new(),
            allow_all,
        }
    }

    /// The approval mode flag for the agent loop.
    pub fn mode(&self) -> Arc<AtomicBool> {
        self.allow_all.clone()
    }

    /// Switches between ask and allow all (`/mode`; Shift+Tab does the same in the
    /// box and while a task runs) and says what is on now.
    pub fn toggle_mode(&mut self) {
        let allow_all = !self.allow_all.fetch_xor(true, Ordering::Relaxed);
        let text = match allow_all {
            true => "Mode `allow all`: actions run without asking; the deny list still applies.",
            false => "Mode `ask`: actions outside the allow list need your yes.",
        };
        self.info(text);
    }

    /// The interrupts of this terminal for the agent loop: keys when there is a
    /// terminal (Esc, Ctrl-C twice), else the Ctrl-C signal.
    pub fn interrupts(&mut self) -> Interrupts {
        if !self.interactive {
            return Interrupts::signal();
        }
        let (sender, interrupts) = Interrupts::channel();
        self.interrupt_tx = Some(sender);
        self.spinner.set_hint("Esc", "to stop");
        interrupts
    }

    /// Starts reading keys for the running task. No-op without a terminal.
    fn watch_start(&mut self) {
        let Some(sender) = self.interrupt_tx.clone() else {
            return;
        };
        if self.watch.is_some() {
            return;
        }
        let Ok(raw) = RawMode::enter() else {
            return;
        };
        let stop = Arc::new(AtomicBool::new(false));
        let (flag, notifier, style, screen) = (
            stop.clone(),
            self.spinner.notifier(),
            self.style,
            self.screen,
        );
        let mode = self.allow_all.clone();
        let view = self.view.clone();
        let thread = std::thread::spawn(move || {
            let mut armed: Option<Instant> = None;
            while !flag.load(Ordering::Relaxed) {
                match event::poll(Duration::from_millis(50)) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(_) => break,
                }
                let key = match event::read() {
                    Ok(TermEvent::Key(key)) if key.kind != KeyEventKind::Release => key,
                    Ok(TermEvent::Mouse(mouse)) => {
                        if let Some(view) = &view {
                            scroll_by_wheel(view, mouse.kind);
                        }
                        continue;
                    }
                    _ => continue,
                };
                if let Some(view) = &view
                    && scroll_by_key(view, key.code, key.modifiers)
                {
                    continue;
                }
                match key.code {
                    KeyCode::Esc => {
                        let _ = sender.send(Interrupt::Stop);
                    }
                    // The progress line shows the mode, so the switch is visible at once.
                    KeyCode::BackTab => {
                        mode.fetch_xor(true, Ordering::Relaxed);
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if armed.is_some_and(|at| at.elapsed() < AGAIN) {
                            let _ = sender.send(Interrupt::Quit);
                        } else {
                            armed = Some(Instant::now());
                            // In the screen layout nothing may be printed past the scrollback.
                            if !notifier.show(AGAIN_HINT) && !screen {
                                // No progress line right now: a line of its own.
                                let _screen = screen_lock();
                                let text = format!(
                                    "\r\n{}{AGAIN_HINT}{}\r\n",
                                    style.action(),
                                    style.reset()
                                );
                                if screen {
                                    print!("{text}");
                                    let _ = std::io::stdout().flush();
                                } else {
                                    eprint!("{text}");
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        });
        self.watch = Some(Watch {
            stop,
            thread: Some(thread),
            _raw: raw,
        });
    }

    /// Stops reading keys and leaves raw mode; needed before the box or a question reads keys.
    fn watch_stop(&mut self) {
        if let Some(mut watch) = self.watch.take() {
            watch.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = watch.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// The text with the line ends the terminal needs right now.
    fn eol(&self, text: &str) -> String {
        if self.watch.is_some() {
            text.replace('\n', "\r\n")
        } else {
            text.to_string()
        }
    }

    /// Screen layout: records output in the scrollback and, when the view is at
    /// the bottom, writes it to the terminal. Returns false in the inline layout.
    fn emit(&self, text: &str) -> bool {
        let Some(view) = &self.view else {
            return false;
        };
        let mut view = view.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if view.push(text) {
            screen_write(&self.eol(text));
        }
        true
    }

    /// A line of chrome: stderr inline, the output area in the screen layout.
    fn line(&self, text: &str) {
        if self.emit(&format!("{text}\n")) {
            return;
        }
        let text = self.eol(&format!("{text}\n"));
        let _screen = screen_lock();
        eprint!("{text}");
    }

    /// Chrome without a line end (a question that waits for a key).
    fn partial(&self, text: &str) {
        if self.emit(text) {
            return;
        }
        let text = self.eol(text);
        let _screen = screen_lock();
        eprint!("{text}");
        let _ = std::io::stderr().flush();
    }

    /// Removes what `partial` wrote on the current line.
    fn erase_partial(&self) {
        match &self.view {
            Some(view) => {
                if view
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .erase_open()
                {
                    screen_write("\r\x1b[2K");
                }
            }
            None => {
                let _screen = screen_lock();
                eprint!("\r\x1b[2K");
                let _ = std::io::stderr().flush();
            }
        }
    }

    /// Removes the last `count` printed lines (the cursor is at the start of a line).
    /// Inline this needs a terminal that can move the cursor up.
    fn drop_lines(&self, count: usize) {
        match &self.view {
            Some(view) => {
                if view
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .drop_lines(count)
                {
                    screen_write(&format!("\x1b[{count}A\r\x1b[2K"));
                }
            }
            None if self.style.on() => {
                let _screen = screen_lock();
                eprint!("\x1b[{count}A\r\x1b[2K");
                let _ = std::io::stderr().flush();
            }
            None => {}
        }
    }

    /// Answer text: always stdout.
    fn text(&self, text: &str) {
        if self.emit(text) {
            return;
        }
        let text = self.eol(text);
        let _screen = screen_lock();
        print!("{text}");
        let _ = std::io::stdout().flush();
    }

    /// Whether the mouse wheel scrolls the output (config `ui.mouse_scroll`). Call before `open_chat`.
    pub fn set_mouse(&mut self, on: bool) {
        self.mouse = on;
    }

    /// Screen layout: clears the screen and draws output area and dock again,
    /// after a window covered them.
    fn redraw(&mut self) {
        if let Some(view) = &self.view {
            screen_write("\x1b[2J");
            self.editor.redraw_dock();
            view.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .repaint();
        }
    }

    /// Starts the chat: on a terminal the screen is cleared and the input box is
    /// docked at the bottom for the rest of the session.
    pub fn open_chat(&mut self, model: &str, cwd: &str) {
        let s = self.style;
        let docked = self.interactive && std::io::stdout().is_terminal();
        if docked {
            // The alternate screen: nothing of the terminal's earlier history is
            // reachable, and the app scrolls its own output.
            screen_write("\x1b[?1049h\x1b[2J\x1b[H");
            if self.mouse {
                // Button and wheel reports, in the SGR form.
                screen_write("\x1b[?1000h\x1b[?1006h");
            }
            let (columns, rows) =
                crossterm::terminal::size().map_or((80, 24), |(c, r)| (c as usize, r as usize));
            let view = Arc::new(Mutex::new(View::new(s, columns, rows.saturating_sub(4))));
            self.editor.set_view(view.clone());
            self.view = Some(view);
            self.screen = true;
        }
        // The picture needs a terminal; a piped chat gets the plain lines only.
        if docked {
            let columns = crossterm::terminal::size().map_or(80, |(columns, _)| columns as usize);
            let art = banner::art(s, columns);
            if !art.is_empty() {
                self.line("");
                for line in &art {
                    self.line(line);
                }
                self.line("");
            }
        }
        self.line(&format!(
            "{}{}  ✻ dsc{} {}{} · {model}{}",
            s.bold(),
            s.accent(),
            s.reset(),
            s.gray(),
            env!("CARGO_PKG_VERSION"),
            s.reset()
        ));
        self.line(&format!("{}  {cwd}{}", s.gray(), s.reset()));
        self.line("");
        self.line(&s.inline(
            "  `/` commands · `Shift+Tab` mode · `Esc` stops a task · `Ctrl-C` twice quits",
            s.gray(),
        ));
        self.line("");
        self.gap = true;
        if docked {
            if self.editor.open_dock().is_ok() {
                self.spinner.dock();
            } else {
                self.screen = false;
            }
        }
    }

    /// Reads the next chat input; `None` = leave. With a terminal this is the
    /// multiline box, otherwise one line of stdin.
    pub fn input(&mut self) -> Option<String> {
        if !self.interactive {
            let mut line = String::new();
            return match std::io::stdin().read_line(&mut line) {
                Ok(n) if n > 0 => Some(line.trim().to_string()),
                _ => None,
            };
        }
        if !self.screen {
            self.blank();
        }
        match self.editor.read("", INPUT_HINTS, true) {
            Ok(Input::Submit(text)) => {
                // A command is not part of the conversation: it is not echoed, its effect is shown.
                if !COMMANDS.iter().any(|(name, _)| *name == text) {
                    self.echo(&text);
                }
                Some(text)
            }
            Ok(Input::Exit) => None,
            Err(err) => {
                self.error(&format!("input: {err}"));
                None
            }
        }
    }

    /// One line of an action's output, indented under the action.
    fn result_line(&self, mark: &str, line: &str) {
        let s = self.style;
        let line = s.links(line, s.gray());
        self.line(&format!("{}  {mark} {line}{}", s.gray(), s.reset()));
    }

    /// The submitted input as part of the scrollback: with colors, a bar with its
    /// own background over the whole width, so a prompt is easy to find.
    fn echo(&mut self, text: &str) {
        let s = self.style;
        self.blank();
        for (i, line) in text.lines().enumerate() {
            let mark = if i == 0 { ">" } else { " " };
            // `ESC[K` fills the rest of the row with the background.
            let fill = if s.on() { "\x1b[K" } else { "" };
            self.line(&format!(
                "{bg}{bold}{accent} {mark} {reset}{bg}{bold}{line} {fill}{reset}",
                bg = s.prompt_bg(),
                bold = s.bold(),
                accent = s.accent(),
                reset = s.reset(),
            ));
        }
        self.gap = false;
        self.blank();
    }

    /// Opens the token window (see `dialog`). `None` = cancelled, or no terminal
    /// to show it on. The token is returned to the caller and goes nowhere else.
    pub fn token_dialog(&mut self, reason: &str) -> Option<String> {
        if !self.interactive {
            return None;
        }
        self.close_output();
        self.spinner.stop();
        self.watch_stop();
        // In the screen layout the app is on the alternate screen already: the
        // window is drawn over it and everything is drawn again after.
        let token = dialog::ask_token(self.style, reason, !self.screen)
            .ok()
            .flatten();
        self.redraw();
        token
    }

    /// Screen layout: everything on the output area as text with its colors, to
    /// save it with a conversation. `None` in the inline layout.
    pub fn output(&self) -> Option<String> {
        let view = self.view.as_ref()?;
        Some(
            view.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .transcript(),
        )
    }

    /// Screen layout: replaces the output area by `text` (the saved output of
    /// the conversation switched to; empty = a clean area).
    pub fn show_output(&mut self, text: &str) {
        self.close_output();
        if let Some(view) = &self.view {
            view.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .replace(text);
            self.gap = text.is_empty();
            self.hidden_result.clear();
        }
    }

    /// Opens a list window (see `dialog::pick`) and returns the chosen index.
    /// Without a terminal the list is printed and nothing is chosen.
    pub fn pick(&mut self, title: &str, items: &[String]) -> Option<usize> {
        if !self.interactive {
            self.info(&format!("{title}:\n{}", items.join("\n")));
            return None;
        }
        self.close_output();
        self.spinner.stop();
        self.watch_stop();
        let chosen = dialog::pick(self.style, title, items, !self.screen)
            .ok()
            .flatten();
        self.redraw();
        chosen
    }

    /// One blank line, unless the last thing printed was one.
    fn blank(&mut self) {
        if !std::mem::replace(&mut self.gap, true) {
            self.line("");
        }
    }

    /// Start of a task: resets the clock and the counters.
    pub fn begin(&mut self) {
        self.started = Instant::now();
        self.answers = 0;
        self.actions = 0;
        self.watch_start();
    }

    /// End of a task: one line with the time it took.
    pub fn finish(&mut self, result: &anyhow::Result<()>) {
        self.close_output();
        self.spinner.stop();
        self.watch_stop();
        self.blank();
        let s = self.style;
        let secs = self.started.elapsed().as_secs_f32();
        let plural = |n: u32, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        let stats = format!(
            "{secs:.1}s · {} · {}",
            plural(self.answers, "answer"),
            plural(self.actions, "action")
        );
        let (mark, color, text) = match result {
            Ok(()) => ("✓", s.ok(), stats),
            Err(err) => ("✗", s.err(), format!("{err:#} · {stats}")),
        };
        if s.on() {
            // A rule over the whole width with the outcome in it: the end of a task is easy to find.
            let columns = crossterm::terminal::size().map_or(80, |(columns, _)| columns as usize);
            // A long error is cut so the line stays one row.
            let room = columns.saturating_sub(10);
            let text = match text.chars().count() > room {
                true => format!(
                    "{}…",
                    text.chars()
                        .take(room.saturating_sub(1))
                        .collect::<String>()
                ),
                false => text,
            };
            let used = 3 + 2 + text.chars().count() + 1;
            let rule = "─".repeat(columns.saturating_sub(used + 1));
            self.line(&format!(
                "{faint}── {reset}{color}{bold}{mark}{reset} {key}{text}{reset} {faint}{rule}{reset}",
                faint = s.faint(),
                reset = s.reset(),
                bold = s.bold(),
                key = s.key(),
            ));
        } else {
            self.line(&format!("{mark} {text}"));
        }
        self.gap = false;
        // In the chat the next thing is the box or another task: keep a gap.
        if self.screen {
            self.blank();
        }
    }

    /// A message of the CLI itself that is not part of a task; `code` spans are colored.
    pub fn info(&mut self, text: &str) {
        self.blank();
        for line in text.lines() {
            self.line(&self.style.inline(line, self.style.gray()));
        }
        self.gap = false;
        self.blank();
    }

    pub fn error(&mut self, text: &str) {
        self.blank();
        self.line(&self.style.inline(&format!("✗ {text}"), self.style.err()));
        self.gap = false;
        self.blank();
    }

    /// The commands and keys of the chat.
    pub fn help(&mut self) {
        let s = self.style;
        let heading = |text: &str| format!("{}{}{text}{}", s.bold(), s.accent(), s.reset());
        let row = |key: &str, what: &str| {
            let pad = " ".repeat(30usize.saturating_sub(key.chars().count()));
            format!(
                "  {}{key}{}{pad}{}{what}{}",
                s.code_text(),
                s.reset(),
                s.gray(),
                s.reset()
            )
        };
        self.blank();
        let mut lines = vec![heading("Commands")];
        lines.extend(COMMANDS.iter().map(|(name, what)| row(name, what)));
        lines.extend([
            String::new(),
            heading("Input box"),
            row(
                "/",
                "list the commands: arrows choose, Enter runs, Tab completes",
            ),
            row("Enter", "send"),
            row(
                "Shift+Enter, Ctrl-J, \\ Enter",
                "new line (Ctrl-J works in every terminal)",
            ),
            row("Up, Down", "move in the text, then through earlier inputs"),
            row(
                "PgUp, PgDn, mouse wheel",
                "scroll the output; Ctrl+Home top, Ctrl+End bottom",
            ),
            row("Ctrl-A, Ctrl-E", "start, end of the line"),
            row(
                "Ctrl-W, Ctrl-U, Ctrl-K",
                "delete word, to the start, to the end",
            ),
            row("Esc", "clear the box"),
            String::new(),
            heading("While a task runs"),
            row("Esc", "stop the task"),
            String::new(),
            heading("Anywhere"),
            row(
                "Shift+Tab",
                "mode: ask (confirm actions) or allow all (run without asking)",
            ),
            row("Ctrl-C twice", "quit"),
            row("Ctrl-D", "quit (empty box)"),
        ]);
        for line in lines {
            self.line(&line);
        }
        self.gap = false;
        self.blank();
    }

    /// Ends an open answer line before chrome is printed. Inline, the spinner
    /// shares the output line and is removed too.
    fn close_output(&mut self) {
        if !self.spinner.docked() {
            self.spinner.stop();
        }
        if std::mem::take(&mut self.thinking_open) || std::mem::take(&mut self.text_open) {
            self.text("\n");
        }
    }

    fn print_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.spinner.docked() {
            self.spinner.start("Answering");
        } else {
            self.spinner.stop();
        }
        if std::mem::take(&mut self.thinking_open) {
            self.text("\n\n");
        }
        self.text(text);
        self.text_open = true;
        self.gap = false;
    }
}

impl Terminal {
    /// Asks about an action with one key; `e` opens the box with the command.
    fn question(&mut self, action: &Action, can_always: bool) -> Decision {
        self.close_output();
        self.spinner.stop();
        let s = self.style;
        if !self.interactive {
            self.line(&format!(
                "{}  no terminal to ask on: denied (use --yes or policy.allow){}",
                s.gray(),
                s.reset()
            ));
            return Decision::No;
        }
        let is_shell = matches!(action, Action::Shell { .. });
        let mut keys = String::from("yn");
        let mut choices = format!("{}y{} yes", s.bold(), s.reset());
        if can_always {
            keys.push('a');
            choices.push_str(&format!(
                " · {}a{} always in this session",
                s.bold(),
                s.reset()
            ));
        }
        choices.push_str(&format!(" · {}n{} no", s.bold(), s.reset()));
        if is_shell {
            keys.push('e');
            choices.push_str(&format!(" · {}e{} edit", s.bold(), s.reset()));
        }
        self.partial(&format!("{}  Allow?{} {choices} ", s.accent(), s.reset()));
        let choice = editor::read_choice(&keys).ok().flatten();
        // The question is replaced by its outcome.
        self.erase_partial();
        let denied = format!("{}  ✗ denied{}", s.err(), s.reset());
        match choice {
            Some('y') => Decision::Yes,
            Some('a') => Decision::Always,
            Some('e') => {
                let Action::Shell { cmd, .. } = action else {
                    return Decision::No;
                };
                match self.editor.read(cmd, EDIT_HINTS, false) {
                    Ok(Input::Submit(edited)) => Decision::Edit(edited),
                    _ => {
                        self.line(&denied);
                        Decision::No
                    }
                }
            }
            _ => {
                self.line(&denied);
                Decision::No
            }
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.watch_stop();
        self.spinner.stop();
        if self.screen {
            self.editor.close_dock();
            if self.mouse {
                screen_write("\x1b[?1006l\x1b[?1000l");
            }
            // Back on the normal screen the conversation is printed once, so it
            // stays in the terminal after the chat closed.
            screen_write("\x1b[?1049l");
            if let Some(view) = self.view.take() {
                let transcript = view
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .transcript();
                println!("{transcript}");
            }
        }
    }
}

impl Ui for Terminal {
    fn event(&mut self, event: &Event) {
        match event {
            Event::Status(label) => {
                // Inline, a status in the middle of printed text would tear the line.
                if self.spinner.docked() || (!self.text_open && !self.thinking_open) {
                    self.spinner.start(label);
                }
            }
            Event::Thinking(text) => {
                if self.spinner.docked() {
                    self.spinner.start("Thinking");
                } else {
                    self.spinner.stop();
                }
                self.text(&format!("{}{text}{}", self.style.dim(), self.style.reset()));
                self.thinking_open = true;
                self.gap = false;
            }
            Event::Text(text) => {
                let shown = self.markdown.feed(text);
                self.print_text(&shown);
            }
        }
    }

    fn answer_end(&mut self) {
        let rest = self.markdown.finish();
        self.print_text(&rest);
        self.close_output();
        self.answers += 1;
    }

    fn action(&mut self, action: &Action) {
        self.close_output();
        let s = self.style;
        let (kind, what) = match action {
            Action::Shell { cmd, .. } => ("Run", cmd.clone()),
            Action::ReadFile { path, .. } => ("Read", path.clone()),
            Action::WriteFile { path, content, .. } => {
                ("Write", format!("{path} ({} bytes)", content.len()))
            }
        };
        self.blank();
        self.gap = false;
        let mut lines = what.lines();
        self.line(&format!(
            "{}●{} {}{kind}{} {}{}{}",
            s.action(),
            s.reset(),
            s.bold(),
            s.reset(),
            s.action(),
            lines.next().unwrap_or(""),
            s.reset()
        ));
        for line in lines {
            self.line(&format!("{}      {line}{}", s.action(), s.reset()));
        }
        if !action.why().is_empty() {
            self.line(&s.inline(&format!("  {}", action.why()), s.gray()));
        }
    }

    fn approve(&mut self, action: &Action, can_always: bool) -> Decision {
        // The question reads keys itself: the task's key watcher pauses.
        self.watch_stop();
        let decision = self.question(action, can_always);
        self.watch_start();
        decision
    }

    fn running(&mut self, _: &Action) {
        self.hidden_result.clear();
        self.actions += 1;
        self.spinner.start("Running");
    }

    fn result(&mut self, text: &str) {
        self.close_output();
        // A denial was already shown by the question or the policy note.
        if text.starts_with("Denied") {
            self.blank();
            return;
        }
        let s = self.style;
        // The text is the message for the model; its markers are shortened here.
        let lines: Vec<String> = text
            .lines()
            .filter(|line| *line != "--- stdout ---")
            .map(|line| match line {
                "--- stderr ---" => "stderr:".to_string(),
                line => line.strip_prefix("Result: ").unwrap_or(line).to_string(),
            })
            .collect();
        for (i, line) in lines.iter().take(RESULT_LINES).enumerate() {
            let mark = if i == 0 { "⎿" } else { " " };
            self.result_line(mark, line);
        }
        self.hidden_result = lines.iter().skip(RESULT_LINES).cloned().collect();
        if !self.hidden_result.is_empty() {
            self.line(&format!(
                "{}    … +{} lines{}",
                s.gray(),
                self.hidden_result.len(),
                s.reset()
            ));
        }
        self.gap = false;
        self.blank();
    }

    fn result_is_answer(&mut self) {
        self.close_output();
        let hidden = std::mem::take(&mut self.hidden_result);
        if hidden.is_empty() {
            return;
        }
        // The "… +N lines" line and the blank line after it are replaced by the lines themselves.
        self.drop_lines(2);
        for line in &hidden {
            self.result_line(" ", line);
        }
        self.gap = false;
        self.blank();
    }

    fn note(&mut self, text: &str) {
        self.close_output();
        self.line(&self.style.inline(&format!("  {text}"), self.style.gray()));
        self.gap = false;
    }
}
