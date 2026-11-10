---
name: terminal-ui
description: The terminal UI in `src/ui/` — `Terminal` (banner, input, answer stream, action line, approval question, result preview, finish line with the seconds), the multiline input box and its keys in `editor.rs`, the stream renderer that hides the `dsc` block and styles markdown in `render.rs`, the progress line in `spinner.rs` and the colors in `style.rs`. Use when the look, a key, the progress line or the printed text of a run must change, or output is torn or doubled.
---

# Terminal UI

`ui::Terminal` implements `agent::run::Ui` (see `../agent-loop/SKILL.md`) and adds what only `src/main.rs` calls:
`banner`, `input`, `begin`, `finish`, `info`, `error`.

| File | Content |
|---|---|
| `src/ui/mod.rs` | `Terminal`; constants `RESULT_LINES` (8), `INPUT_HINT` |
| `src/ui/editor.rs` | `Buffer` (pure text + cursor, tested), `Editor` (the box, history), `Input`, `read_choice`, `RawMode` |
| `src/ui/render.rs` | `Markdown`: `feed`, `finish` |
| `src/ui/spinner.rs` | `Spinner`: `start`, `stop` |
| `src/ui/style.rs` | `Style`: every escape code; `detect()` |

## Rules

- Answer text goes to stdout, everything else (box, spinner, action lines, finish line) to stderr.
- Without a terminal (`Style::detect`, `Terminal::interactive`) there are no colors, no spinner, no box: `input`
  reads plain lines, a question counts as no. `NO_COLOR` or `TERM=dumb` switch colors off.
- Raw mode only while a key is awaited (`RawMode` guard in `Editor::read`, `read_choice`), so Ctrl-C is a real
  signal while the model or a command works.
- Before chrome is printed call `close_output` (stops the spinner, ends an open answer line). `blank()` prints
  one empty line unless `gap` says the last line was one.
- A color is never written as a literal escape outside `style.rs`.

## What a task looks like

```
> how much disk space is free on / ?        echo of the input (bold)

⠹ Waiting for DeepSeek… 3s                  spinner, replaced by the output

● Run df -h /                               Ui::action
  check free disk space                     its `why`
  Allow? y yes · a always in this session · n no · e edit      only when policy says ask; one key
  ⎿ exit code 0                             Ui::result, at most RESULT_LINES lines
    Filesystem  Size ...

There is 810G free.                         the answer, markdown styled
✓ 8.1s · 2 answers · 1 action               Terminal::finish; ✗ <error> on a failure
```

| Piece | Code | Detail |
|---|---|---|
| progress | `Event::Status` → `Spinner::start` | labels come from `deepseek::session` ("Pausing N s (throttle)", "Opening conversation", "Solving proof of work", "Waiting for DeepSeek") and `Ui::running` ("Running"); a status is ignored while answer text is open; own thread, 90 ms tick, clock runs from the first label after a stop |
| reasoning | `Event::Thinking` | printed dim, a blank line before the answer |
| answer | `Markdown` | the `dsc` block is never shown; blank lines at the edges dropped, at most one inside; with colors: inline code and code blocks colored (backticks removed inline), `**bold**`, heading lines; a line start is held back until it cannot be a fence |
| question | `approve` | `read_choice`; Esc, Ctrl-C, Ctrl-D = no; `e` opens the box with the command (`remember = false`); the question line is erased after the key |
| result | `result` | `Result: ` prefix and `--- stdout ---` dropped, `--- stderr ---` → `stderr:`; a text starting with `Denied` is not repeated |
| finish | `finish` | seconds since `begin`, answers of the model, actions that ran |

## Input box (`Editor::read`)

Drawn below the scrollback (no full screen): top border, the rows, bottom border, hint line. Every key redraws:
`move_to_top` goes up `cursor_line` rows and clears down. Wrapping: `Buffer::rows(width)` splits at line ends and
at the box width (wide characters count two columns); a box taller than the screen shows the rows around the cursor.

| Key | Effect |
|---|---|
| Enter | send (empty input is ignored); after a `\` at the cursor: new line |
| Alt+Enter, Shift+Enter (where the terminal reports it), Ctrl-J | new line |
| Up, Down | move a row; at the first or last row go through the history (the draft is kept) |
| Left, Right, Home, End, Ctrl-A, Ctrl-E | move |
| Backspace, Delete, Ctrl-W or Alt+Backspace (word), Ctrl-U, Ctrl-K | delete |
| Ctrl-C | clear the box; on an empty box quit |
| Ctrl-D | quit on an empty box, else delete |
| paste | bracketed paste keeps line ends; tabs become spaces, control characters are dropped |

History: `<home>/history.json`, the last 200 inputs, consecutive duplicates skipped; only the chat passes a file.

## Checking a change

`Buffer`, `Markdown` have unit tests. The drawn screen: `scripts/pty-drive.py` (pseudo-terminal, scripted keys,
prints the resulting screen), see `../../quality-gate/build-run-check/SKILL.md`.
