//! Turns the streamed answer into what the user sees: the `dsc` action block and
//! DeepSeek's own tool-call markup (DSML) are hidden (the action line shows them), blank lines at the edges are dropped, and
//! with colors on a little markdown is styled (inline code, code blocks with a
//! bar instead of the fences, bold, headings).

use super::style::Style;
use crate::agent::protocol::FENCE;

/// How DeepSeek's own tool-call markup starts a line (`<｜DSML｜function_calls>`, with
/// one or two fullwidth bars).
const DSML_START: &str = "<｜";

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Plain,
    Code,
    Bold,
    Heading,
    Dim,
    Link,
}

pub struct Markdown {
    style: Style,
    /// Start of a line not shown yet: it may turn out to be a fence
    held: String,
    line_start: bool,
    /// Line ends not printed yet; printed before the next visible character
    pending_newlines: usize,
    emitted: bool,
    in_action: bool,
    /// Inside DSML tool calls (`<｜DSML｜…`): hidden until their closing `calls>` line
    in_dsml: bool,
    action_line: String,
    in_code: bool,
    inline_code: bool,
    bold: bool,
    heading: bool,
    /// A `*` that may be the first of `**`
    star: bool,
    /// Start of a word that may become `http://` or `https://`
    url_held: String,
    /// Inside a URL: colored until whitespace or a closing mark
    in_url: bool,
    /// Punctuation after URL characters: part of the link only when more of it follows
    url_punct: String,
    /// The last character printed was a letter or digit (a URL starts only at a word start)
    prev_word: bool,
    current: Look,
}

impl Markdown {
    pub fn new(style: Style) -> Self {
        Self {
            style,
            held: String::new(),
            line_start: true,
            pending_newlines: 0,
            emitted: false,
            in_action: false,
            in_dsml: false,
            action_line: String::new(),
            in_code: false,
            inline_code: false,
            bold: false,
            heading: false,
            star: false,
            url_held: String::new(),
            in_url: false,
            url_punct: String::new(),
            prev_word: false,
            current: Look::Plain,
        }
    }

    /// Takes the next piece of the answer; returns the text to print now.
    pub fn feed(&mut self, text: &str) -> String {
        let mut out = String::new();
        for c in text.chars() {
            self.push(c, &mut out);
        }
        out
    }

    /// End of the answer: returns the rest and resets for the next answer.
    pub fn finish(&mut self) -> String {
        let mut out = String::new();
        let held = std::mem::take(&mut self.held);
        // A fence as the very last line has no line end: with colors it is dropped like the others.
        let fence = held.trim().starts_with("```");
        let dsml = self.in_dsml || held.trim_start().starts_with(DSML_START);
        if !self.in_action && !dsml && held.trim() != FENCE && !(fence && self.style.on()) {
            for c in held.trim_end().chars() {
                if fence {
                    self.put(c, Look::Dim, &mut out);
                } else {
                    self.visible(c, &mut out);
                }
            }
        }
        self.flush_url(&mut out);
        self.flush_star(&mut out);
        self.set_look(Look::Plain, &mut out);
        *self = Self::new(self.style);
        out
    }

    fn push(&mut self, c: char, out: &mut String) {
        if self.in_dsml {
            if c == '\n' {
                let line = std::mem::take(&mut self.action_line);
                let line = line.trim();
                if line.starts_with("</") && line.contains("DSML") && line.ends_with("calls>") {
                    self.in_dsml = false;
                }
            } else {
                self.action_line.push(c);
            }
            return;
        }
        if self.in_action {
            if c == '\n' {
                if self.action_line.trim() == "```" {
                    self.in_action = false;
                }
                self.action_line.clear();
            } else {
                self.action_line.push(c);
            }
            return;
        }
        if c == '\n' {
            let held = std::mem::take(&mut self.held);
            let fence = held.trim();
            if fence == FENCE {
                self.in_action = true;
                return;
            }
            // DSML tool calls: hidden from here on (one call on a single line ends at once).
            if fence.starts_with(DSML_START) {
                self.in_dsml = !(fence.contains("calls>")
                    && fence.ends_with("calls>")
                    && fence.matches("calls>").count() > 1);
                return;
            }
            if fence.starts_with("```") {
                self.in_code = !self.in_code;
                if self.style.on() {
                    // The bar in front of the code lines marks the block; the fence line is dropped.
                    return;
                }
                for c in held.chars() {
                    self.put(c, Look::Dim, out);
                }
            } else if self.in_code && self.style.on() && self.line_start {
                // An empty line inside a block keeps its bar.
                self.code_bar(out);
            }
            self.end_line(out);
            return;
        }
        if self.line_start {
            self.held.push(c);
            let start = self.held.trim_start();
            if start.is_empty()
                || FENCE.starts_with(start)
                || start.starts_with("```")
                || DSML_START.starts_with(start)
                || start.starts_with(DSML_START)
            {
                return;
            }
            // Not a fence: an ordinary line.
            let held = std::mem::take(&mut self.held);
            self.line_start = false;
            self.heading = !self.in_code && held.trim_start().starts_with('#');
            if self.in_code && self.style.on() {
                self.code_bar(out);
            }
            for c in held.chars() {
                self.visible(c, out);
            }
            return;
        }
        self.visible(c, out);
    }

