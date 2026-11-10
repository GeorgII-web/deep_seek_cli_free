---
name: terminal-ui
description: The terminal UI in `src/ui/` — `Terminal` (banner, input, answer stream, action line, approval question, result preview, finish line with the seconds), the multiline input box and its keys in `editor.rs`, the stream renderer that hides the `dsc` block and styles markdown in `render.rs`, the progress line in `spinner.rs` and the colors in `style.rs`. Use when the look, a key, the progress line or the printed text of a run must change, or output is torn or doubled.
---

# Terminal UI

`ui::Terminal` implements `agent::run::Ui` (see `../agent-loop/SKILL.md`) and adds what only `src/main.rs` calls:
`open_chat`, `input`, `begin`, `finish`, `info`, `error`.

| File | Content |
|---|---|
| `src/ui/mod.rs` | `Terminal` (`line`, `partial`, `text` = the only print paths); `screen_lock`, `screen_write`; constants `RESULT_LINES` (8), `INPUT_HINT` |
| `src/ui/editor.rs` | `Buffer` (pure text + cursor, tested), `Editor` (the box inline or docked, history), `Dock`, `Input`, `read_choice`, `RawMode` |
| `src/ui/render.rs` | `Markdown`: `feed`, `finish` |
| `src/ui/spinner.rs` | `Spinner`: `start`, `stop`, `dock` |
| `src/ui/style.rs` | `Style`: every escape code; `detect()` |

## Layouts

| Layout | When | Behaviour |
|---|---|---|
| inline | a single task; the chat without a terminal | output follows the shell prompt; answer text on stdout, everything else on stderr; the spinner shares the output line and is removed before any output; the box (only for editing a command) is drawn in place |
| screen | the chat with stdin, stdout and stderr on a terminal (`Terminal::open_chat`) | screen cleared, banner at the top, the box docked at the bottom, output above it; everything on stdout; the spinner lives on the last row and keeps running while text streams ("Thinking", "Answering") |

Screen layout mechanics (`Editor::open_dock`, `set_area`, `draw_docked`, `dock_idle`, `close_dock`):

- The rows above the dock are a scroll region (`ESC[1;<bottom>r`). Output written there scrolls inside it and
  leaves through the top into the terminal's own scrollback; the dock rows are never touched by output.
- The dock = top border, 1 to `DOCK_MAX_ROWS` (10, at most a third of the screen) text rows, bottom border,
  status row. It is drawn with absolute positions (`ESC[<row>;1H`).
- `Dock::out` = where output continues. It is read from the terminal (`crossterm::cursor::position`) when the
  dock opens and at the start of every input; after the input the cursor is put back there.
- Growing the dock: when `out` would be covered, line feeds at the old region bottom push the output up first.
  Shrinking and resizing clear the freed rows; a new terminal height starts the layout over.
- The spinner writes the last row between save and restore cursor (`ESC7`, `ESC8`), under `screen_lock`.
- `Terminal::drop` → `close_dock`: scroll region reset, dock rows cleared, cursor after the output.

## Rules

- Print only through `Terminal::line` (chrome), `partial` (a question waiting for a key) and `text` (answer);
  they pick the stream of the layout and hold `screen_lock`, which the spinner thread takes too.
- Without a terminal (`Style::detect`, `Terminal::interactive`) there are no colors, no spinner, no box: `input`
  reads plain lines, a question counts as no. `NO_COLOR` or `TERM=dumb` switch colors off.
- Raw mode only while a key is awaited (`RawMode` guard in `Editor::read`, `read_choice`), so Ctrl-C is a real
  signal while the model or a command works.
- Before chrome is printed call `close_output` (ends an open answer line; inline it also removes the spinner). `blank()` prints
  one empty line unless `gap` says the last line was one.
- A color is never written as a literal escape outside `style.rs`.

## What a task looks like

```
> how much disk space is free on / ?        echo of the input (bold)

⠹ Waiting for DeepSeek… 3s                  spinner (inline: replaced by the output; screen: last row)

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

Inline: drawn in place (top border, the rows, bottom border, hint line); every key redraws, `move_to_top` goes
up `cursor_line` rows and clears down. Docked: see Layouts; both use `box_lines`. Wrapping: `Buffer::rows(width)` splits at line ends and
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

`Buffer`, `Markdown` have unit tests. The drawn screen: `scripts/pty-drive.py '<keys>' [rows] [columns]` runs the
chat in a pseudo-terminal with a small VT100 emulation (scroll region, absolute moves, cursor report) and prints
the scrollback, the screen with the region marked (`-` region row, `#` dock row) and the cursor. See
`../../quality-gate/build-run-check/SKILL.md`. Not covered by it: a real terminal's scrollback behaviour with a
scroll region, and a resize while a task runs (the layout is redone at the next input).
