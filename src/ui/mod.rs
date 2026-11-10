//! The terminal side of the agent: input box, streamed answer, progress line,
//! action lines, approval question, result preview, finish line.
//!
//! Two layouts. Inline (a single task, or no terminal): output follows the
//! shell prompt, answer text on stdout, everything else on stderr. Screen (the
//! chat on a terminal): the screen is cleared, the input box is docked at the
//! bottom, output scrolls above it; everything goes to stdout.
//! Without a terminal there are no colors, no spinner and no box.

pub mod editor;
pub mod render;
pub mod spinner;
pub mod style;

use crate::agent::protocol::Action;
use crate::agent::run::{Decision, Ui};
use crate::backend::Event;
use editor::{Editor, Input};
use render::Markdown;
use spinner::Spinner;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;
use style::Style;

/// Lines of a result shown to the user (the model gets the capped full text).
const RESULT_LINES: usize = 8;
const INPUT_HINT: &str =
    "Enter send · Alt+Enter or \\ + Enter new line · ↑↓ history · /help · Ctrl-D quit";

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
}

impl Terminal {
    /// `history_file` keeps the chat inputs between runs.
    pub fn new(history_file: Option<PathBuf>) -> Self {
        let style = Style::detect();
        let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
        Self {
            style,
            interactive,
            screen: false,
            spinner: Spinner::new(std::io::stderr().is_terminal(), style),
            markdown: Markdown::new(style),
            editor: Editor::new(style, history_file),
            text_open: false,
            thinking_open: false,
            gap: false,
            started: Instant::now(),
            answers: 0,
            actions: 0,
        }
    }

    /// A line of chrome: stderr inline, the output area in the screen layout.
    fn line(&self, text: &str) {
        let _screen = screen_lock();
        if self.screen {
            println!("{text}");
        } else {
            eprintln!("{text}");
        }
    }

    /// Chrome without a line end (a question that waits for a key).
    fn partial(&self, text: &str) {
        let _screen = screen_lock();
        if self.screen {
            print!("{text}");
            let _ = std::io::stdout().flush();
        } else {
            eprint!("{text}");
            let _ = std::io::stderr().flush();
        }
    }

    /// Answer text: always stdout.
    fn text(&self, text: &str) {
        let _screen = screen_lock();
        print!("{text}");
        let _ = std::io::stdout().flush();
    }

    /// Starts the chat: on a terminal the screen is cleared and the input box is
    /// docked at the bottom for the rest of the session.
    pub fn open_chat(&mut self, model: &str, cwd: &str) {
        let s = self.style;
        let docked = self.interactive && std::io::stdout().is_terminal();
        if docked {
            screen_write("\x1b[2J\x1b[H");
            self.screen = true;
        }
        self.line(&format!(
            "{}{}✻ dsc{} {}{} · DeepSeek web · {model}{}",
            s.bold(),
            s.accent(),
            s.reset(),
            s.gray(),
            env!("CARGO_PKG_VERSION"),
            s.reset()
        ));
        self.line(&format!("{}  {cwd}{}", s.gray(), s.reset()));
        self.line(&format!("{}  /help for commands{}", s.gray(), s.reset()));
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
        match self.editor.read("", INPUT_HINT, true) {
            Ok(Input::Submit(text)) => {
                self.echo(&text);
                Some(text)
            }
            Ok(Input::Exit) => None,
            Err(err) => {
                self.error(&format!("input: {err}"));
                None
            }
        }
    }

    /// The submitted input as part of the scrollback.
    fn echo(&mut self, text: &str) {
        let s = self.style;
        self.blank();
        for (i, line) in text.lines().enumerate() {
            let mark = if i == 0 { ">" } else { " " };
            self.line(&format!(
                "{}{}{mark}{} {}{line}{}",
                s.bold(),
                s.accent(),
                s.reset(),
                s.bold(),
                s.reset()
            ));
        }
        self.gap = false;
        self.blank();
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
    }

    /// End of a task: one line with the time it took.
    pub fn finish(&mut self, result: &anyhow::Result<()>) {
        self.close_output();
        self.spinner.stop();
        let s = self.style;
        let secs = self.started.elapsed().as_secs_f32();
        let plural = |n: u32, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        let stats = format!(
            "{secs:.1}s · {} · {}",
            plural(self.answers, "answer"),
            plural(self.actions, "action")
        );
        match result {
            Ok(()) => self.line(&format!(
                "{}✓{} {}{stats}{}",
                s.ok(),
                s.reset(),
                s.gray(),
                s.reset()
            )),
            Err(err) => self.line(&format!(
                "{}✗ {err:#}{} {}· {stats}{}",
                s.err(),
                s.reset(),
                s.gray(),
                s.reset()
            )),
        }
        self.gap = false;
    }

    /// A message of the CLI itself that is not part of a task.
    pub fn info(&mut self, text: &str) {
        for line in text.lines() {
            self.line(&format!(
                "{}{line}{}",
                self.style.gray(),
                self.style.reset()
            ));
        }
        self.gap = false;
    }

    pub fn error(&mut self, text: &str) {
        self.line(&format!(
            "{}✗ {text}{}",
            self.style.err(),
            self.style.reset()
        ));
        self.gap = false;
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

impl Drop for Terminal {
    fn drop(&mut self) {
        self.spinner.stop();
        if self.screen {
            self.editor.close_dock();
            self.text("\n");
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
            self.line(&format!("{}  {}{}", s.gray(), action.why(), s.reset()));
        }
    }

    fn approve(&mut self, action: &Action, can_always: bool) -> Decision {
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
        self.partial("\r\x1b[2K");
        let denied = format!("{}  ✗ denied{}", s.err(), s.reset());
        match choice {
            Some('y') => Decision::Yes,
            Some('a') => Decision::Always,
            Some('e') => {
                let Action::Shell { cmd, .. } = action else {
                    return Decision::No;
                };
                match self.editor.read(cmd, "Enter run · Ctrl-D cancel", false) {
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

    fn running(&mut self, _: &Action) {
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
            self.line(&format!("{}  {mark} {line}{}", s.gray(), s.reset()));
        }
        if lines.len() > RESULT_LINES {
            self.line(&format!(
                "{}    … +{} lines{}",
                s.gray(),
                lines.len() - RESULT_LINES,
                s.reset()
            ));
        }
        self.gap = false;
        self.blank();
    }

    fn note(&mut self, text: &str) {
        self.close_output();
        self.line(&format!(
            "{}  {text}{}",
            self.style.gray(),
            self.style.reset()
        ));
        self.gap = false;
    }
}
