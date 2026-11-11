---
name: scrollback
description: The chat's output kept by the app in `src/ui/scrollback.rs` — `View` with its lines and wrapped rows, scrolling while the box stays docked, foldable blocks for action output, the status line with the progress icon, mouse selection, save and restore with a session (`Snapshot`), repaint rules. Use when scrolling, folding, selection, the progress icon in the output or a restored session misbehaves, or the output and the screen get out of step.
---

# Scrollback

`scrollback::View` holds everything printed in the chat's screen layout; `ui::Terminal` records into it
(`emit`) and the terminal UI skill (`../terminal-ui/SKILL.md`) describes the layout around it.

## The model (screen layout only)

The app keeps its own output: complete lines plus one open line, wrapped to the screen width (`wrap` knows
escape sequences and carries colors over a break). The view is the last rows of that; `offset` = rows below it.

| Case | What happens |
|---|---|
| at the bottom | `Terminal::emit` records the text (`View::push`) and writes it to the terminal, which wraps and scrolls inside the scroll region exactly like the model |
| scrolled up | `push` only records; the view stays on the same rows (`offset` grows), nothing is written |
| a block (`begin_fold`, `end_fold`) | the lines pushed between them are the body under the header line; `end_fold(folded_text, open_text, folded)` sets the two header texts and hides the body when folded; `rewrap` skips hidden bodies, `wrapped` keeps the line each row comes from |
| the running step | `Spinner` thread → `View::animate(frame)`: rewrites the header of the block being written and draws only that row (between save and restore cursor); nothing happens without a block being written. The thread releases its own state lock before it takes the view's |
| select, copy, paste | `mouse_event`: left button down → `select_start`, drag → `select_to` (repaint, the selected part inverted and shown without colors); the text stays selected, nothing is copied. Copy = right button down with a selection, or Ctrl-C (`copy_selection`: `View::copy_selection` takes the text, rows of one wrapped line joined, and ends the selection; OSC 52 puts it on the terminal's clipboard; `View::flash` confirms on the dock border). Paste = right button down without a selection, or Ctrl-V in the box where the key reaches the app: `paste_text` = the system clipboard (`clipboard::read`, so text copied in other windows too), else `View::copied`; the box inserts it. Terminals that paste themselves on Ctrl-V send a paste event instead. Press and release on one cell is a click. With a selection, Ctrl-C does not count towards quitting. Shift+drag is the terminal's own selection |
| the step in progress, outside a block | the status line: `Terminal::begin_status` (first event of an answer) puts a line of its own at the start of the answer (`View::begin_status`), the `Spinner` thread fills it every tick (`View::animate_status`: `<frame> <label>… Ns`, the label of the bottom line: "Waiting for DeepSeek", "Thinking", "Answering"), and `Terminal::end_status` (`answer_end`, `finish`) takes the line out again (`View::end_status`: the rest moves up, blocks after it keep their place in the list, one repaint). It never reaches a saved session |
| click, Ctrl-O | `mouse_event` → `View::click(row)`: a left click on the header row of a finished block with a body toggles it (`toggle` keeps the rows above in place); Ctrl-O → `toggle_last`; `unfold_last` keeps the view at the bottom |
| scroll (PgUp, PgDn, wheel 3 rows, Ctrl+Home, Ctrl+End) | `View::scroll` / `top` / `bottom` → `repaint`: every row of the area cleared and drawn from the model, cursor at `out()` |
| dock grows or shrinks, terminal resized | `Editor::sync_view` → `View::resize` (rewrap on a new width) → `repaint`; `Dock::out` comes from `View::out`, no cursor query |
| input sent | `Editor::erase` → `View::bottom` first |
| another conversation (`/sessions`, `--session`, `/new`, `/model`) | `Terminal::output` = `View::snapshot` as JSON (lines, open line, finished blocks with both header texts and their folded state); `Terminal::show_output(saved)` → `View::restore`, so the blocks still fold; text that is not such JSON (a save of an older version, or empty) → `View::replace` as plain lines |
| a window (`token_dialog`, `pick`) | drawn over the screen (no second alternate screen), then `Terminal::redraw`: clear, `Editor::redraw_dock`, `repaint` |

- Everything printed in the screen layout must go through `emit` (`line`, `partial`, `text`) or `erase_partial`
  (removes the open line: the approval question). A direct write would put the model and the screen out of step.
- The scroll mark (`View::indicator`) sits on the dock's top border, right side; the box redraws it after itself.
- Keys reach the view from two places: `Editor::read` (while typing) and the key watcher (while a task runs).
- Mouse reports (`ESC[?1000h` buttons, `ESC[?1002h` motion while a button is down, `ESC[?1006h` SGR form) are on
  when `ui.mouse_scroll` is true: the terminal gives the mouse to the app, which scrolls, folds and selects itself.
- `KEEP_LINES` (20000) lines are kept; `forget` drops the oldest with the blocks that start in them.
- `transcript` (the text printed at exit) writes every block out in full. Saved sessions use `snapshot`, not this.

## Checking a change

`View` has unit tests for wrapping, blocks, clicks, selection, the status line and save/restore. The drawn result:
`scripts/pty-drive.py` (see `../../quality-gate/build-run-check/SKILL.md`); wheel `\x1b[<64;1;1M`, click
`\x1b[<0;<col>;<row>M` then `m`, PgUp `\x1b[5~`.
