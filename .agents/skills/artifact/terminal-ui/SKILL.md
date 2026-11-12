---
name: terminal-ui
description: The terminal UI in `src/ui/` — `Terminal` and its two layouts (inline, and the chat screen with the docked box), the print rules, what a task looks like (status line, action blocks, finish bar), the keys watched while a task runs, the mode and hint lines, `spinner.rs`, `render.rs`, `style.rs`, `banner.rs`. Use when the look of a run, the progress lines, colors or the printed text must change, or output is torn or doubled.
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
| `src/ui/clipboard.rs` | `read()`: the system clipboard through the desktop's tool (`wl-paste`, `xclip`, `xsel`, `powershell.exe Get-Clipboard` under WSL, `pbpaste`); used by `paste_text` |
| `src/ui/editor.rs` | `Buffer` (pure text + cursor, tested), `Editor` (the box inline or docked, history, command menu), `command_matches`, `Dock`, `Input`, `read_choice`, `RawMode` |
| `src/ui/scrollback.rs` | `View`: the output kept by the app (lines, wrapped rows, scroll offset, blocks, selection, progress mark), `wrap`, `columns_of`, `plain_of` |
| `src/ui/render.rs` | `Markdown`: `feed`, `finish` |
| `src/ui/spinner.rs` | `Spinner`: `start`, `stop`, `dock`, `set_hint`, `set_mode`; `Notifier` (a 2 s notice in place of the key hint) |
| `src/ui/style.rs` | `Style`: every escape code; `detect()`; `inline(text, base)` colors `code` spans and links in chrome lines; `links(text, base)` colors only the links (result preview); `link()`, `hyperlink(url)` (OSC 8 open and close); `hint(key, what)` and `mode_label` for hint lines |

Two parts have their own skill: the output kept and scrolled by the app, with blocks, selection and the status
line → `../scrollback/SKILL.md`; the input box, the command menu, copy and paste, and the windows (token, list) →
`../input-box/SKILL.md`.

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
- Tab title: `open_chat` saves the terminal's title (`ESC[22;0t`), `write_title` writes `ESC]0;<icon> <title>BEL` and the same as `ESC]2;…BEL`,
  `drop` restores it (`ESC[23;0t`). `set_title` sets the text (`dsc`, or `dsc · <conversation>` from the first
  task on); `set_tab_state` sets the icon: `⏳` from `begin` to `finish` (`TabState::Working`), `❓` during an
  approval question (`Asking`), `✻` otherwise (`Idle`), so the state is visible from another tab. While prompts
  wait in the queue, `+N` follows the icon (`⏳ +2 dsc · <conversation>`; nothing for an empty queue): `tab_text`,
  written by `write_tab` from the shared `Tab` (icon and title of the last `write_title`), by `write_title` and by
  the key watcher when a prompt is queued or dropped; `input` writes it again when it takes a queued prompt.
- `Terminal::drop` → `close_dock` (scroll region reset), mouse reports off, back to the normal screen, then
  `View::transcript` is printed once so the conversation stays in the terminal.

## Rules

- Print only through `Terminal::line` (chrome), `partial` (a question waiting for a key) and `text` (answer);
  they pick the stream of the layout and hold `screen_lock`, which the spinner thread takes too.
- Without a terminal (`Style::detect`, `Terminal::interactive`) there are no colors, no spinner, no box: `input`
  reads plain lines, a question counts as no. `NO_COLOR` or `TERM=dumb` switch colors off.
- Raw mode while a key is awaited: `Editor::read`, `read_choice`, and for the whole task (`Watch`, section "Keys while a task runs"). In raw mode a line must end with `\r\n`: `Terminal::eol` does that inside `line`, `partial`, `text`.
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
| output of an action | `running`, `output`, `result` | a block: header `⎿ ⠧ running… 2s` (the progress thread calls `View::animate` with its frame every tick, so the step in progress is marked in the output itself; the seconds count from the block's start), the body lines as they arrive (`output` → `clean_output` → `body_line`; a result without live output, like a file read, gives its lines in `result`), then the header becomes `⎿ ✓ <outcome> · N lines ▸ show`, or red `⎿ ✗ <outcome> …` when `outcome_failed` (an `Error…` of the tool, an exit code other than 0, a timeout, a kill), and the body folds when it has more than `FOLD_OPEN_LINES` (5) lines, else `▾ hide` and open; `result_is_answer` (the model's `done`) unfolds it; a text starting with `Denied` is not repeated. Inline there is no folding: the lines, then `⎿ <outcome> · N lines` |
| prompt | `echo` | the submitted input on a bar with its own background (`Style::prompt_bg`, filled to the row end with `ESC[K`), one empty row of the bar above and below the text; attached files one row each on the bar after an empty bar row (`📎` / `⚠ name · size`, `../attachments/SKILL.md`); plain `> text` without colors |
| finish | `finish` | a bar with a background filled to the row end (`ESC[K`), like the prompt: dark green `✓ 6.4s · 2 answers · 1 action · 4 requests` (`Style::done_bg`; the last number is the prompts sent to DeepSeek for this task, one per answer of the model, given to `finish` by the caller), dark red `✗ <error> · …` (`failed_bg`) after a failure or an interrupt; the text is cut to one row; without colors just the text |
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
| other keys, a paste, the mouse | chat screen: `Editor::busy_event` (the box takes them: typing, dropped files, Enter queues the prompt, Alt+N / Ctrl+N drops queued prompt N, the wheel and selection as in the box; a full queue shows `QUEUE_FULL` on the progress line). Inline (single task): swallowed, the mouse goes to the output |

