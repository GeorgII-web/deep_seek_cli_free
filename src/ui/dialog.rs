//! A modal window for the login token: a masked field and the buttons OK and
//! Cancel. It is drawn on the terminal's alternate screen, so the chat comes
//! back untouched when it closes. The token goes only to the caller: it is
//! never printed, never put into the input history, never sent as a message.

use super::editor::RawMode;
use super::screen_write;
use super::style::Style;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use std::io;

/// Columns of the window, borders included, when the terminal is wide enough.
const WIDTH: usize = 66;
const SNIPPET: &str = "copy(JSON.parse(localStorage.getItem('userToken')).value)";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Focus {
    Ok,
    Cancel,
}

/// The rows of the window, each `width` columns wide. `token_len` = characters
/// typed so far; they are shown as dots only.
fn rows(style: Style, width: usize, reason: &str, token_len: usize, focus: Focus) -> Vec<String> {
    let s = style;
    let inner = width - 4;
    let (gray, reset) = (s.gray(), s.reset());
    // A text row: `text` is what counts for the width, `shown` is its colored form.
    let row = |text: &str, shown: String| {
        let pad = " ".repeat(inner.saturating_sub(text.chars().count()));
        format!("{gray}│{reset} {shown}{pad} {gray}│{reset}")
    };
    let plain = |text: &str| {
        let text: String = text.chars().take(inner).collect();
        row(&text, text.clone())
    };
    let title = " DeepSeek token ";
    let top = format!(
        "{gray}╭─{reset}{}{}{title}{reset}{gray}{}╮{reset}",
        s.bold(),
        s.accent(),
        "─".repeat(width - 3 - title.chars().count())
    );

    let field = inner - "Token: ".len();
    let dots = "•".repeat(token_len.min(field));
    let token_text = format!("Token: {dots}");
    let token_shown = match token_len {
        0 => format!(
            "{}Token:{} {}paste it here{}",
            s.bold(),
            reset,
            s.faint(),
            reset
        ),
        _ => format!("{}Token:{} {}{dots}{}", s.bold(), reset, s.ok(), reset),
    };
    let token_row = match token_len {
        0 => row("Token: paste it here", token_shown),
        _ => row(&token_text, token_shown),
    };
    let count = match token_len {
        0 => String::new(),
        n => format!("{n} characters, hidden"),
    };

    let button = |label: &str, focused: bool| match focused {
        // The focused button is drawn inverted.
        true => format!(
            "{}{}{}[ {label} ]{reset}",
            s.bold(),
            s.accent(),
            s.inverse()
        ),
        false => format!("{}[ {label} ]{reset}", s.key()),
    };
    let buttons_text = "[ OK ]      [ Cancel ]";
    let left = (inner - buttons_text.chars().count()) / 2;
    let buttons_shown = format!(
        "{}{}      {}",
        " ".repeat(left),
        button("OK", focus == Focus::Ok),
        button("Cancel", focus == Focus::Cancel)
    );
    let buttons_row = row(
        &format!("{}{buttons_text}", " ".repeat(left)),
        buttons_shown,
    );

    vec![
        top,
        plain(""),
        row(reason, format!("{}{reason}{reset}", s.action())),
        plain(""),
        plain("Open chat.deepseek.com, log in, and run in the DevTools"),
        plain("console:"),
        row(SNIPPET, format!("{}{SNIPPET}{reset}", s.code_text())),
        plain("Then paste the token here. It is saved on this machine only"),
        plain("and never shown or sent as a message."),
        plain(""),
        token_row,
        row(&count, format!("{}{count}{reset}", s.faint())),
        plain(""),
        buttons_row,
        plain(""),
        format!("{gray}╰{}╯{reset}", "─".repeat(width - 2)),
    ]
}

