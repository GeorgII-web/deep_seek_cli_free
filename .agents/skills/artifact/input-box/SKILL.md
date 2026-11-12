---
name: input-box
description: Input in the terminal UI — the multiline box of `src/ui/editor.rs` (keys, history, command menu, inline and docked drawing), copy and paste (`clipboard.rs`, right click, Ctrl-C, Ctrl-V), and the windows of `src/ui/dialog.rs` (the token window, the list window, wrapped hints). Use when a key, the command menu, paste, the token window or the sessions and model lists must change or misbehave.
---

# Input box and windows

`Editor::read` is the box (called by `Terminal::input` and for editing a command); `dialog::ask_token` and
`dialog::pick` are the windows (called by `Terminal::token_dialog` and `Terminal::pick`). The layout around
them: `../terminal-ui/SKILL.md`; the output above the box: `../scrollback/SKILL.md`.

## Input box (`Editor::read`)

Inline: drawn in place (top border, the rows, bottom border, hint line); every key redraws, `move_to_top` goes
up `cursor_line` rows and clears down. Docked: at the bottom of the screen with the output above it (Layouts in `../terminal-ui/SKILL.md`); both use `box_lines`. Wrapping: `Buffer::rows(width)` splits at line ends and
at the box width (wide characters count two columns); a box taller than the screen shows the rows around the cursor.

