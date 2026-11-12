//! The terminal side of the agent: input box, streamed answer, progress line,
//! action lines, approval question, result preview, finish line.
//!
//! Two layouts. Inline (a single task, or no terminal): output follows the
//! shell prompt, answer text on stdout, everything else on stderr. Screen (the
//! chat on a terminal): the screen is cleared, the input box is docked at the
//! bottom, output scrolls above it; everything goes to stdout.
//! Without a terminal there are no colors, no spinner and no box.

pub mod banner;
pub mod clipboard;
pub mod dialog;
pub mod editor;
pub mod render;
pub mod scrollback;
pub mod spinner;
pub mod style;

use crate::agent::interrupt::{Interrupt, Interrupts};
use crate::agent::protocol::Action;
use crate::agent::run::{Decision, Ui};
use crate::attach::Attachment;
use crate::backend::Event;
use base64::Engine as _;
use crossterm::event::{
    self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
};
use editor::{AGAIN, AGAIN_HINT, Busy, Editor, Input, Queue, Queued, RawMode};
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

/// An action's output of up to this many lines stays open when the action ends; a longer one folds.
const FOLD_OPEN_LINES: usize = 5;

/// Whether the first line of an action's result says it went wrong: an error
/// of the tool, a command that did not exit with 0, a timeout, a kill.
fn outcome_failed(outcome: &str) -> bool {
    if let Some(code) = outcome.strip_prefix("exit code ") {
        return code.split_whitespace().next() != Some("0");
    }
    outcome.starts_with("Error")
        || outcome.starts_with("timed out")
        || outcome.starts_with("killed")
}

