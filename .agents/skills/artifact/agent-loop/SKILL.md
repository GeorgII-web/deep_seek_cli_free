---
name: agent-loop
description: The agent in `src/agent/` and `src/ui.rs` — the text protocol that emulates tool calls (`protocol.rs`, first prompt, `dsc` action block, parser, result messages), the tools (`tools.rs`, shell in its own process group, output cap, file read and write), the loop and the `Ui` trait (`run.rs`) and the terminal output. Use when the model's actions are parsed wrong, a tool misbehaves, a tool is added, the prompt or the loop end conditions change.
---

# Agent loop

Entry `agent::run::run(backend, ui, task, settings, yes)`, called from `src/main.rs` for `dsc "<task>"`.

| File | Content |
|---|---|
| `src/agent/protocol.rs` | `Action`, `FENCE`, `first_prompt`, `parse` → `Parsed`, `shell_result` |
| `src/agent/tools.rs` | `shell` → `ShellOutput`, `cap`, `read_file`, `write_file`; constants `KEEP_BYTES`, `DRAIN` |
| `src/agent/run.rs` | `run`, `act` (policy, question, edit), `execute`; `Ui`, `Decision`; tests with `Script` and `FakeUi` |
| `src/ui.rs` | `Terminal` implements `Ui`; `RESULT_LINES` = lines of a result shown to the user |

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

- `parse`: lines between a `` ```dsc `` line and a `` ``` `` line; an unclosed block counts. The **first** block
  runs; further blocks are ignored and the model is told so. Broken JSON, an unknown tool or field → `Invalid`,
  its text goes back to the model.
- Output is capped per stream to `agent.max_output_bytes` (`tools::cap`: head + tail, cut on character borders).
- `tools::shell`: `bash -c`, stdin closed, own process group; timeout or Ctrl-C kills the group; output of a
  finished command is collected for 0.5 s more at most (a background child may hold the pipe).
- The loop ends on a final answer, after `agent.max_steps` answers, or on Ctrl-C while waiting for the model
  (Ctrl-C while a command runs only kills the command and tells the model).
- The loop knows only the `Backend` and `Ui` traits; `run.rs` tests script both.

## Loop steps

1. `prompt` = `first_prompt(task, cwd)`.
2. `backend.ask(prompt)`; pieces go to `Ui::event`, then `Ui::answer_end`.
3. `parse(answer)`: `Final` → return; `Invalid` → its text is the next prompt; `Action` → `act`.
4. `act`: `policy::check` (see `../approval-policy/SKILL.md`), then `execute`; the result text is shown with
   `Ui::result` and becomes the next prompt.
5. After `agent.max_steps` answers: `Ui::note` and return.

## Terminal (`ui::Terminal`)

Answer text to stdout, reasoning dimmed; the action line, question, result preview and notes to stderr. The raw
`dsc` block is still printed as part of the answer (`TASKS.md`).

## Adding a tool

A variant in `Action` (serde tag `tool`), its arms in `why` and `summary`, a line in `first_prompt`, the run in
`execute`, the subject and key in `policy::check` and `policy::always_key`, tests in `protocol.rs` and `run.rs`,
the tables here and in `README.md`.
