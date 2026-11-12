---
name: agent-loop
description: The agent in `src/agent/` — the text protocol that emulates tool calls (`protocol.rs`, first prompt, `dsc` action block, parser, result messages), the tools (`tools.rs`, shell in its own process group, output cap, file read and write), the loop and the `Ui` trait (`run.rs`). Use when the model's actions are parsed wrong, a tool misbehaves, a tool is added, the prompt or the loop end conditions change.
---

# Agent loop

Entry `agent::run::run(backend, ui, Task { prompt, text, browse }, settings, yes, always, interrupts)` = one task (`text` = the task as the user wrote it, `browse` = Browse on); called from `src/main.rs` once for
`dsc "<task>"` and once per line in the chat. `always` (what the user allowed for the session) belongs to the caller; `yes` is the mode flag (`&AtomicBool`,
see `../approval-policy/SKILL.md`).

| File | Content |
|---|---|
| `src/agent/protocol.rs` | `Action`, `FENCE`, `first_prompt`, `next_prompt`, `with_files` (the note `Attached files (…): a, b` before the `Task:` line), `parse` → `Parsed`, `shell_result` |
| `src/agent/interrupt.rs` | `Interrupt`, `Interrupts` (`signal`, `channel`, `clear`, `next`) |
| `src/agent/tools.rs` | `shell` → `ShellOutput`, `cap`, `read_file`, `write_file`; constants `KEEP_BYTES`, `DRAIN` |
| `src/agent/run.rs` | `run`, `act` (policy, question, edit), `execute`; `Ui`, `Decision`; tests with `Script` and `FakeUi` |

## Protocol

First message of a conversation = `first_prompt`: role, the action format, the tools, the rules (one action per
answer, never invent the result, no interactive commands, prefer read-only, plain text when done), the working
directory, the task. The model answers either plain text (final answer, the loop ends) or with a block:

````
```dsc
{"tool":"shell","cmd":"ls -la","why":"see the files"}
```
````

| Tool | Fields | Result message |
|---|---|---|
| `shell` | `cmd`, `timeout_s` (optional, capped by `agent.command_timeout_secs`), `why` | `Result: exit code N` / `timed out, killed` / `killed by a signal`, then `--- stdout ---`, `--- stderr ---` or `(no output)` |
| `read_file` | `path`, `why` | `--- <path> ---` + content |
| `write_file` | `path`, `content` (whole file), `why` | `Written: <path> (N bytes)` |
| `web` (offered only with Browse on) | `query`, `why` | `protocol::web_result`: `Result: searched the web`, `Query: …`, the answer of `Backend::search`; with Browse off `protocol::WEB_OFF` |

The model also answers in DeepSeek's own tool-call markup (DSML, seen 2026-11-12: `<｜｜DSML｜｜ calls>`,
`<｜DSML｜invoke name="shell">`, `<｜DSML｜parameter name="cmd" string="true">…</｜DSML｜parameter>`).
Without a `dsc` block, `parse` reads it with `dsml_calls` into the same JSON objects (`string="false"` = a JSON
value), so it is the same action; `ui::render` hides it like the `dsc` block.
| `done` | none | nothing is run: the task ends without a text answer (`Parsed::Done`) |

- Web: `first_prompt(task, cwd, web)`. With `web` (Browse on) the tools include `web` and the rules say that
  anything on the web or current (weather, news, prices, a page) is got with it, then answered in text; nothing
  from the internet is fetched on the user's machine (no curl, wget, wttr.in, web APIs) unless the user asks for a
  local fetch or the address is only reachable from there. `next_prompt` repeats that, with the tool, for every
  task while Browse is on (a conversation may have started without it). Without it the rules say there is no web
  access and pages are fetched as an action.
- Why a tool and not the model's own search: inside a conversation with the agent rules the real model does not
  search, even with `search_enabled`; it curls `wttr.in` or `ya.ru` (seen 2026-11-12), while the same question
  as a plain message on the site searches ("Found 24 web pages"). So `web` asks the question that way:
  `Backend::search` in a chat of its own, without the rules (see `../deepseek-client/SKILL.md`). And the loop
  replaces a public fetch by such a search, see the loop steps.
