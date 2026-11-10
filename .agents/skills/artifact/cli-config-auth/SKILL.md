---
name: cli-config-auth
description: The front of the CLI — `src/cli.rs` (clap commands and run flags), `src/main.rs` (dispatch, `login`, logout, `doctor`, building the `Conversation`), `src/config.rs` (`Paths`, `Settings` sections and defaults, `config.toml`) and `src/auth.rs` (`auth.json` with mode 0600, import, token paste). Use when adding a command, flag or config key, changing login or doctor, or when the config or auth file is read wrong.
---

# CLI, config, auth

Entry `main()` in `src/main.rs`: `Cli::parse` → `Paths::new` → `Settings::load_or_create` → dispatch.

## Commands (`src/cli.rs`)

| Input | Handler |
|---|---|
| `dsc <task words>` + `--yes`, `--think`, `--search`, `--expert` (`RunArgs`) | `conversation()` + `agent::run::run` with `ui::Terminal` |
| `dsc login --import <file>` / `--token` | `login()`; without a mode it bails (browser login is `TASKS.md`) |
| `dsc logout` | `Auth::remove` |
| `dsc doctor` | `doctor()`: paths, model, throttle, then token (`create_challenge`), wasm, proof of work; sends no prompt, not throttled |
| `dsc chat --resume` | bails "not implemented yet" |

`conversation()` builds `TurnOpts` from the flags over `[deepseek]`: `--expert` → `model_type = "expert"`,
`--think` and `--search` switch the flags on.

## Config (`src/config.rs`)

`Paths`: home = `$DSC_HOME`, else `~/.config/dsc` (created); `config()`, `auth()`, `cache()`.
`Settings::load_or_create` writes the defaults when `config.toml` is missing; unknown keys fail
(`deny_unknown_fields`), missing keys take defaults.

| Section | Struct | Keys |
|---|---|---|
| `[deepseek]` | `Deepseek` | `model_type`, `thinking`, `search`, `locale`, `timeout_secs` |
| `[throttle]` | `Throttle` | see `../deepseek-client/SKILL.md` |
| `[agent]` | `Agent` | `max_steps`, `max_output_bytes`, `command_timeout_secs` |
| `[policy]` | `Policy` | `allow`, `deny`, see `../approval-policy/SKILL.md` |

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