| Key | Effect |
|---|---|
| Enter | send (empty input is ignored unless files are attached); after a `\` at the cursor: new line |
| Shift+Enter (where the terminal reports it: `RawMode` asks for the keyboard flags), Alt+Enter, Ctrl-J | new line |
| PgUp, PgDn, Ctrl+Home, Ctrl+End, mouse wheel | scroll the output (`scroll_by_key`, `mouse_event`) |
| Ctrl-O, click on a block header | fold or unfold the output of an action |
| Up, Down | move a row; at the first or last row go through the history (the draft is kept) |
| Left, Right, Home, End, Ctrl-A, Ctrl-E | move |
| Backspace, Delete, Ctrl-W or Alt+Backspace (word), Ctrl-U, Ctrl-K | delete; Backspace in an empty chat box drops the last attached file |
| `/` as the first character | command menu, described after this table |
| Shift+Tab | switch the mode (ask ⇄ allow) |
| click on `✦ Think`, `◉ Browse`, the mode label (bottom row) | the same as `/think`, `/browse`, `/permissions`; the text and files stay in the box. While a task runs: the mode switches at once, Think and Browse show "Not while a task runs" (`Busy::Later`) |
| Esc | clear the box (text and attached files) |
| Ctrl-C | with text selected in the output: copy it; else clear the box and show "Press Ctrl-C again to exit" for `AGAIN` (2 s); a second one in that time quits |
| Ctrl-V, right click with nothing selected | paste the copied text into the box |
| Ctrl-D | quit on an empty box, else delete |
| paste | bracketed paste keeps line ends; tabs become spaces, control characters are dropped. In the chat box a paste of existing absolute file paths (a drop) attaches the files instead: `../attachments/SKILL.md` |

Command menu (chat prompt only, `remember = true`): while the input is one word starting with `/`,
`command_matches(COMMANDS, text)` gives the commands that start with it and `menu_lines` draws them between the box
and the hint line (the dock grows by that many rows). Every row has a background over the width of the box
(`Style::menu_bg`, the selected one `menu_selected_bg`), so the list is visible even as a single row. A command typed out in full offers nothing, so Enter sends it.

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

## Keys of the box, two callers (`Editor::handle`)

`handle(typing, event, items, remember, width) -> Step` (`Continue`, `Submit`, `Exit`) is the whole key, paste and
mouse logic; `parts` gives the menu items, menu rows and hint line. `Typing` is the state of one reading: buffer,
place in the history and draft, armed Ctrl-C, menu selection. Two callers:

- `read` (between tasks): draws with the real cursor and the hint row, waits for events; `Submit` moves the
  files to `sent`, `erase` draws the box at rest, the text goes into the history.
- `busy_event` (while a task runs, from the key watcher thread, `Editor` behind `Arc<Mutex>`): the same `handle`;
  `Submit` puts `Queued { text, files }` into the queue (or `Busy::Full`, the text stays), then `dock_idle` draws.

The chat prompt's `Typing` lives on in `Editor::typing`: what is typed while a task runs is in the box when `read`
starts. An edited shell command (`remember = false`) gets a `Typing` of its own.

## Prompt queue

`Queue = Arc<Mutex<VecDeque<Queued>>>`, at most `QUEUE_MAX` (5), shared by the box and `Terminal` (`queue`).

- Shown above the box (`queue_lines`), only while it is not empty: an empty row (apart from the output), the
  title ` Queued prompts:` with the key `Alt + 1…5 drops one` at the right (left out when too narrow),
  then one row per prompt: ` N  <first line of the text, line ends as ⏎, cut with …>  📎 <files> ` on
  `Style::queue_bg`. The dock grows by `QUEUE_HEAD_ROWS` (2) plus one row per prompt. `View::set_above_border`
  tells the output how many rows come before the top border, where its scroll mark sits.
- Drop: Alt+N or Ctrl+N (where the terminal reports Ctrl with a digit) in `handle`; `drop_queued(index)`.
- The tab title shows the count after its icon (`⏳ +2 …`), see the tab title in `../terminal-ui/SKILL.md`.
- `Terminal::input` takes the oldest queued prompt before it reads the box (`redraw_idle` shows the shorter queue),
  then `submitted` treats it like a typed one: echo, files confirmed, commands run. So a prompt is echoed when it
  runs, in order with the output. Esc stops only the running task; Ctrl-C twice quits and drops the queue.
- While a task runs the box has no hint row (the progress line uses it); `draw_docked` with `hint = None` puts the
  real cursor back into the output and the box draws its own (`with_cursor`).

## Token window (`dialog::ask_token`)

A modal window centered on the terminal's alternate screen (`ESC[?1049h` … `l`), so the chat is not overwritten
and comes back as it was; `Terminal::token_dialog` then calls `Editor::refresh_dock` to set the scroll region
again. Content (`dialog::rows`, which also returns the row of the token field): title, the reason, five numbered steps
to get the token (open the site, open the console with F12 or Ctrl+Shift+J, type `allow pasting`, run the
`SNIPPET`, paste the token; keys and things to type colored through `Style::inline`), the field, the buttons
`[ OK ]` and `[ Cancel ]` (the focused one inverted), and below the window the key hints, wrapped to the
window's width over as many lines as needed and aligned with its text (`hint_lines`, also used by the list window). The README section "Log in" has the same steps.

| Key | Effect |
|---|---|
| paste, characters | added to the token; shown as `•` only, with the count; whitespace and control characters dropped |
| Backspace, Ctrl-U | delete one, delete all |
| Tab, Shift+Tab, Left, Right | switch the focused button |
| Enter | the focused button; OK without a token does nothing |
| Esc, Ctrl-C, Ctrl-D | Cancel |

While the window is open, `Terminal::token_dialog` switches the mouse reports off, so the terminal handles the
mouse itself: its own selection and copy work on the steps, the site in step 1 is a real link (`Style::inline`
→ `links`), and a right click pastes (it arrives as a paste event). They are switched on again after.

The token leaves the function only as the return value: it is never printed, logged or put into `Editor` history.

`dialog::pick(style, title, items)` is the list window of `/sessions`, drawn the same way: one row per item (cut
to the window), the selected one inverted with `❯`, "N more below" in the border when the list is longer than
the screen. Up, Down, PgUp, PgDn, Home, End move; Enter returns the index; Esc, `q`, Ctrl-C close.
