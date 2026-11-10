---
name: cli-config-auth
description: The front of the CLI — `src/cli.rs` (clap commands and run flags), `src/main.rs` (dispatch, `login`, logout, `doctor`, building the `Conversation`), `src/config.rs` (`Paths`, `Settings` sections and defaults, `config.toml`) and `src/auth.rs` (`auth.json` with mode 0600, import, token paste). Use when adding a command, flag or config key, changing login or doctor, or when the config or auth file is read wrong.
---

# CLI, config, auth

Entry `main()` in `src/main.rs`: `Cli::parse` → `Paths::new` → `Settings::load_or_create` → dispatch.

## Commands (`src/cli.rs`)

| Input | Handler |
|---|---|
| `dsc <task words>` + `--yes`, `--think`, `--search`, `--expert` (`RunArgs`) | `conversation()` + `agent::run::run` with `protocol::first_prompt` and `ui::Terminal` |
| `dsc` without a task (flags allowed), `dsc chat` (no flags) | `chat()` |
| `dsc login --import <file>` / `--token` | `login()`; without a mode it bails (browser login is `TASKS.md`) |
| `dsc logout` | `Auth::remove` |
| `dsc doctor` | `doctor()`: paths, model, throttle, then token (`create_challenge`), wasm, proof of work; sends no prompt, not throttled |

`conversation()` builds `TurnOpts` from the flags over `[deepseek]`: `--expert` → `model_type = "expert"`,
`--think` and `--search` switch the flags on.

## Chat (`chat()` in `src/main.rs`)

Calls `Terminal::open_chat` (clears the screen and docks the input box when on a terminal), then reads inputs with `Terminal::input` (the multiline box, or plain lines without a
terminal) until it returns `None`. One `Conversation` and one "always" set live for the whole chat; each task is
wrapped in `Terminal::begin` and `finish`; the first task uses `protocol::first_prompt`, later ones `next_prompt`.

| Input | Effect |
|---|---|
| empty | ignored |
| `/new` | new `Conversation`, the next task gets the rules again |
| `/mode` | `Terminal::toggle_mode` (ask ⇄ allow all) |
| `/help` | `Terminal::help` |
| `/exit`, `/quit`, Ctrl-D on an empty box, Ctrl-C twice | leave |
| other `/word` | "unknown command"; a text with a second `/` or a space (a path, a sentence) is a task |
| anything else | a task; an error of the run (network, dead token, stopped with Esc) is shown in the finish line and the chat goes on; `Interrupt::Quit` ends the chat |

A new command: a row in `COMMANDS` (`src/ui/mod.rs`; feeds the menu and `/help`) and an arm in `chat()`.

Not saved: no transcript, no resume (`TASKS.md`). Inputs are kept in `<home>/history.json` (`Paths::history`).
A single task (`dsc "<task>"`) uses `Terminal::new(None, yes)` and exits with code 1 when the run failed, 130 when it
was interrupted. Both modes take their `Interrupts` from `Terminal::interrupts`.

## Config (`src/config.rs`)

`Paths`: home = `$DSC_HOME`, else `~/.config/deep_seek_cli_free` (created); `config()`, `auth()`, `history()`, `cache()`.
`Paths::new` calls `migrate`: a folder `~/.config/dsc` of the first versions is renamed to the new one, once,
only when the new one does not exist; `$DSC_HOME` skips that. `Settings::load_or_create` writes the defaults when
`config.toml` is missing; unknown keys fail (`deny_unknown_fields`), missing keys take defaults; a deny list
that protects only the old token path gets the new one (`guard_auth_file`). `Settings::save` rewrites the whole
file (comments are lost).

Saved mode: `policy.mode` (`config::Mode`: `ask`, `allow_all`) is the mode at the start. `ModeSaver` in
`src/main.rs` compares the mode flag with its last value after every input, `/mode` and task (`sync`) and saves
when the user switched it; a mode that only comes from `--yes` is never written.

| Section | Struct | Keys |
|---|---|---|
| `[deepseek]` | `Deepseek` | `model_type`, `thinking`, `search`, `locale`, `timeout_secs` |
| `[throttle]` | `Throttle` | see `../deepseek-client/SKILL.md` |
| `[agent]` | `Agent` | `max_steps`, `max_output_bytes`, `command_timeout_secs` |
| `[policy]` | `Policy` | `mode`, `allow`, `deny`, see `../approval-policy/SKILL.md` |

A new key: the struct field, its default, the README config block, the skill of its section.

## Auth (`src/auth.rs`)

`<home>/auth.json` (0600): `token` (the site's `localStorage.userToken.value`), `cookie`, `wasm_url`.

| `dsc login` mode | How |
|---|---|
| `--import <file>` | reads FreeDeepseekAPI's `deepseek-auth.json` (`token`, `cookie`, `wasmUrl`) |
| `--token` | token pasted on a hidden prompt, cookie optional |
| (default) | starts the system Chrome with `--remote-debugging-port` and profile `<home>/chrome-profile`, the user logs in, the CLI reads `userToken` and cookies over CDP, closes it; needs WSLg or a desktop |

`Auth::save` removes the old file and creates the new one with mode 0600; `Auth::require` gives "not logged in".
Parse errors of auth files never quote the content. Verified live: the token alone is enough, the cookie may be
empty. The browser mode is not written yet.
