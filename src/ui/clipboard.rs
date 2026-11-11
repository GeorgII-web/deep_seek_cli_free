//! Reading the system clipboard, for a paste that the terminal does not do
//! itself (a right click while the app handles the mouse). A terminal program
//! cannot ask the terminal for the clipboard, so the tool of the desktop is run.

use std::process::{Command, Stdio};

/// The tools that print the clipboard, most specific first: Wayland, X11,
/// Windows from WSL, macOS. One that is not installed is skipped.
fn readers() -> Vec<(&'static str, Vec<&'static str>)> {
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let mut readers = Vec::new();
    if set("WAYLAND_DISPLAY") {
        readers.push(("wl-paste", vec!["--no-newline"]));
    }
    if set("DISPLAY") {
        readers.push(("xclip", vec!["-selection", "clipboard", "-o"]));
        readers.push(("xsel", vec!["--clipboard", "--output"]));
    }
    if set("WSL_DISTRO_NAME") {
        readers.push((
            "powershell.exe",
            vec![
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Get-Clipboard -Raw",
            ],
        ));
    }
    readers.push(("pbpaste", Vec::new()));
    readers
}

/// What a clipboard tool printed, as text to insert: Windows line ends become
/// plain ones, the line end the tool adds at the very end is dropped.
fn clean(output: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(output).replace("\r\n", "\n");
    let text = text.strip_suffix('\n').unwrap_or(&text);
    (!text.is_empty()).then(|| text.to_string())
}

/// The text on the system clipboard; `None` when no tool gives one.
pub fn read() -> Option<String> {
    // Never in the unit tests: they must not start desktop tools.
    if cfg!(test) {
        return None;
    }
    readers().into_iter().find_map(|(program, args)| {
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| clean(&output.stdout))
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_output_becomes_text_to_insert() {
        assert_eq!(clean(b"one\r\ntwo\r\n").as_deref(), Some("one\ntwo"));
        assert_eq!(clean(b"token\n").as_deref(), Some("token"));
        assert_eq!(
            clean(b"keep  inner\n\nlines").as_deref(),
            Some("keep  inner\n\nlines")
        );
        assert_eq!(clean(b""), None);
        assert_eq!(clean(b"\n"), None);
        assert_eq!(read(), None);
    }

    #[test]
    fn there_is_always_a_reader_to_try() {
        assert!(readers().iter().any(|(program, _)| *program == "pbpaste"));
    }
}
