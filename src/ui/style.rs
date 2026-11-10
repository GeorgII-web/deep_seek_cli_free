//! Colors. Every escape code comes from here, so "no color" is one switch.

use std::io::IsTerminal;

#[derive(Debug, Clone, Copy)]
pub struct Style {
    on: bool,
}

impl Style {
    /// Colors only on a real terminal and without `NO_COLOR`.
    pub fn detect() -> Self {
        let dumb = std::env::var("TERM").is_ok_and(|term| term == "dumb");
        let off = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
        Self {
            on: std::io::stdout().is_terminal() && std::io::stderr().is_terminal() && !dumb && !off,
        }
    }

    #[cfg(test)]
    pub const fn new(on: bool) -> Self {
        Self { on }
    }

    pub fn on(&self) -> bool {
        self.on
    }

    fn code(&self, code: &'static str) -> &'static str {
        if self.on { code } else { "" }
    }

    pub fn reset(&self) -> &'static str {
        self.code("\x1b[0m")
    }
    pub fn bold(&self) -> &'static str {
        self.code("\x1b[1m")
    }
    pub fn dim(&self) -> &'static str {
        self.code("\x1b[2m")
    }
    /// Prompt, headings, the brand color
    pub fn accent(&self) -> &'static str {
        self.code("\x1b[38;5;75m")
    }
    /// Actions the model asks for
    pub fn action(&self) -> &'static str {
        self.code("\x1b[38;5;179m")
    }
    pub fn ok(&self) -> &'static str {
        self.code("\x1b[38;5;114m")
    }
    pub fn err(&self) -> &'static str {
        self.code("\x1b[38;5;203m")
    }
    /// Code blocks and inline code
    pub fn code_text(&self) -> &'static str {
        self.code("\x1b[38;5;80m")
    }
    /// Borders, hints, meta lines
    pub fn gray(&self) -> &'static str {
        self.code("\x1b[38;5;245m")
    }
}
