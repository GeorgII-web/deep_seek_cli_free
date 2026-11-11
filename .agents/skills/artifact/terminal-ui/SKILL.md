---
name: terminal-ui
description: The terminal UI in `src/ui/` — `Terminal` (banner, input, answer stream, action line, approval question, result preview, finish line with the seconds), the multiline input box and its keys in `editor.rs`, the stream renderer that hides the `dsc` block and styles markdown in `render.rs`, the progress line in `spinner.rs` and the colors in `style.rs`. Use when the look, a key, the progress line or the printed text of a run must change, or output is torn or doubled.
---

# Terminal UI

`ui::Terminal` implements `agent::run::Ui` (see `../agent-loop/SKILL.md`) and adds what only `src/main.rs` calls:
`open_chat`, `input`, `begin`, `finish`, `info`, `error`, `help`, `token_dialog`, `pick`, `interrupts`, `mode`,
`toggle_mode`.

| File | Content |
|---|---|
| `src/ui/mod.rs` | `Terminal` (`line`, `partial`, `text` = the only print paths); `screen_lock`, `screen_write`; constants `FOLD_OPEN_LINES` (5), `INPUT_HINTS`, `EDIT_HINTS`, `COMMANDS` |
| `src/ui/banner.rs` | `art(style, columns)`: the start picture; `glyph` holds the 5-row letters (`#` = filled cell) |
| `src/ui/dialog.rs` | `ask_token(style, reason)`: the token window; `rows` builds it (tested for width and masking) |
| `src/ui/editor.rs` | `Buffer` (pure text + cursor, tested), `Editor` (the box inline or docked, history, command menu), `command_matches`, `Dock`, `Input`, `read_choice`, `RawMode` |
| `src/ui/scrollback.rs` | `View`: the output kept by the app (lines, wrapped rows, scroll offset), `wrap`, `columns_of` |
| `src/ui/render.rs` | `Markdown`: `feed`, `finish` |
| `src/ui/spinner.rs` | `Spinner`: `start`, `stop`, `dock`, `set_hint`, `set_mode`; `Notifier` (a 2 s notice in place of the key hint) |
| `src/ui/style.rs` | `Style`: every escape code; `detect()`; `inline(text, base)` colors `code` spans and links in chrome lines; `links(text, base)` colors only the links (result preview); `link()`, `hyperlink(url)` (OSC 8 open and close); `hint(key, what)` and `mode_label` for hint lines |

## Layouts

| Layout | When | Behaviour |
|---|---|---|
| inline | a single task; the chat without a terminal | output follows the shell prompt; answer text on stdout, everything else on stderr; the spinner shares the output line and is removed before any output; the box (only for editing a command) is drawn in place |
| screen | the chat with stdin, stdout and stderr on a terminal (`Terminal::open_chat`) | the alternate screen (no terminal history), banner at the top, the box docked at the bottom, output above it kept and scrolled by the app (`View`); everything on stdout; the spinner lives on the last row and keeps running while text streams ("Thinking", "Answering") |

Screen layout mechanics (`Editor::open_dock`, `set_area`, `draw_docked`, `dock_idle`, `close_dock`):

- `open_chat` prints, after the clear: the picture "DEEP SEEK CLI FREE" (`banner::art`: one band from 78
  columns, two bands from 42, none below; blue, gray and green shades per row, plain blocks without colors; only
  in the screen layout), then version and model, the working directory and the key hint line.
- The rows above the dock are a scroll region (`ESC[1;<bottom>r`). Output written there scrolls inside it and
  leaves through the top into the terminal's own scrollback; the dock rows are never touched by output.
- The dock = top border, 1 to `DOCK_MAX_ROWS` (10, at most a third of the screen) text rows, bottom border,
  status row. It is drawn with absolute positions (`ESC[<row>;1H`).
- `Dock::out` = where output continues. It is read from the terminal (`crossterm::cursor::position`) when the
  dock opens and at the start of every input; after the input the cursor is put back there.
- Growing the dock: when `out` would be covered, line feeds at the old region bottom push the output up first.
  Shrinking and resizing clear the freed rows; a new terminal height starts the layout over.
- The spinner writes the last row between save and restore cursor (`ESC7`, `ESC8`), under `screen_lock`.
- Tab title: `open_chat` saves the terminal's title (`ESC[22;0t`), `set_title` writes `ESC]0;<text>BEL`, `drop`
  restores it (`ESC[23;0t`).
- `Terminal::drop` → `close_dock` (scroll region reset), mouse reports off, back to the normal screen, then
  `View::transcript` is printed once so the conversation stays in the terminal.

