---
name: approval-policy
description: The approval rules of `src/agent/policy.rs` and the approval question of the terminal UI in `src/ui.rs` — check order (deny list, `--yes`, always in this run, allow list, ask), how allow and deny entries match, what `always` covers, the edit path, the default lists in `src/config.rs` and why no content-printing program is allowed by default. Use when a command runs or is blocked unexpectedly, the defaults or matching rules change, or a new action kind needs a rule.
---

# Approval policy

`policy::check(action, policy, yes, always) -> Verdict` is the only gate; `agent::run::act` calls it before every
action and again after an edit. Lists come from `[policy]` in `config.toml` (`config::Policy`).

## Modes

`yes` is the mode flag `Terminal::mode()` (an `Arc<AtomicBool>`): false = ask, true = allow all. The start value is `policy.mode` from
`config.toml` (the last mode the user chose, saved by `ModeSaver`, see `../cli-config-auth/SKILL.md`) or `--yes`; Shift+Tab (input box and while a task runs) and `/mode` switch it. `agent::run::act` reads it before
every action, so a switch in the middle of a task counts for the next action. The deny list holds in both modes.

## Rules

`check` order: deny list → `--yes` → allowed "always" in this run → allow list → ask.

| Rule | Detail |
|---|---|
| `policy.deny` | substring of the command or file path (whitespace squeezed); never runs, also with `--yes` and after an edit; the model is told not to retry |
| `policy.allow` | a program or a program with its first arguments (`git status`), matched on whole words; only for a simple command, i.e. none of `; & | > < $ ( ) { }` backtick or newline |
| files | `read_file` and `write_file` always ask without `--yes` |
| ask (`ui::Terminal::approve`) | `y` once, `a` always in this run (the program of a simple command, or the tool; not offered for compound commands), `n` no (the model may try another way), `e` edit the command, then only the deny list is checked again |
| no terminal on stdin | a question counts as no |

Defaults: allow = `ls pwd df du uname whoami date which stat wc file`, `git status`; nothing that prints file
content, because every result is sent to DeepSeek. Deny = destructive patterns plus `.ssh/id_`,
`.aws/credentials`, `deep_seek_cli_free/auth.json`, `dsc/auth.json`. The deny list is a plain substring guard, not a sandbox.

## Code

| Item | Role |
|---|---|
| `Verdict` | `Allow`, `Ask`, `Deny(pattern)` |
| `SHELL_SYNTAX` | characters that make a command compound; such a command never matches the allow list or "always" |
| `squeeze` | whitespace normalization of commands and entries |
| `always_key` | program of a simple command, `read_file` or `write_file`; `None` for compound commands |
| `run::act` | on `Decision::Edit` the new command is checked with `yes = true`, so only the deny list applies |

## Changing defaults

`impl Default for Policy` in `src/config.rs`; mirror the lists in `README.md` (Configuration, Safety). An existing
`config.toml` keeps its old lists: say so in the change notes.