    fn end_line(&mut self, out: &mut String) {
        self.flush_url(out);
        self.flush_star(out);
        self.set_look(Look::Plain, out);
        self.inline_code = false;
        self.bold = false;
        self.heading = false;
        self.pending_newlines += 1;
        self.line_start = true;
        self.prev_word = false;
    }

    /// A character of an ordinary line; markdown marks are consumed when colors are on.
    fn visible(&mut self, c: char, out: &mut String) {
        if self.style.on() && !self.in_code {
            if !self.inline_code && self.url_step(c, out) {
                return;
            }
            if c == '`' {
                self.flush_star(out);
                self.inline_code = !self.inline_code;
                return;
            }
            if c == '*' && !self.inline_code {
                if std::mem::take(&mut self.star) {
                    self.bold = !self.bold;
                } else {
                    self.star = true;
                }
                return;
            }
            self.flush_star(out);
        }
        self.styled(c, out);
    }

    /// Prints `c` in the look of the place it stands in.
    fn styled(&mut self, c: char, out: &mut String) {
        let look = if self.in_code || self.inline_code {
            Look::Code
        } else if self.heading {
            Look::Heading
        } else if self.bold {
            Look::Bold
        } else {
            Look::Plain
        };
        self.put(c, look, out);
        self.prev_word = c.is_alphanumeric();
    }

    /// URL coloring, one character at a time. Returns true when `c` was consumed.
    /// A word that starts like `http://` or `https://` is held back until it is
    /// one; then everything up to whitespace or a closing mark is the link.
    fn url_step(&mut self, c: char, out: &mut String) -> bool {
        if self.in_url {
            if c.is_whitespace() || matches!(c, ')' | '>' | ']' | '"' | '\'' | '`' | '*') {
                self.flush_url(out);
                return false;
            }
            if matches!(c, '.' | ',' | ';' | ':' | '!' | '?') {
                // "see https://x.org." - the last dot is not part of the link.
                self.url_punct.push(c);
                return true;
            }
            let punct = std::mem::take(&mut self.url_punct);
            self.url_held.push_str(&punct);
            self.url_held.push(c);
            return true;
        }
        let candidate = format!("{}{c}", self.url_held);
        let starts = !self.url_held.is_empty() || !self.prev_word;
        if starts && (candidate == "http://" || candidate == "https://") {
            // From here the URL is collected: the terminal link needs it whole.
            self.url_held = candidate;
            self.in_url = true;
            return true;
        }
        if starts && ("http://".starts_with(&candidate) || "https://".starts_with(&candidate)) {
            self.url_held = candidate;
            return true;
        }
        self.flush_url(out);
        false
    }

    /// Ends a URL: it is printed as a terminal hyperlink (hover and click work in
    /// the terminal), punctuation after it as text. A word start that did not
    /// become a URL is printed as ordinary text.
    fn flush_url(&mut self, out: &mut String) {
        let held = std::mem::take(&mut self.url_held);
        let punct = std::mem::take(&mut self.url_punct);
        if std::mem::take(&mut self.in_url) && !held.ends_with("://") {
            let mut chars = held.chars();
            // The first character goes the normal way (line ends, look), the link wraps all.
            if let Some(first) = chars.next() {
                let (open, close) = self.style.hyperlink(&held);
                let mut piece = String::new();
                self.put(first, Look::Link, &mut piece);
                // `piece` = [line ends][color]first: the link opens right before the character.
                let at = piece.len() - first.len_utf8();
                out.push_str(&piece[..at]);
                out.push_str(&open);
                out.push(first);
                out.push_str(chars.as_str());
                out.push_str(&close);
            }
            self.prev_word = true;
        } else {
            for c in held.chars() {
                self.styled(c, out);
            }
        }
        for c in punct.chars() {
            self.styled(c, out);
        }
    }

    fn code_bar(&mut self, out: &mut String) {
        for c in "  │ ".chars() {
            self.put(c, Look::Dim, out);
        }
    }

    fn flush_star(&mut self, out: &mut String) {
        if std::mem::take(&mut self.star) {
            let look = if self.bold { Look::Bold } else { Look::Plain };
            self.put('*', look, out);
        }
    }

    fn put(&mut self, c: char, look: Look, out: &mut String) {
        if self.pending_newlines > 0 {
            // Nothing before the first text, at most one blank line inside.
            if self.emitted {
                out.push_str(&"\n".repeat(self.pending_newlines.min(2)));
            }
            self.pending_newlines = 0;
        }
        self.set_look(look, out);
        out.push(c);
        self.emitted = true;
    }