## Scrollback (`scrollback::View`, screen layout only)

The app keeps its own output: complete lines plus one open line, wrapped to the screen width (`wrap` knows
escape sequences and carries colors over a break). The view is the last rows of that; `offset` = rows below it.

| Case | What happens |
|---|---|
| at the bottom | `Terminal::emit` records the text (`View::push`) and writes it to the terminal, which wraps and scrolls inside the scroll region exactly like the model |
| scrolled up | `push` only records; the view stays on the same rows (`offset` grows), nothing is written |
| a block (`begin_fold`, `end_fold`) | the lines pushed between them are the body under the header line; `end_fold(folded_text, open_text, folded)` sets the two header texts and hides the body when folded; `rewrap` skips hidden bodies, `wrapped` keeps the line each row comes from |
| the running step | `Spinner` thread → `View::animate(frame)`: rewrites the header of the block being written and draws only that row (between save and restore cursor); nothing happens without a block being written. The thread releases its own state lock before it takes the view's |
| click, Ctrl-O | `mouse_event` → `View::click(row)`: a left click on the header row of a finished block with a body toggles it (`toggle` keeps the rows above in place); Ctrl-O → `toggle_last`; `unfold_last` keeps the view at the bottom |
| scroll (PgUp, PgDn, wheel 3 rows, Ctrl+Home, Ctrl+End) | `View::scroll` / `top` / `bottom` → `repaint`: every row of the area cleared and drawn from the model, cursor at `out()` |
| dock grows or shrinks, terminal resized | `Editor::sync_view` → `View::resize` (rewrap on a new width) → `repaint`; `Dock::out` comes from `View::out`, no cursor query |
| input sent | `Editor::erase` → `View::bottom` first |
| another conversation (`/sessions`, `/new`, `/model`) | `Terminal::show_output(text)` → `View::replace`: the saved output of that conversation, or a clean area; `Terminal::output` gives the text to save |
| a window (`token_dialog`, `pick`) | drawn over the screen (no second alternate screen), then `Terminal::redraw`: clear, `Editor::redraw_dock`, `repaint` |

- Everything printed in the screen layout must go through `emit` (`line`, `partial`, `text`) or `erase_partial`
  (removes the open line: the approval question). A direct write would put the model and the screen out of step.
- The scroll mark (`View::indicator`) sits on the dock's top border, right side; the box redraws it after itself.
- Keys reach the view from two places: `Editor::read` (while typing) and the key watcher (while a task runs).
- Mouse reports (`ESC[?1000h`, `ESC[?1006h`) are on when `ui.mouse_scroll` is true: the terminal then gives
  clicks to the app, so selecting text needs Shift.
- `KEEP_LINES` (20000) lines are kept; `forget` drops the oldest with the blocks that start in them.
- `transcript` (exit, saved sessions) writes every block out in full; a restored session has plain lines, no folds.

## Rules

- Print only through `Terminal::line` (chrome), `partial` (a question waiting for a key) and `text` (answer);
  they pick the stream of the layout and hold `screen_lock`, which the spinner thread takes too.
- Without a terminal (`Style::detect`, `Terminal::interactive`) there are no colors, no spinner, no box: `input`
  reads plain lines, a question counts as no. `NO_COLOR` or `TERM=dumb` switch colors off.
- Raw mode while a key is awaited: `Editor::read`, `read_choice`, and for the whole task (`Watch`, see Keys while a
  task runs). In raw mode a line must end with `\r\n`: `Terminal::eol` does that inside `line`, `partial`, `text`.
- Before chrome is printed call `close_output` (ends an open answer line; inline it also removes the spinner). `blank()` prints
  one empty line unless `gap` says the last line was one.
- A color is never written as a literal escape outside `style.rs`.

## What a task looks like

```
 > how much disk space is free on / ?       echo of the input (bold, on a background bar)

⠹ Waiting for DeepSeek… 3s                  spinner (inline: replaced by the output; screen: last row)

● Run df -h /                               Ui::action
  check free disk space                     its `why`
  Allow? y yes · a always in this session · n no · e edit      only when policy says ask; one key
  ⎿ exit code 0 · 2 lines  ▾ hide           the action's block: header, then its output (folded when long)
    Filesystem  Size ...

There is 810G free.                         the answer, markdown styled
 ✓ 8.1s · 2 answers · 1 action               Terminal::finish (on a background bar); ✗ <error> on a failure
```

