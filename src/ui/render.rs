//! Turns the streamed answer into what the user sees: the `dsc` action block is
//! hidden (the action line shows it), blank lines at the edges are dropped, and
//! with colors on a little markdown is styled (inline code, code blocks with a
//! bar instead of the fences, bold, headings).

use super::style::Style;
use crate::agent::protocol::FENCE;

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Plain,
    Code,
    Bold,
    Heading,
    Dim,
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
    action_line: String,
    in_code: bool,
    inline_code: bool,
    bold: bool,
    heading: bool,
    /// A `*` that may be the first of `**`
    star: bool,
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
            action_line: String::new(),
            in_code: false,
            inline_code: false,
            bold: false,
            heading: false,
            star: false,
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
        if !self.in_action && held.trim() != FENCE && !(fence && self.style.on()) {
            for c in held.trim_end().chars() {
                if fence {
                    self.put(c, Look::Dim, &mut out);
                } else {
                    self.visible(c, &mut out);
                }
            }
        }
        self.flush_star(&mut out);
        self.set_look(Look::Plain, &mut out);
        *self = Self::new(self.style);
        out
    }

    fn push(&mut self, c: char, out: &mut String) {
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
            if start.is_empty() || FENCE.starts_with(start) || start.starts_with("```") {
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
        self.flush_star(out);
        self.set_look(Look::Plain, out);
        self.inline_code = false;
        self.bold = false;
        self.heading = false;
        self.pending_newlines += 1;
        self.line_start = true;
    }

    /// A character of an ordinary line; markdown marks are consumed when colors are on.
    fn visible(&mut self, c: char, out: &mut String) {
        if self.style.on() && !self.in_code {
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
}