/// Opens the window and waits. `Some(token)` = OK with a token, `None` = Cancel.
pub fn ask_token(style: Style, reason: &str) -> io::Result<Option<String>> {
    let _raw = RawMode::enter()?;
    // Alternate screen: the chat below is not overwritten and returns as it was.
    screen_write("\x1b[?1049h");
    let result = run(style, reason);
    screen_write("\x1b[?1049l");
    result
}

fn run(style: Style, reason: &str) -> io::Result<Option<String>> {
    let s = style;
    let mut token = String::new();
    let mut focus = Focus::Ok;
    loop {
        let (columns, lines) = terminal::size()
            .map(|(c, l)| (c as usize, l as usize))
            .unwrap_or((80, 24));
        let width = WIDTH.min(columns.saturating_sub(2)).max(SNIPPET.len() + 4);
        let reason: String = reason.chars().take(width - 4).collect();
        let window = rows(s, width, &reason, token.chars().count(), focus);
        let top = lines.saturating_sub(window.len() + 2) / 2 + 1;
        let left = columns.saturating_sub(width) / 2 + 1;
        let mut frame = String::from("\x1b[2J");
        for (i, row) in window.iter().enumerate() {
            frame.push_str(&format!("\x1b[{};{left}H{row}", top + i));
        }
        let hint = [
            ("Paste", "the token"),
            ("Tab", "switch button"),
            ("Enter", "confirm"),
            ("Esc", "cancel"),
        ]
        .map(|(key, what)| s.hint(key, what))
        .join(&format!("{} · {}", s.faint(), s.reset()));
        frame.push_str(&format!("\x1b[{};{left}H  {hint}", top + window.len() + 1));
        // The cursor waits in the token field.
        let field = width - 4 - "Token: ".len();
        let column = left + 2 + "Token: ".len() + token.chars().count().min(field);
        frame.push_str(&format!("\x1b[{};{column}H", top + 10));
        screen_write(&frame);

        match event::read()? {
            Event::Paste(text) => token.extend(
                text.chars()
                    .filter(|c| !c.is_control() && !c.is_whitespace()),
            ),
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('c') | KeyCode::Char('d') if ctrl => return Ok(None),
                    KeyCode::Char('u') if ctrl => token.clear(),
                    KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                        focus = if focus == Focus::Ok {
                            Focus::Cancel
                        } else {
                            Focus::Ok
                        };
                    }
                    KeyCode::Enter if focus == Focus::Cancel => return Ok(None),
                    // OK without a token keeps the window open.
                    KeyCode::Enter if !token.is_empty() => return Ok(Some(token)),
                    KeyCode::Backspace => {
                        token.pop();
                    }
                    KeyCode::Char(c) if !ctrl && !c.is_whitespace() => token.push(c),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(token_len: usize, focus: Focus) -> Vec<String> {
        rows(
            Style::new(false),
            WIDTH,
            "The token is dead.",
            token_len,
            focus,
        )
    }

    #[test]
    fn every_row_has_the_window_width() {
        for token_len in [0, 5, 64, 500] {
            for row in plain(token_len, Focus::Ok) {
                assert_eq!(row.chars().count(), WIDTH, "{row:?}");
            }
        }
    }

    #[test]
    fn the_token_is_shown_as_dots_only() {
        let window = plain(5, Focus::Ok).join("\n");
        assert!(window.contains("Token: •••••"));
        assert!(window.contains("5 characters, hidden"));
        assert!(plain(0, Focus::Ok).join("\n").contains("paste it here"));
        assert!(window.contains("[ OK ]") && window.contains("[ Cancel ]"));
    }

    #[test]
    fn the_focused_button_is_inverted() {
        let s = Style::new(true);
        let ok = rows(s, WIDTH, "r", 1, Focus::Ok).join("\n");
        let cancel = rows(s, WIDTH, "r", 1, Focus::Cancel).join("\n");
        assert!(ok.contains("\x1b[7m[ OK ]") && !ok.contains("\x1b[7m[ Cancel ]"));
        assert!(cancel.contains("\x1b[7m[ Cancel ]") && !cancel.contains("\x1b[7m[ OK ]"));
    }
}
