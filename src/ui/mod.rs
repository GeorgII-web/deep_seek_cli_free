//! The terminal side of the agent: input box, streamed answer, progress line,
//! action lines, approval question, result preview, finish line.
//!
//! Answer text goes to stdout, everything else to stderr. Without a terminal
//! there are no colors, no spinner and no box: plain lines only.

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
use std::time::Instant;
use style::Style;

/// Lines of a result shown to the user (the model gets the capped full text).
const RESULT_LINES: usize = 8;
const INPUT_HINT: &str =
    "Enter send · Alt+Enter or \\ + Enter new line · ↑↓ history · /help · Ctrl-D quit";

pub struct Terminal {
    style: Style,
    /// Keys can be read one by one: stdin and stderr are a terminal
    interactive: bool,
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

    /// The head of a chat.
    pub fn banner(&self, model: &str, cwd: &str) {
        let s = self.style;
        eprintln!(
            "{}{}✻ dsc{} {}{} · DeepSeek web · {model}{}",
            s.bold(),
            s.accent(),
            s.reset(),
            s.gray(),
            env!("CARGO_PKG_VERSION"),
            s.reset()
        );
        eprintln!("{}  {cwd}{}", s.gray(), s.reset());
        eprintln!("{}  /help for commands{}", s.gray(), s.reset());
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
        eprintln!();
        match self.editor.read("", INPUT_HINT, true) {
            Ok(Input::Submit(text)) => {
                self.echo(&text);
                self.gap = true;
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
    fn echo(&self, text: &str) {
        let s = self.style;
        for (i, line) in text.lines().enumerate() {
            let mark = if i == 0 { ">" } else { " " };
            eprintln!(
                "{}{}{mark}{} {}{line}{}",
                s.bold(),
                s.accent(),
                s.reset(),
                s.bold(),
                s.reset()
            );
        }
        eprintln!();
    }

    /// One blank line, unless the last thing printed was one.
    fn blank(&mut self) {
        if !std::mem::replace(&mut self.gap, true) {
            eprintln!();
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
        let s = self.style;
        let secs = self.started.elapsed().as_secs_f32();
        let plural = |n: u32, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        let stats = format!(
            "{secs:.1}s · {} · {}",
            plural(self.answers, "answer"),
            plural(self.actions, "action")
        );
        match result {
            Ok(()) => eprintln!("{}✓{} {}{stats}{}", s.ok(), s.reset(), s.gray(), s.reset()),
            Err(err) => eprintln!(
                "{}✗ {err:#}{} {}· {stats}{}",
                s.err(),
                s.reset(),
                s.gray(),
                s.reset()
            ),
        }
    }

    /// A message of the CLI itself that is not part of a task.
    pub fn info(&self, text: &str) {
        eprintln!("{}{text}{}", self.style.gray(), self.style.reset());
    }

    pub fn error(&self, text: &str) {
        eprintln!("{}✗ {text}{}", self.style.err(), self.style.reset());
    }

    /// Ends whatever is on the screen (spinner, open answer line) before chrome is printed.
    fn close_output(&mut self) {
        self.spinner.stop();
        if std::mem::take(&mut self.thinking_open) || std::mem::take(&mut self.text_open) {
            println!();
            let _ = std::io::stdout().flush();
        }
    }

    fn print_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.spinner.stop();
        if std::mem::take(&mut self.thinking_open) {
            println!("\n");
        }
        print!("{text}");
        let _ = std::io::stdout().flush();
        self.text_open = true;
        self.gap = false;
    }
}

impl Ui for Terminal {
    fn event(&mut self, event: &Event) {
        match event {
            Event::Status(label) => {
                // A status in the middle of printed text would tear the line.
                if !self.text_open && !self.thinking_open {
                    self.spinner.start(label);
                }
            }
            Event::Thinking(text) => {
                self.spinner.stop();
                print!("{}{text}{}", self.style.dim(), self.style.reset());
                let _ = std::io::stdout().flush();
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
        eprintln!(
            "{}●{} {}{kind}{} {}{}{}",
            s.action(),
            s.reset(),
            s.bold(),
            s.reset(),
            s.action(),
            lines.next().unwrap_or(""),
            s.reset()
        );
        for line in lines {
            eprintln!("{}      {line}{}", s.action(), s.reset());
        }
        if !action.why().is_empty() {
            eprintln!("{}  {}{}", s.gray(), action.why(), s.reset());
        }
    }

    fn approve(&mut self, action: &Action, can_always: bool) -> Decision {
        self.close_output();
        let s = self.style;
        if !self.interactive {
            eprintln!(
                "{}  no terminal to ask on: denied (use --yes or policy.allow){}",
                s.gray(),
                s.reset()
            );
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
        eprint!("{}  Allow?{} {choices} ", s.accent(), s.reset());
        let _ = std::io::stderr().flush();
        let choice = editor::read_choice(&keys).ok().flatten();
        // The question is replaced by its outcome.
        eprint!("\r\x1b[2K");
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
                        eprintln!("{}  ✗ denied{}", s.err(), s.reset());
                        Decision::No
                    }
                }
            }
            _ => {
                eprintln!("{}  ✗ denied{}", s.err(), s.reset());
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
            eprintln!("{}  {mark} {line}{}", s.gray(), s.reset());
        }
        if lines.len() > RESULT_LINES {
            eprintln!(
                "{}    … +{} lines{}",
                s.gray(),
                lines.len() - RESULT_LINES,
                s.reset()
            );
        }
        self.gap = false;
        self.blank();
    }

    fn note(&mut self, text: &str) {
        self.close_output();
        eprintln!("{}  {text}{}", self.style.gray(), self.style.reset());
        self.gap = false;
    }
}