`watch_start` sets `Editor::set_busy(true)` and gives the thread the shared `Arc<Mutex<Editor>>` (chat screen only);
`watch_stop` sets it back. The prompt queue itself is in `../input-box/SKILL.md` ("Prompt queue").

The terminal cursor is hidden while the watcher runs (`ESC[?25l` in `watch_start`, `ESC[?25h` in `watch_stop`):
the progress marks show where the work is, a blinking cursor next to them only distracts. The box draws a cursor
of its own (inverse) meanwhile. The real one is back for the input box, an approval question, a window and at exit.

## Mode and hint lines

`Terminal::allow_all` (`Arc<AtomicBool>`, false = ask) is shared with the box, the progress thread, the key
watcher and the agent loop (`Terminal::mode`). With a terminal it is shown as `Style::mode_label`: `⏵ ask` in green
(`Style::ok`), `⏵⏵ allow` in the action color (orange).

| Line | Content |
|---|---|
| under the box (`Editor::hint_line`) | left: the keys, as many as fit: `INPUT_HINTS` = `Ctrl + C twice quit` and `↑↓ history` only (every other key is in `/help`), or `EDIT_HINTS`, `MENU_HINTS`; the row starts with `HINT_INDENT` (one column) and its labels end `LABELS_MARGIN` (1) column before the box edge, so it stands equally far inside both edges of the box (`hint_columns`); right: model label (`Style::model_label`: `◆ DeepSeek` blue, `◆ Mock` cyan), `✦ Think` (`Style::think_label`: pink while deep thinking is on, gray while off; the flag `Terminal::think`, set by `set_think`), `◉ Browse` (`Style::browse_label`: pink while DeepSeek's web search is on, gray while off; the flag `Terminal::browse`, set by `set_browse`), then the mode label with the key that switches it right after (`MODE_KEY`, `Shift + Tab`); built by `Editor::right_labels`. A left click on `✦ Think`, `◉ Browse` or the mode label (`Editor::label_at`, chat box only) runs `/think`, `/browse` or `/permissions` as if typed (`Step::Toggle` → `Input::Submit(command)`; the text and the files stay in the box) |
| progress line (`Spinner`) | screen layout: the row under the box keeps its shape. Left, where the keys are when idle: spinner and a short word from `WORDS` (`busy_word`: a random start per piece of work, the next word every `WORD_SECS` = 6 s), then `Esc interrupt` or a 2 s notice in its place. Right, as when idle, ending `LABELS_MARGIN` column before the box edge: model label, `✦ Think`, `◉ Browse`, mode label, `Shift + Tab`. The labels never move (a click on one is found by `Editor::label_at` at the place it has when idle): the word, the notice and the hint on the left are cut to what is left (`after_for`). A click on the mode label switches it at once; on Think or Browse it shows `SWITCH_LATER` ("Not while a task runs"), since the conversation is busy. The step and its seconds are not repeated here: they are in the output (the status line, the header of a running action). Inline it is the only progress line: spinner, the real label and clock, the hint, the labels |

The model flag `Terminal::mock` (`Arc<AtomicBool>`, set by `Terminal::set_model` from `chat()` at the start and on
`/model`) is shared with the box and the progress thread the same way as the mode.

Two grays: `Style::key` (lighter) for the key, `Style::faint` (darker) for what it does and the ` · ` separators.

## Checking a change

`Buffer`, `Markdown` have unit tests. The drawn screen: `scripts/pty-drive.py '<keys>' [rows] [columns]` runs the
chat in a pseudo-terminal with a small VT100 emulation (scroll region, absolute moves, cursor report) and prints
the scrollback, the screen with the region marked (`-` region row, `#` dock row) and the cursor. See
`../../quality-gate/build-run-check/SKILL.md`. Not covered by it: a real terminal's scrollback behaviour with a
scroll region, and a resize while a task runs (the layout is redone at the next input).