/// What a command printed, made safe for the screen: escape sequences and
/// control characters are dropped, a carriage return ends the line.
fn clean_output(piece: &str) -> String {
    let mut out = String::new();
    let mut chars = piece.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI: up to its final byte. OSC: up to BEL or ESC \.
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
            },
            '\r' => {
                if chars.peek() != Some(&'\n') {
                    out.push('\n');
                }
            }
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}
/// Hints under the input box: key, what it does. Only the two worth seeing all
/// the time; `/help` has the rest.
const INPUT_HINTS: &[(&str, &str)] = &[
    // Next to the mode it switches; everything else about keys is in `/help`.
    ("Shift + Tab", ""),
    ("Ctrl + C twice", "quit"),
    ("↑↓", "history"),
];
const EDIT_HINTS: &[(&str, &str)] = &[("Enter", "run"), ("Ctrl-D", "cancel")];
/// Shown on the progress line when Enter finds the queue full.
const QUEUE_FULL: &str = "Queue full · Alt+N drops one";
/// The slash commands of the chat: offered in the menu when the input starts with `/`.
/// `src/main.rs` (`chat`) runs them.
const COMMANDS: &[editor::Command] = &[
    (
        "/new",
        "start a fresh conversation (the model forgets the earlier tasks)",
    ),
    (
        "/permissions",
        "switch between ask and allow all (also Shift+Tab)",
    ),
    (
        "/model",
        "choose the real DeepSeek account or the built-in mock",
    ),
    (
        "/think",
        "deep thinking on or off (web search is off while it is on)",
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
    // The unit tests draw into nothing.
    if cfg!(test) {
        return;
    }
    let _screen = screen_lock();
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// What the icon in the tab title says.
#[derive(Clone, Copy, PartialEq)]
enum TabState {
    /// Nothing runs; the default icon
    Idle,
    /// A task is in progress
    Working,
    /// The task waits for the user: an approval question
    Asking,
}

/// What the tab title is made of. Shared with the key watcher, which writes the
/// title again when the queue changes while a task runs.
struct Tab {
    icon: &'static str,
    title: String,
}

/// `<icon> <title>`, with `+N` after the icon while N prompts wait in the queue.
fn tab_text(tab: &Tab, queued: usize) -> String {
    match queued {
        0 => format!("{} {}", tab.icon, tab.title),
        n => format!("{} +{n} {}", tab.icon, tab.title),
    }
}

/// Writes the tab title. Both forms: OSC 0 (icon name and title) and OSC 2
/// (title only); some terminal setups act on one of them only.
fn write_tab(tab: &Mutex<Tab>, queue: &Queue) {
    let queued = queue.lock().unwrap_or_else(|p| p.into_inner()).len();
    let text = tab_text(&tab.lock().unwrap_or_else(|p| p.into_inner()), queued);
    screen_write(&format!("\x1b]0;{text}\x07\x1b]2;{text}\x07"));
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
/// Copies the selected text: to the terminal's clipboard (OSC 52) and into the
/// view, for a paste inside the app. Returns false when nothing is selected.
pub(crate) fn copy_selection(view: &Mutex<View>) -> bool {
    let mut view = view.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(text) = view.copy_selection() else {
        return false;
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(&text);
    screen_write(&format!("\x1b]52;c;{encoded}\x07"));
    view.flash(&format!("✓ copied {} characters", text.chars().count()));
    true
}

/// What a paste inserts: the system clipboard (also text copied in other
/// windows), read through the desktop's tool; when that gives nothing, the text
/// copied last inside the app.
pub(crate) fn paste_text(view: &Mutex<View>) -> String {
    clipboard::read().unwrap_or_else(|| {
        view.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .copied()
            .to_string()
    })
}

/// The mouse on the output: the wheel scrolls it, a click on the header of a
/// block folds or unfolds it, a drag selects text (nothing is copied yet). The
/// right button copies the selection; without a selection it asks for a paste:
/// the returned text goes into the input box. Shared by the key watcher and the box.
pub(crate) fn mouse_event(
    view: &Mutex<View>,
    mouse: crossterm::event::MouseEvent,
) -> Option<String> {
    use crossterm::event::MouseButton::{Left, Right};
    let (row, column) = (mouse.row as usize, mouse.column as usize);
    if mouse.kind == MouseEventKind::Down(Right) {
        if copy_selection(view) {
            return None;
        }
        return Some(paste_text(view)).filter(|text| !text.is_empty());
    }
    let mut view = view.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match mouse.kind {
        MouseEventKind::ScrollUp => view.scroll(WHEEL_ROWS),
        MouseEventKind::ScrollDown => view.scroll(-WHEEL_ROWS),
        MouseEventKind::Down(Left) => view.select_start(row, column),
        MouseEventKind::Drag(Left) => view.select_to(row, column),
        // Press and release on one cell is a click; a drag leaves the text selected.
        MouseEventKind::Up(Left) if view.selection_is_click() => {
            view.select_clear();
            view.click(row);
        }
        _ => {}
    }
    None
}

/// Scrolls the output for PgUp, PgDn, Ctrl+Home and Ctrl+End, and folds or
/// unfolds the last block for Ctrl-O. Returns true when the key was one of them.
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
        KeyCode::Char('o') if ctrl => view.toggle_last(),
        _ => return false,
    }
    true
}

pub struct Terminal {
    style: Style,
    /// Files submitted with the last input (dropped on the box), for `take_files`
    files: Vec<Attachment>,
    /// Keys can be read one by one: stdin and stderr are a terminal
    interactive: bool,
    /// Screen layout is on: box docked at the bottom
    screen: bool,
    spinner: Spinner,
    markdown: Markdown,
    /// The input box; shared with the key watcher, which fills it while a task runs
    editor: Arc<Mutex<Editor>>,
    /// Prompts sent while a task ran, taken by `input` before the box is read again
    queue: Queue,
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
    /// The title of the terminal tab, without its icon
    title: String,
    tab_state: TabState,
    /// The tab title as last written, for the key watcher (`write_tab`)
    tab: Arc<Mutex<Tab>>,
    /// The status line of the answer in progress is up (screen layout)
    status_on: bool,
    /// Output of the running command not yet ended by a line end
    live: String,
    /// Body lines of the action's block so far (live output, or lines of its result)
    live_lines: usize,
    /// Who answers: true = the built-in mock, false = DeepSeek. Shown under the box and on the progress line.
    mock: Arc<AtomicBool>,
    /// Deep thinking is on. Shown under the box and on the progress line.
    think: Arc<AtomicBool>,
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
        let mock = Arc::new(AtomicBool::new(false));
        let think = Arc::new(AtomicBool::new(false));
        let mut spinner = Spinner::new(std::io::stderr().is_terminal(), style);
        let mut editor = Editor::new(style, history_file);
        if interactive {
            // The mode can only be switched with keys, so it is only shown with a terminal.
            spinner.set_mode(allow_all.clone());
            editor.set_mode(allow_all.clone());
            spinner.set_model(mock.clone());
            editor.set_model(mock.clone());
            spinner.set_think(think.clone());
            editor.set_think(think.clone());
            editor.set_commands(COMMANDS);
        }
        Self {
            style,
            interactive,
            screen: false,
            spinner,
            markdown: Markdown::new(style),
            queue: editor.queue(),
            editor: Arc::new(Mutex::new(editor)),
            text_open: false,
            thinking_open: false,
            gap: false,
            files: Vec::new(),
            started: Instant::now(),
            answers: 0,
            actions: 0,
            interrupt_tx: None,
            watch: None,
            view: None,
            mouse: true,
            title: String::from("dsc"),
            tab_state: TabState::Idle,
            tab: Arc::new(Mutex::new(Tab {
                icon: "✻",
                title: String::from("dsc"),
            })),
            status_on: false,
            live: String::new(),
            live_lines: 0,
            mock,
            think,
            allow_all,
        }
    }

    /// The input box (the key watcher may hold it for a moment while a task runs).
    fn editor(&self) -> MutexGuard<'_, Editor> {
        self.editor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Tells the UI whether deep thinking is on, for the lines at the bottom.
    pub fn set_think(&mut self, on: bool) {
        self.think.store(on, Ordering::Relaxed);
    }

    /// Tells the UI who answers now, for the lines at the bottom.
    pub fn set_model(&mut self, mock: bool) {
        self.mock.store(mock, Ordering::Relaxed);
    }

    /// Sets the title of the terminal tab (screen layout only): `dsc`, or the
    /// name of the conversation. An icon in front shows what the app is doing.
    pub fn set_title(&mut self, title: &str) {
        self.title = title.chars().filter(|c| !c.is_control()).take(80).collect();
        self.write_title();
    }

    /// The tab shows the state at a glance, also when another tab is in front:
    /// working on a task, waiting for the user's yes or no, or idle.
    fn set_tab_state(&mut self, state: TabState) {
        if self.tab_state != state {
            self.tab_state = state;
            self.write_title();
        }
    }

    fn write_title(&self) {
        if self.screen {
            let icon = match self.tab_state {
                TabState::Idle => "✻",
                TabState::Working => "⏳",
                TabState::Asking => "❓",
            };
            *self.tab.lock().unwrap_or_else(|p| p.into_inner()) = Tab {
                icon,
                title: self.title.clone(),
            };
            write_tab(&self.tab, &self.queue);
        }
    }

    /// The approval mode flag for the agent loop.
    pub fn mode(&self) -> Arc<AtomicBool> {
        self.allow_all.clone()
    }

    /// Switches between ask and allow all (`/permissions`; Shift+Tab does the same in the
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
        self.spinner.set_hint("Esc", "interrupt");
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
        let (tab, queue) = (self.tab.clone(), self.queue.clone());
        // The chat screen: the box takes the other keys, so the next prompt can be
        // written (and queued) while this one runs.
        let editor = (screen && view.is_some()).then(|| self.editor.clone());
        self.editor().set_busy(true);
        let thread = std::thread::spawn(move || {
            let mut armed: Option<Instant> = None;
            while !flag.load(Ordering::Relaxed) {
                match event::poll(Duration::from_millis(50)) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(_) => break,
                }
                let to_box = |event: TermEvent| {
                    let Some(editor) = &editor else {
                        return Busy::Nothing;
                    };
                    let before = queue.lock().unwrap_or_else(|p| p.into_inner()).len();
                    let outcome = editor
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .busy_event(event);
                    // A prompt queued or dropped: the count in the tab follows.
                    if queue.lock().unwrap_or_else(|p| p.into_inner()).len() != before {
                        write_tab(&tab, &queue);
                    }
                    outcome
                };
                let key = match event::read() {
                    Ok(TermEvent::Key(key)) if key.kind != KeyEventKind::Release => key,
                    Ok(TermEvent::Mouse(mouse)) => {
                        if editor.is_some() {
                            // The box: a right-click paste, or on to the output.
                            to_box(TermEvent::Mouse(mouse));
                        } else if let Some(view) = &view {
                            // Inline there is no box to paste into.
                            let _ = mouse_event(view, mouse);
                        }
                        continue;
                    }
                    Ok(paste @ TermEvent::Paste(_)) => {
                        to_box(paste);
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
                        // With text selected, Ctrl-C copies it and is not a step towards quitting.
                        if view.as_deref().is_some_and(copy_selection) {
                            continue;
                        }
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
                    _ => {
                        if to_box(TermEvent::Key(key)) == Busy::Full {
                            notifier.show(QUEUE_FULL);
                        }
                    }
                }
            }
        });
        self.watch = Some(Watch {
            stop,
            thread: Some(thread),
            _raw: raw,
        });
        // While a task runs the progress marks show where the work is; the
        // terminal cursor would only blink next to them.
        screen_write("\x1b[?25l");
    }

    /// Stops reading keys and leaves raw mode; needed before the box or a question reads keys.
    fn watch_stop(&mut self) {
        if let Some(mut watch) = self.watch.take() {
            watch.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = watch.thread.take() {
                let _ = thread.join();
            }
            self.editor().set_busy(false);
            // Back for the input box, a question or the shell.
            screen_write("\x1b[?25h");
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
            self.editor().redraw_dock();
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
            // The tab title is saved first; it comes back when the chat closes.
            screen_write("\x1b[22;0t\x1b[?1049h\x1b[2J\x1b[H");
            if self.mouse {
                // Button and wheel reports, in the SGR form.
                // Also motion while a button is down: selecting text by dragging.
                screen_write("\x1b[?1000h\x1b[?1002h\x1b[?1006h");
            }
            let (columns, rows) =
                crossterm::terminal::size().map_or((80, 24), |(c, r)| (c as usize, r as usize));
            let view = Arc::new(Mutex::new(View::new(s, columns, rows.saturating_sub(4))));
            self.editor().set_view(view.clone());
            self.spinner.set_view(view.clone());
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
            if self.editor().open_dock().is_ok() {
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
        // A prompt sent while the last task ran goes first; the box keeps what is typed in it.
        let queued = self
            .queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pop_front();
        if let Some(Queued { text, files }) = queued {
            let _ = self.editor().redraw_idle();
            self.write_title();
            return self.submitted(text, files);
        }
        if !self.screen {
            self.blank();
        }
        let read = self.editor().read("", INPUT_HINTS, true);
        match read {
            Ok(Input::Submit(text)) => {
                let files = self.editor().take_files();
                self.submitted(text, files)
            }
            Ok(Input::Exit) => None,
            Err(err) => {
                self.error(&format!("input: {err}"));
                None
            }
        }
    }

    /// A prompt that is about to run (typed now or taken from the queue): echoed
    /// with its files, flagged files confirmed. `Some("")` = nothing to run.
    fn submitted(&mut self, text: String, files: Vec<Attachment>) -> Option<String> {
        // A command is not part of the conversation: it is not echoed, its effect is shown.
        if COMMANDS.iter().any(|(name, _)| *name == text) {
            if !files.is_empty() {
                self.info("The dropped files were not sent: a command takes no files.");
            }
            return Some(text);
        }
        self.echo(&text, &files);
        match self.confirm_files(files) {
            Some(files) if text.is_empty() && files.is_empty() => Some(String::new()),
            Some(files) => {
                self.files = files;
                Some(text)
            }
            None => {
                self.info("Cancelled: nothing was sent.");
                Some(String::new())
            }
        }
    }

    /// One line of an action's output, indented under the action. Inline the
    /// progress line shares the row, so it steps aside for the moment.
    fn body_line(&mut self, line: &str) {
        let s = self.style;
        let inline_spinner = self.view.is_none();
        if inline_spinner {
            self.spinner.stop();
        }
        self.line(&format!(
            "{}    {}{}",
            s.gray(),
            s.links(line, s.gray()),
            s.reset()
        ));
        self.live_lines += 1;
        if inline_spinner {
            self.spinner.start("Running");
        }
    }

    /// The files submitted with the last input; empty after the call.
    pub fn take_files(&mut self) -> Vec<Attachment> {
        std::mem::take(&mut self.files)
    }

    /// Files that look private (keys, passwords, tokens) need a yes before they
    /// leave the machine: `y` sends all, `s` sends the others, Esc or `n` cancels
    /// the prompt (`None`). Files without a warning pass unasked.
    fn confirm_files(&mut self, files: Vec<Attachment>) -> Option<Vec<Attachment>> {
        if files.iter().all(|file| file.warning.is_none()) {
            return Some(files);
        }
        let s = self.style;
        for file in files.iter().filter(|file| file.warning.is_some()) {
            self.line(&format!(
                "{}  ⚠ {} looks private: {}{}",
                s.action(),
                file.name,
                file.warning.as_deref().unwrap_or_default(),
                s.reset()
            ));
        }
        let others = files.iter().any(|file| file.warning.is_none());
        let mut keys = String::from("yn");
        let mut choices = format!("{}y{} yes", s.bold(), s.reset());
        if others {
            keys.push('s');
            choices.push_str(&format!(" · {}s{} skip the flagged", s.bold(), s.reset()));
        }
        choices.push_str(&format!(" · {}n{} cancel", s.bold(), s.reset()));
        self.partial(&format!(
            "{}  Send to DeepSeek?{} {choices} ",
            s.accent(),
            s.reset()
        ));
        let choice = editor::read_choice(&keys).ok().flatten();
        self.erase_partial();
        match choice {
            Some('y') => Some(files),
            Some('s') => {
                let kept: Vec<Attachment> = files
                    .into_iter()
                    .filter(|file| file.warning.is_none())
                    .collect();
                self.line(&format!(
                    "{}  sending without the flagged files{}",
                    s.gray(),
                    s.reset()
                ));
                Some(kept)
            }
            _ => None,
        }
    }

    /// The submitted input as part of the scrollback: with colors, a bar with its
    /// own background over the whole width, so a prompt is easy to find. Dropped
    /// files follow on the bar, one per row.
    fn echo(&mut self, text: &str, files: &[Attachment]) {
        let s = self.style;
        self.blank();
        // `ESC[K` fills the rest of the row with the background.
        let fill = if s.on() { "\x1b[K" } else { "" };
        // With colors an empty row of the bar above and below the text.
        let pad = format!("{}{fill}{}", s.prompt_bg(), s.reset());
        if s.on() {
            self.line(&pad);
        }
        for (i, line) in text.lines().enumerate() {
            let mark = if i == 0 { ">" } else { " " };
            self.line(&format!(
                "{bg}{bold}{accent} {mark} {reset}{bg}{bold}{line} {fill}{reset}",
                bg = s.prompt_bg(),
                bold = s.bold(),
                accent = s.accent(),
                reset = s.reset(),
            ));
        }
        // An empty row of the bar between the text and the files.
        if s.on() && !text.is_empty() && !files.is_empty() {
            self.line(&pad);
        }
        for (i, file) in files.iter().enumerate() {
            let mark = if i == 0 && text.is_empty() { ">" } else { " " };
            let (icon, color) = match file.warning {
                Some(_) => ("⚠", s.action()),
                None => ("📎", s.key()),
            };
            self.line(&format!(
                "{bg}{accent} {mark} {reset}{bg}{color}{icon} {label} {fill}{reset}",
                bg = s.prompt_bg(),
                accent = s.accent(),
                reset = s.reset(),
                label = file.label(),
            ));
        }
        if s.on() {
            self.line(&pad);
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
        // In the window the terminal has the mouse back: text can be selected and
        // copied the terminal's way, links can be clicked, a right click pastes.
        let app_mouse = self.screen && self.mouse;
        if app_mouse {
            screen_write("\x1b[?1006l\x1b[?1002l\x1b[?1000l");
        }
        let token = dialog::ask_token(self.style, reason, !self.screen)
            .ok()
            .flatten();
        if app_mouse {
            screen_write("\x1b[?1000h\x1b[?1002h\x1b[?1006h");
        }
        self.redraw();
        token
    }

    /// Screen layout: the output area with its foldable blocks, as JSON, to
    /// save it with a conversation. `None` in the inline layout.
    pub fn output(&self) -> Option<String> {
        let view = self.view.as_ref()?;
        let snapshot = view
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .snapshot();
        serde_json::to_string(&snapshot).ok()
    }

    /// Screen layout: replaces the output area by a saved one (what `output`
    /// gave for the conversation switched to; empty = a clean area). Text that
    /// is not such a save is shown as plain lines.
    pub fn show_output(&mut self, saved: &str) {
        self.close_output();
        if let Some(view) = &self.view {
            let mut view = view.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            match serde_json::from_str(saved) {
                Ok(snapshot) => view.restore(snapshot),
                Err(_) => view.replace(saved),
            }
            self.gap = saved.is_empty();
            self.live.clear();
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

    /// Screen layout: puts up the status line for the answer that is awaited: a
    /// line of its own at the start of the answer, where the spinner shows what
    /// is going on. The progress thread animates it; `end_status` removes it.
    fn begin_status(&mut self) {
        if self.status_on {
            return;
        }
        let Some(view) = &self.view else {
            return;
        };
        view.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .begin_status();
        self.status_on = true;
        // The line itself; the next tick of the progress thread fills it.
        self.line(&format!("{}…{}", self.style.gray(), self.style.reset()));
    }

    /// Takes the status line away again; the answer moves up into its place.
    fn end_status(&mut self) {
        if std::mem::take(&mut self.status_on)
            && let Some(view) = &self.view
        {
            view.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .end_status();
        }
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
        self.set_tab_state(TabState::Working);
        self.watch_start();
    }

    /// End of a task: one line with the time it took, the answers, the actions
    /// and the requests sent to the DeepSeek API.
    pub fn finish(&mut self, result: &anyhow::Result<()>, requests: u64) {
        self.close_output();
        // A step that failed or was stopped left its status line up.
        self.end_status();
        self.set_tab_state(TabState::Idle);
        self.spinner.stop();
        self.watch_stop();
        self.blank();
        let s = self.style;
        let secs = self.started.elapsed().as_secs_f32();
        let plural = |n: u32, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        // `requests` = calls to the DeepSeek API this task made: what it cost the account.
        let stats = format!(
            "{secs:.1}s · {} · {} · {}",
            plural(self.answers, "answer"),
            plural(self.actions, "action"),
            plural(requests.min(u32::MAX as u64) as u32, "request")
        );
        let (mark, color, text) = match result {
            Ok(()) => ("✓", s.ok(), stats),
            Err(err) => ("✗", s.err(), format!("{err:#} · {stats}")),
        };
        if s.on() {
            // A bar with its own background, like the prompt: the end of a task is
            // easy to find. The terminal fills the row (`ESC[K`), so nothing has to
            // be as long as the screen and nothing wraps on a narrow one.
            let columns = crossterm::terminal::size().map_or(80, |(columns, _)| columns as usize);
            // A long error is cut so the line stays one row.
            let room = columns.saturating_sub(6);
            let text = match text.chars().count() > room {
                true => format!(
                    "{}…",
                    text.chars()
                        .take(room.saturating_sub(1))
                        .collect::<String>()
                ),
                false => text,
            };
            let bg = if result.is_ok() {
                s.done_bg()
            } else {
                s.failed_bg()
            };
            self.line(&format!(
                "{bg} {color}{bold}{mark}{reset}{bg} {key}{text} \x1b[K{reset}",
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
            row(
                "Click on ▸ or ▾, Ctrl-O",
                "unfold or fold the output of an action (Ctrl-O: the last one)",
            ),
            row(
                "Drag with the mouse",
                "select text (Shift+drag: the terminal's own selection)",
            ),
            row("Right click, Ctrl-C", "copy the selected text"),
            row(
                "Right click, Ctrl-V",
                "paste into the input box (right click: when nothing is selected)",
            ),
            row("Ctrl-A, Ctrl-E", "start, end of the line"),
            row(
                "Ctrl-W, Ctrl-U, Ctrl-K",
                "delete word, to the start, to the end",
            ),
            row("Esc", "clear the box"),
            String::new(),
            row(
                "Drop files on the box",
                "attach them; Backspace in an empty box drops the last",
            ),
            String::new(),
            heading("While a task runs"),
            row("Esc", "stop the task"),
            row(
                "Type, Enter",
                "queue the next prompt (up to 5); queued prompts run in turn",
            ),
            row(
                "Alt+1…5, Ctrl+1…5",
                "drop a queued prompt (Ctrl+N where the terminal reports it)",
            ),
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
                let read = self.editor().read(cmd, EDIT_HINTS, false);
                match read {
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
            self.editor().close_dock();
            if self.mouse {
                screen_write("\x1b[?1006l\x1b[?1002l\x1b[?1000l");
            }
            // Back on the normal screen the conversation is printed once, so it
            // stays in the terminal after the chat closed.
            screen_write("\x1b[?1049l\x1b[23;0t");
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
        self.begin_status();
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
        self.end_status();
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
        self.set_tab_state(TabState::Asking);
        let decision = self.question(action, can_always);
        self.set_tab_state(TabState::Working);
        self.watch_start();
        decision
    }

    fn running(&mut self, _: &Action) {
        self.actions += 1;
        self.live.clear();
        self.live_lines = 0;
        // Screen layout: what the action prints becomes a block that can be folded.
        if let Some(view) = &self.view {
            view.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .begin_fold();
            self.line(&format!(
                "{}  ⎿ running…{}",
                self.style.gray(),
                self.style.reset()
            ));
        }
        self.spinner.start("Running");
    }

    fn output(&mut self, piece: &str) {
        self.live.push_str(&clean_output(piece));
        while let Some(end) = self.live.find('\n') {
            let line: String = self.live.drain(..=end).collect();
            self.body_line(line.trim_end_matches('\n'));
        }
    }

    fn result(&mut self, text: &str) {
        self.close_output();
        // A denial was already shown by the question or the policy note.
        if text.starts_with("Denied") {
            self.blank();
            return;
        }
        let s = self.style;
        if !self.live.is_empty() {
            let rest = std::mem::take(&mut self.live);
            self.body_line(&rest);
        }
        // The text is the message for the model: its first line is the outcome, the rest the output.
        let mut lines =
            text.lines()
                .filter(|line| *line != "--- stdout ---")
                .map(|line| match line {
                    "--- stderr ---" => "stderr:",
                    line => line.strip_prefix("Result: ").unwrap_or(line),
                });
        let outcome = lines
            .next()
            .unwrap_or("")
            .trim_matches('-')
            .trim()
            .to_string();
        // Nothing was shown live (a file read, a command without output): the body comes from the result.
        if self.live_lines == 0 {
            for line in lines.filter(|line| *line != "(no output)") {
                self.body_line(line);
            }
        }
        let summary = match self.live_lines {
            0 => outcome,
            1 => format!("{outcome} · 1 line"),
            n => format!("{outcome} · {n} lines"),
        };
        // A failed action stands out: red with a cross; a good one gets a green check.
        let base = match outcome_failed(&summary) {
            true => format!("{}  ⎿ ✗ {summary}{}", s.err(), s.reset()),
            false => format!(
                "{}  ⎿ {}✓{}{} {summary}{}",
                s.gray(),
                s.ok(),
                s.reset(),
                s.gray(),
                s.reset()
            ),
        };
        match &self.view {
            Some(view) => {
                // The block ends: a long output folds to its header; a click or Ctrl-O opens it.
                let switch = |mark: &str, what: &str| {
                    format!(
                        "{base}  {}{mark}{} {}{what}{}",
                        s.accent(),
                        s.reset(),
                        s.faint(),
                        s.reset()
                    )
                };
                let open_text = if self.live_lines > 0 {
                    switch("▾", "hide")
                } else {
                    base.clone()
                };
                let mut view = view.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                view.end_fold(
                    switch("▸", "show"),
                    open_text,
                    self.live_lines > FOLD_OPEN_LINES,
                );
                view.repaint();
            }
            None => self.line(&base),
        }
        self.gap = false;
        self.blank();
    }

    fn result_is_answer(&mut self) {
        self.close_output();
        // The output is what the user asked for: it must be visible.
        if let Some(view) = &self.view {
            view.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .unfold_last();
        }
    }

    fn note(&mut self, text: &str) {
        self.close_output();
        self.line(&self.style.inline(&format!("  {text}"), self.style.gray()));
        self.gap = false;
    }
}

#[cfg(test)]
mod tests {
    use super::{Tab, clean_output, outcome_failed, tab_text};

    #[test]
    fn the_tab_counts_queued_prompts() {
        let tab = Tab {
            icon: "⏳",
            title: "dsc · fix the build".into(),
        };
        assert_eq!(tab_text(&tab, 0), "⏳ dsc · fix the build");
        assert_eq!(tab_text(&tab, 3), "⏳ +3 dsc · fix the build");
    }

    #[test]
    fn failed_outcomes_are_recognized() {
        for failed in [
            "Error: read x: No such file or directory (os error 2)",
            "exit code 1 · 3 lines",
            "exit code 127",
            "timed out, killed",
            "killed by a signal",
        ] {
            assert!(outcome_failed(failed), "{failed}");
        }
        for good in [
            "exit code 0",
            "exit code 0 · 12 lines",
            "Written: a.txt (3 bytes)",
            "src/main.rs · 40 lines",
        ] {
            assert!(!outcome_failed(good), "{good}");
        }
    }

    #[test]
    fn command_output_is_cleaned() {
        assert_eq!(
            clean_output("a\x1b[31mred\x1b[0m\tb\r\nc\rd\x07\x1b]0;title\x07e\n"),
            "ared    b\nc\nde\n"
        );
    }
}
