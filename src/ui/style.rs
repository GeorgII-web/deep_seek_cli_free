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
    /// `text` in the color `base`, with every `code` span in the code color.
    /// Without colors the text stays as it is, backticks included.
    pub fn inline(&self, text: &str, base: &'static str) -> String {
        if !self.on {
            return text.to_string();
        }
        let mut out = String::from(base);
        for (i, part) in text.split('`').enumerate() {
            if i % 2 == 1 {
                out.push_str(&format!(
                    "{}{}{part}{}{base}",
                    self.reset(),
                    self.code_text(),
                    self.reset()
                ));
            } else {
                out.push_str(&self.links(part, base));
            }
        }
        out.push_str(self.reset());
        out
    }

    /// Foreground and background swapped: the focused button of a dialog
    pub fn inverse(&self) -> &'static str {
        self.code("\x1b[7m")
    }
    /// Background of the user's prompt in the output
    pub fn prompt_bg(&self) -> &'static str {
        self.code("\x1b[48;5;237m")
    }
    /// Keys in a hint line: the lighter of the two grays
    pub fn key(&self) -> &'static str {
        self.code("\x1b[38;5;252m")
    }
    /// What a key does, in a hint line: the darker gray
    pub fn faint(&self) -> &'static str {
        self.code("\x1b[38;5;242m")
    }
    /// `⏵ ask` (green) or `⏵⏵ allow all` (orange), for the hint and progress lines.
    pub fn mode_label(&self, allow_all: bool) -> String {
        match allow_all {
            true => format!("{}⏵⏵ allow all{}", self.action(), self.reset()),
            false => format!("{}⏵ ask{}", self.ok(), self.reset()),
        }
    }
    /// A hint: the key light, its meaning darker.
    pub fn hint(&self, key: &str, what: &str) -> String {
        let space = if what.is_empty() { "" } else { " " };
        format!(
            "{}{key}{}{}{space}{what}{}",
            self.key(),
            self.reset(),
            self.faint(),
            self.reset()
        )
    }
    /// The escape sequences that make the text between them a terminal hyperlink
    /// (OSC 8): the terminal shows it as a link on hover and opens it on click
    /// (Ctrl+click in most). Terminals without support ignore them.
    pub fn hyperlink(&self, url: &str) -> (String, String) {
        match self.on {
            true => (format!("\x1b]8;;{url}\x1b\\"), "\x1b]8;;\x1b\\".to_string()),
            false => (String::new(), String::new()),
        }
    }
    /// URLs: blue, not underlined. The terminal underlines a hyperlink under the
    /// mouse; an underline of our own would hide that hover effect.
    pub fn link(&self) -> &'static str {
        self.code("\x1b[38;5;39m")
    }
    /// `text` in the color `base` with every `http(s)://` URL in the link color.
    /// A URL ends at whitespace or a closing mark; punctuation at its end stays outside.
    pub fn links(&self, text: &str, base: &'static str) -> String {
        if !self.on {
            return text.to_string();
        }
        let mut out = String::new();
        let mut rest = text;
        while let Some(start) = ["https://", "http://"]
            .iter()
            .filter_map(|scheme| rest.find(scheme))
            .min()
        {
            // Not in the middle of a word.
            let inside = rest[..start]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric);
            let tail = &rest[start..];
            let end = tail
                .find(|c: char| c.is_whitespace() || ")>]\"'`".contains(c))
                .unwrap_or(tail.len());
            let url = tail[..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
            out.push_str(&rest[..start]);
            if inside || url.ends_with("://") {
                out.push_str(url);
            } else {
                let (open, close) = self.hyperlink(url);
                out.push_str(&format!(
                    "{}{}{open}{url}{close}{}{base}",
                    self.reset(),
                    self.link(),
                    self.reset()
                ));
            }
            rest = &tail[url.len()..];
        }
        out.push_str(rest);
        out
    }
    /// Borders, hints, meta lines
    pub fn gray(&self) -> &'static str {
        self.code("\x1b[38;5;245m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_colors_code_spans_only_with_colors_on() {
        assert_eq!(
            Style::new(false).inline("see `/help` now", "x"),
            "see `/help` now"
        );
        let s = Style::new(true);
        let (gray, code, reset) = (s.gray(), s.code_text(), s.reset());
        assert_eq!(
            s.inline("see `/help` now", gray),
            format!("{gray}see {reset}{code}/help{reset}{gray} now{reset}")
        );
    }

    #[test]
    fn links_colors_urls_and_leaves_the_rest() {
        let s = Style::new(true);
        let reset = s.reset();
        let hyper = |url: &str| {
            let (open, close) = s.hyperlink(url);
            format!("{reset}{}{open}{url}{close}{reset}B", s.link())
        };
        assert_eq!(
            s.links("go to https://a.org/x, then", "B"),
            format!("go to {}, then", hyper("https://a.org/x"))
        );
        assert_eq!(
            s.links("(http://a.org) xhttp://b no", "B"),
            format!("({}) xhttp://b no", hyper("http://a.org"))
        );
        assert_eq!(s.links("no link here", "B"), "no link here");
        assert_eq!(
            Style::new(false).links("https://a.org", "B"),
            "https://a.org"
        );
        assert_eq!(
            Style::new(false).hyperlink("https://a.org"),
            (String::new(), String::new())
        );
        // Inside `inline` too.
        let wrapped = "\x1b]8;;https://a.org\x1b\\https://a.org\x1b]8;;\x1b\\";
        assert!(s.inline("see https://a.org", s.gray()).contains(wrapped));
    }
}
