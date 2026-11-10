//! The terminal side of the agent loop: streamed answer, questions, results.

use crate::agent::protocol::Action;
use crate::agent::run::{Decision, Ui};
use crate::backend::Event;
use std::io::{IsTerminal, Write};

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";
/// Lines of a result shown to the user (the model gets the capped full text).
const RESULT_LINES: usize = 20;

#[derive(Default)]
pub struct Terminal {
    /// Something was printed on the current line
    open_line: bool,
}

impl Terminal {
    fn end_line(&mut self) {
        if std::mem::take(&mut self.open_line) {
            println!();
        }
    }

    fn read_line(prompt: &str) -> Option<String> {
        eprint!("{prompt}");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(n) if n > 0 => Some(line.trim().to_string()),
            _ => None,
        }
    }
}

impl Ui for Terminal {
    fn event(&mut self, event: &Event) {
        match event {
            Event::Thinking(text) => print!("{DIM}{text}{RESET}"),
            Event::Text(text) => print!("{text}"),
        }
        self.open_line = true;
        let _ = std::io::stdout().flush();
    }

    fn answer_end(&mut self) {
        self.end_line();
    }

    fn approve(&mut self, action: &Action, can_always: bool) -> Decision {
        self.end_line();
        eprintln!("{YELLOW}{BOLD}{}{RESET}", action.summary());
        if !action.why().is_empty() {
            eprintln!("{DIM}why: {}{RESET}", action.why());
        }
        if !std::io::stdin().is_terminal() {
            eprintln!("{DIM}no terminal to ask on: denied (use --yes or policy.allow){RESET}");
            return Decision::No;
        }
        let is_shell = matches!(action, Action::Shell { .. });
        let mut choices = String::from("[y] yes");
        if can_always {
            choices.push_str("  [a] always in this run");
        }
        choices.push_str("  [n] no");
        if is_shell {
            choices.push_str("  [e] edit");
        }
        loop {
            // End of input counts as no.
            let Some(answer) = Self::read_line(&format!("{choices} > ")) else {
                return Decision::No;
            };
            match answer.to_lowercase().as_str() {
                "y" | "yes" => return Decision::Yes,
                "a" | "always" if can_always => return Decision::Always,
                "n" | "no" => return Decision::No,
                "e" | "edit" if is_shell => match Self::read_line("command > ") {
                    Some(cmd) if !cmd.is_empty() => return Decision::Edit(cmd),
                    _ => continue,
                },
                _ => continue,
            }
        }
    }

    fn running(&mut self, action: &Action) {
        self.end_line();
        eprintln!("{YELLOW}{}{RESET}", action.summary());
    }

    fn result(&mut self, text: &str) {
        let lines: Vec<&str> = text.lines().collect();
        for line in lines.iter().take(RESULT_LINES) {
            eprintln!("{DIM}{line}{RESET}");
        }
        if lines.len() > RESULT_LINES {
            eprintln!("{DIM}[{} more lines]{RESET}", lines.len() - RESULT_LINES);
        }
    }

    fn note(&mut self, text: &str) {
        self.end_line();
        eprintln!("{DIM}dsc: {text}{RESET}");
    }
}