- No repeated output: the user sees every result, so the prompt tells the model to send `{"tool":"done"}` as its
  whole answer when the output already is what was asked, and to write text only when it adds something (a
  conclusion, a computed number, an explanation, the reason for a failure). `parse` checks for `done` before the
  actions; the block is hidden like every `dsc` block. The prompts say that `done` is never the first answer to a
  task and that each task starts from nothing; `next_prompt` repeats it, because a model that already did a
  similar task tends to end the new one at once.
- `parse`: lines between a `` ```dsc `` line and a `` ``` `` line; an unclosed block counts. The **first** block
  runs; further blocks are ignored and the model is told so. Broken JSON, an unknown tool or field → `Invalid`,
  its text goes back to the model.
- Output is capped per stream to `agent.max_output_bytes` (`tools::cap`: head + tail, cut on character borders).
- `tools::shell(cmd, timeout, interrupts, on_output)`: `bash -c`, stdin closed, own process group; a timeout or an
  interrupt kills the group; each piece read from stdout or stderr goes to `on_output` at once (`Ui::output`) and
  into the buffers of the result; output of a
  finished command is collected for 0.5 s more at most (a background child may hold the pipe).
- The loop ends on a final answer, after `agent.max_steps` answers, or with an `Interrupt` as the error
  (`interrupt::Interrupts::next` is raced against the model's answer and against a running command, which is
  killed). `Stop` = end this task (Esc; Ctrl-C when there is no terminal), `Quit` = leave the app (Ctrl-C twice).
  `run` calls `Interrupts::clear` first, so a key from before the task does not count. The stopped exchange is
  not continued: the next prompt hangs below the last complete answer.
- The loop knows only the `Backend` and `Ui` traits; `run.rs` tests script both.

## Loop steps

1. The caller builds `prompt`: `first_prompt(task, cwd)` for a new conversation, `next_prompt(task)` for a
   further task in the same one (a short reminder of the rules + the task).
2. `backend.ask(prompt)`; pieces go to `Ui::event`, then `Ui::answer_end`.
3. `parse(answer)`: `Final` → return; `Done` → when an action of this task put output on the screen (`shown`:
   the last action was not denied): `Ui::result_is_answer`, return; otherwise the model gets
   `protocol::DONE_TOO_EARLY` and must act or answer (a model that sends `done` at once would leave the user
   with nothing); `Invalid` → its text is the next
   prompt; `Action` → `act`.
4. Web: with Browse on and `protocol::asks_local_fetch(text)` false (the task says no "curl", "locally", ...),
   a shell action for which `protocol::fetches_public_web(cmd)` holds (an `http(s)://` URL with a public host,
   or a bare host after curl, wget, xh, aria2c, lynx, w3m, ...; localhost, private networks, `.local`, one-word
   hosts and file names do not count) is not asked or run: `web_search` runs a `web` action for the task's
   `text` (why: "Browse is on: searched instead of `<cmd>`") and `protocol::searched_instead` is the next prompt.
   Once per task; a second try goes the normal way (step 5). A `web` action from the model runs the same
   `web_search` with its query (no approval: nothing runs on the machine; with Browse off `WEB_OFF`).
   `web_search` shows it like an action (`Ui::action`, `Ui::running`, the answer's text pieces as `Ui::output`,
   then `Ui::result`); a failed search closes the block with `Error: web search failed …` and ends the task.
5. `act`: `Ui::action` (what the model asked for), `policy::check` (see `../approval-policy/SKILL.md`), maybe
   `Ui::approve`, then `Ui::running` and `execute`; the result text is shown with `Ui::result` and becomes the
   next prompt. After an edit `Ui::action` shows the new command.
6. After `agent.max_steps` answers: `Ui::note` and return.

## `Ui` trait

`event` (pieces of the answer and `Event::Status` progress), `answer_end`, `action`, `approve`, `running`,
`output` (what a running command prints, as it arrives), `result`, `result_is_answer`, `note`. The terminal implementation is `../terminal-ui/SKILL.md`; tests use `FakeUi`.

## Adding a tool

A variant in `Action` (serde tag `tool`), its arms in `why` and `summary`, a line in `first_prompt`, the run in
`execute`, the subject and key in `policy::check` and `policy::always_key`, tests in `protocol.rs` and `run.rs`,
the tables here and in `README.md`.