| Piece | Code | Detail |
|---|---|---|
| progress | `Event::Status` → `Spinner::start` | labels come from `deepseek::session` ("Pausing N s (throttle)", "Opening conversation", "Solving proof of work", "Waiting for DeepSeek") and `Ui::running` ("Running"); a status is ignored while answer text is open; own thread, 90 ms tick, clock runs from the first label after a stop |
| reasoning | `Event::Thinking` | printed dim, a blank line before the answer |
| answer | `Markdown` | the `dsc` block is never shown; blank lines at the edges dropped, at most one inside; with colors: inline code colored (backticks removed), code blocks colored with a gray bar `  │ ` in front of each line and the fence lines dropped, `**bold**`, heading lines, links (`http://`, `https://` at a word start, up to whitespace or a closing mark, punctuation at the end left out; not inside code): the URL is collected, then printed blue inside an OSC 8 hyperlink (`Style::hyperlink`); the terminal underlines it under the mouse and opens it on click, which is why `Style::link` has no underline of its own (it would hide the hover effect; an app cannot see the mouse or change its pointer without taking over mouse selection); a line start is held back until it cannot be a fence |
| question | `approve` | `read_choice`; Esc, Ctrl-C, Ctrl-D = no; `e` opens the box with the command (`remember = false`); the question line is erased after the key |
| output of an action | `running`, `output`, `result` | a block: header `⎿ ⠧ running… 2s` (the progress thread calls `View::animate` with its frame every tick, so the step in progress is marked in the output itself; the seconds count from the block's start), the body lines as they arrive (`output` → `clean_output` → `body_line`; a result without live output, like a file read, gives its lines in `result`), then the header becomes `⎿ <outcome> · N lines ▸ show` and the body folds when it has more than `FOLD_OPEN_LINES` (5) lines, else `▾ hide` and open; `result_is_answer` (the model's `done`) unfolds it; a text starting with `Denied` is not repeated. Inline there is no folding: the lines, then `⎿ <outcome> · N lines` |
| prompt | `echo` | the submitted input on a bar with its own background (`Style::prompt_bg`, filled to the row end with `ESC[K`); plain `> text` without colors |
| finish | `finish` | a bar with a background filled to the row end (`ESC[K`), like the prompt: dark green `✓ 6.4s · 2 answers · 1 action` (`Style::done_bg`), dark red `✗ <error> · …` (`failed_bg`) after a failure or an interrupt; the text is cut to one row; without colors just the text |
| spacing | `blank()` | one empty line between the echo, each action block, the answer, the finish line and the next input |
| chrome text | `info`, `error`, `note`, the `why` line, the banner | `code` spans colored through `Style::inline`; `help` prints commands and keys in columns |

## Keys while a task runs (`Watch`)

`Terminal::interrupts` creates the channel of `agent::interrupt::Interrupts` (without a terminal it returns the
signal variant and nothing below applies). `begin` → `watch_start`: raw mode on, a thread polls
`crossterm::event` every 50 ms. `finish`, `approve` (the question reads keys itself; it restarts the watcher
after) and `drop` → `watch_stop`: thread joined, raw mode off.

| Key | Effect |
|---|---|
| Esc | `Interrupt::Stop`: the loop ends the task, a running command is killed |
| Ctrl-C | first: "Press Ctrl-C again to exit" on the progress line (`Notifier::show`; a line of its own when no progress line is drawn); second within `AGAIN`: `Interrupt::Quit` |
| Shift+Tab | switches the mode flag; the progress line shows it |
| other keys | swallowed (no type-ahead) |

## Mode and hint lines

`Terminal::allow_all` (`Arc<AtomicBool>`, false = ask) is shared with the box, the progress thread, the key
watcher and the agent loop (`Terminal::mode`). With a terminal it is shown as `Style::mode_label`: `⏵ ask` in green
(`Style::ok`), `⏵⏵ allow all` in the action color (orange).

| Line | Content |
|---|---|
| under the box (`Editor::hint_line`) | left: mode label, model label (`Style::model_label`: `◆ DeepSeek` blue, `◆ Mock` cyan) and the hint of `MODE_KEY` (Shift+Tab); right, ending at the box edge: the other `(key, meaning)` pairs of `INPUT_HINTS` (or `EDIT_HINTS`, `MENU_HINTS`) that fit, most important first |
| progress line (`Spinner`) | spinner, label, clock, then the hint `Esc to stop` or a 2 s notice, then the mode label and the model label |

The model flag `Terminal::mock` (`Arc<AtomicBool>`, set by `Terminal::set_model` from `chat()` at the start and on
`/model`) is shared with the box and the progress thread the same way as the mode.