    fn set_look(&mut self, look: Look, out: &mut String) {
        if look == self.current {
            return;
        }
        if self.current != Look::Plain {
            out.push_str(self.style.reset());
        }
        match look {
            Look::Plain => {}
            Look::Code => out.push_str(self.style.code_text()),
            Look::Bold => out.push_str(self.style.bold()),
            Look::Heading => {
                out.push_str(self.style.bold());
                out.push_str(self.style.accent());
            }
            Look::Dim => out.push_str(self.style.gray()),
            Look::Link => out.push_str(self.style.link()),
        }
        self.current = look;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `text` in pieces of `chunk` characters.
    fn render(text: &str, color: bool, chunk: usize) -> String {
        let mut markdown = Markdown::new(Style::new(color));
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::new();
        for part in chars.chunks(chunk) {
            out.push_str(&markdown.feed(&part.iter().collect::<String>()));
        }
        out.push_str(&markdown.finish());
        out
    }

    #[test]
    fn dsml_tool_calls_are_hidden_in_any_chunking() {
        let answer = "I will fetch it.\n\n<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ invoke name=\"shell\">\n<｜｜DSML｜｜ parameter name=\"cmd\" string=\"true\">curl -sL 'https://ya.ru'</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>\nafter";
        for chunk in [1, 3, 7, 1000] {
            assert_eq!(
                render(answer, false, chunk),
                "I will fetch it.\n\nafter",
                "chunk {chunk}"
            );
        }
        // Without its closing line (the stream ended): hidden to the end.
        assert_eq!(
            render(
                "<｜DSML｜function_calls>\n<｜DSML｜invoke name=\"shell\">",
                false,
                5
            ),
            ""
        );
        // Text that only mentions it stays.
        assert_eq!(render("a <b> tag", false, 2), "a <b> tag");
    }

    #[test]
    fn action_block_is_hidden_in_any_chunking() {
        let answer = "\n\nLet me look.\n\n```dsc\n{\"tool\":\"shell\",\"cmd\":\"ls\"}\n```\n";
        for chunk in [1, 3, 1000] {
            assert_eq!(render(answer, false, chunk), "Let me look.");
        }
        assert_eq!(render("```dsc\n{\"tool\":\"shell\"}", false, 2), "");
        assert_eq!(
            render("before\n```dsc\n{}\n```\nafter", false, 4),
            "before\nafter"
        );
    }

    #[test]
    fn other_fences_and_text_pass_without_color() {
        let answer = "Run:\n```bash\nls *.txt\n```\nUse `x` and **y**.\n\n\n\nEnd";
        assert_eq!(
            render(answer, false, 1),
            "Run:\n```bash\nls *.txt\n```\nUse `x` and **y**.\n\nEnd"
        );
        assert_eq!(render("``not a fence", false, 1), "``not a fence");
    }

    #[test]
    fn color_styles_code_bold_and_headings() {
        let s = Style::new(true);
        let (code, bold, reset) = (s.code_text(), s.bold(), s.reset());
        assert_eq!(render("a `b` c", true, 1), format!("a {code}b{reset} c"));
        assert_eq!(
            render("**b** 2 * 3", true, 1),
            format!("{bold}b{reset} 2 * 3")
        );
        assert_eq!(
            render("# T\nx", true, 2),
            format!("{bold}{}# T{reset}\nx", s.accent())
        );
        // Inside a code block marks stay as they are; the fences become a bar.
        let block = render("x\n```bash\na*b `c`\n\nd\n```\ny", true, 1);
        let bar = format!("{}  │ {reset}", s.gray());
        assert_eq!(
            render("```\nd\n```", true, 1),
            format!("{bar}{code}d{reset}")
        );
        assert_eq!(
            block,
            format!("x\n{bar}{code}a*b `c`{reset}\n{bar}\n{bar}{code}d{reset}\ny")
        );
    }

    #[test]
    fn links_are_colored_up_to_their_end() {
        let s = Style::new(true);
        let (link, reset, code) = (s.link(), s.reset(), s.code_text());
        // Color, then the terminal hyperlink around the URL text.
        let hyper = |url: &str| {
            let (open, close) = s.hyperlink(url);
            format!("{link}{open}{url}{close}{reset}")
        };
        for chunk in [1, 5, 1000] {
            assert_eq!(
                render("see https://a.org/x?y=1.", true, chunk),
                format!("see {}.", hyper("https://a.org/x?y=1"))
            );
            assert_eq!(
                render("[docs](http://a.org/b) and more", true, chunk),
                format!("[docs]({}) and more", hyper("http://a.org/b"))
            );
            // A word that only starts like a scheme, and a scheme inside a word, stay text.
            assert_eq!(
                render("http is old, xhttps://a no", true, chunk),
                "http is old, xhttps://a no"
            );
            assert_eq!(
                render("end https://a.org", true, chunk),
                format!("end {}", hyper("https://a.org"))
            );
            assert_eq!(
                render("a\nhttps://a.org b", true, chunk),
                format!("a\n{} b", hyper("https://a.org"))
            );
            // A bare scheme is no link; in code the code color wins.
            assert_eq!(render("https:// x", true, chunk), "https:// x");
            assert_eq!(
                render("`https://a.org`", true, chunk),
                format!("{code}https://a.org{reset}")
            );
        }
        assert_eq!(
            hyper("https://a.org"),
            format!("{link}\x1b]8;;https://a.org\x1b\\https://a.org\x1b]8;;\x1b\\{reset}")
        );
        assert_eq!(render("see https://a.org.", false, 1), "see https://a.org.");
    }
}