Two grays: `Style::key` (lighter) for the key, `Style::faint` (darker) for what it does and the ` · ` separators.

## Token window (`dialog::ask_token`)

A modal window centered on the terminal's alternate screen (`ESC[?1049h` … `l`), so the chat is not overwritten
and comes back as it was; `Terminal::token_dialog` then calls `Editor::refresh_dock` to set the scroll region
again. Content (`dialog::rows`, which also returns the row of the token field): title, the reason, five numbered steps
to get the token (open the site, open the console with F12 or Ctrl+Shift+J, type `allow pasting`, run the
`SNIPPET`, paste the token; keys and things to type colored through `Style::inline`), the field, the buttons
`[ OK ]` and `[ Cancel ]` (the focused one inverted), a hint line below. The README section "Log in" has the same steps.

| Key | Effect |
|---|---|
| paste, characters | added to the token; shown as `•` only, with the count; whitespace and control characters dropped |
| Backspace, Ctrl-U | delete one, delete all |
| Tab, Shift+Tab, Left, Right | switch the focused button |
| Enter | the focused button; OK without a token does nothing |
| Esc, Ctrl-C, Ctrl-D | Cancel |

The token leaves the function only as the return value: it is never printed, logged or put into `Editor` history.

`dialog::pick(style, title, items)` is the list window of `/sessions`, drawn the same way: one row per item (cut
to the window), the selected one inverted with `❯`, "N more below" in the border when the list is longer than
the screen. Up, Down, PgUp, PgDn, Home, End move; Enter returns the index; Esc, `q`, Ctrl-C close.

## Input box (`Editor::read`)

Inline: drawn in place (top border, the rows, bottom border, hint line); every key redraws, `move_to_top` goes
up `cursor_line` rows and clears down. Docked: see Layouts; both use `box_lines`. Wrapping: `Buffer::rows(width)` splits at line ends and
at the box width (wide characters count two columns); a box taller than the screen shows the rows around the cursor.

| Key | Effect |
|---|---|
| Enter | send (empty input is ignored); after a `\` at the cursor: new line |
| Shift+Enter (where the terminal reports it: `RawMode` asks for the keyboard flags), Alt+Enter, Ctrl-J | new line |
| PgUp, PgDn, Ctrl+Home, Ctrl+End, mouse wheel | scroll the output (`scroll_by_key`, `mouse_event`) |
| Ctrl-O, click on a block header | fold or unfold the output of an action |
| Up, Down | move a row; at the first or last row go through the history (the draft is kept) |
| Left, Right, Home, End, Ctrl-A, Ctrl-E | move |
| Backspace, Delete, Ctrl-W or Alt+Backspace (word), Ctrl-U, Ctrl-K | delete |
| `/` as the first character | command menu, see below |
| Shift+Tab | switch the mode (ask ⇄ allow all) |
| Esc | clear the box |
| Ctrl-C | clear the box and show "Press Ctrl-C again to exit" for `AGAIN` (2 s); a second one in that time quits |
| Ctrl-D | quit on an empty box, else delete |
| paste | bracketed paste keeps line ends; tabs become spaces, control characters are dropped |

Command menu (chat prompt only, `remember = true`): while the input is one word starting with `/`,
`command_matches(COMMANDS, text)` gives the commands that start with it and `menu_lines` draws them between the box
and the hint line (the dock grows by that many rows). A command typed out in full offers nothing, so Enter sends it.

| Key with the menu open | Effect |
|---|---|
| Up, Down | move the selection (wraps) |
| Enter | run the selected command at once (`Input::Submit`) |
| Tab | complete: put the selected command into the box |
| Esc | close the menu for this text; the text stays |
| any edit | new matches, selection back to the first |

`COMMANDS` lives in `src/ui/mod.rs` (`Editor::set_commands`); `chat()` in `src/main.rs` runs them. A submitted
command is not echoed into the output (`Terminal::input`): only its effect is shown.

History: `<home>/history.json`, the last 200 inputs, consecutive duplicates skipped; only the chat passes a file.

## Checking a change

`Buffer`, `Markdown` have unit tests. The drawn screen: `scripts/pty-drive.py '<keys>' [rows] [columns]` runs the
chat in a pseudo-terminal with a small VT100 emulation (scroll region, absolute moves, cursor report) and prints
the scrollback, the screen with the region marked (`-` region row, `#` dock row) and the cursor. See
`../../quality-gate/build-run-check/SKILL.md`. Not covered by it: a real terminal's scrollback behaviour with a
scroll region, and a resize while a task runs (the layout is redone at the next input).
