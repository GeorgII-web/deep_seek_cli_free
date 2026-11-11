---
name: cli-config-auth
description: The front of the CLI — `src/cli.rs` (clap commands and run flags), `src/main.rs` (dispatch, `login`, logout, `doctor`, building the `Conversation`), `src/config.rs` (`Paths`, `Settings` sections and defaults, `config.toml`) and `src/auth.rs` (`auth.json` with mode 0600, import, token paste). Use when adding a command, flag or config key, changing login or doctor, or when the config or auth file is read wrong.
---

# CLI, config, auth

Entry `main()` in `src/main.rs`: `Cli::parse` → `Paths::new` → `Settings::load_or_create` → dispatch.

## Commands (`src/cli.rs`)

| Input | Handler |
|---|---|
| `dsc <task words>` + `--yes`, `--think`, `--search` / `--no-search`, `--expert`, `--session=<code>` (`RunArgs`) | `conversation()` + `agent::run::run` with `protocol::first_prompt` and `ui::Terminal` |
| `dsc` without a task (flags allowed), `dsc chat` (no flags) | `chat()` |
| `dsc login` | `login()`: without a flag the token window (`Terminal::token_dialog`), `--token` a hidden prompt, `--import <file>` a file |
| `dsc logout` | `Auth::remove` |
| `dsc doctor` | `doctor()`: paths, model, throttle, then token (`create_challenge`), wasm, proof of work; sends no prompt, not throttled |

`web_search(settings, run)` = `deepseek.search` (default true) or `--search`, unless `--no-search`: it sets
`search_enabled` of every request and the web rule of the first prompt. `Settings::upgrade` (`version` key) turns
`search` on once in a config written before that default.

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
| `/model` | `Terminal::pick` ("DeepSeek", "Mock"); a change: `ensure_login` for the real account, a new `Model`, clean screen, `ModeSaver::save_api` → `deepseek.api` |
| `/login` | `relogin`: token window, save, `Model::set_client` |
| `/sessions` | `Terminal::pick` over `Store::rows`; the chosen one: new `Conversation` + `resume(id, parent)`, `first = false` |
| `/help` | `Terminal::help` |
| `/exit`, `/quit`, Ctrl-D on an empty box, Ctrl-C twice | leave |
| other `/word` | "unknown command"; a text with a second `/` or a space (a path, a sentence) is a task |
| anything else | a task; an error of the run (network, dead token, stopped with Esc) is shown in the finish line and the chat goes on; `Interrupt::Quit` ends the chat |

A new command: a row in `COMMANDS` (`src/ui/mod.rs`; feeds the menu and `/help`) and an arm in `chat()`.

Model (`src/model.rs`): the chat's `Backend` is `Model::Deepseek(Conversation)` or `Model::Mock` (`src/mock.rs`),
built by `model(paths, settings, run, api)`. With the mock there is no login, no token check, no sessions; `Mock::new(&settings.mock)` sets its speed
(`Mock::default()` has no pauses: unit tests); `mock_help()` (what it is, where the speed is set, `mock::TASKS`)
is printed when the chat starts on the mock and when `/model` switches to it; its
`dead` task shows the token window and saves nothing. `ModeSaver` owns the one `Settings` copy that is written
back, for the mode and for the model choice.

Sessions (`src/sessions.rs`): after every task `chat()` writes the conversation in use to `<home>/sessions.json`
(`Store::upsert`: newest first, 50 kept): `id` = DeepSeek's chat session id, `parent_message_id` = the last
answer (`Conversation::state`), `title` = the first task, `cwd`, `tasks`, `updated`. The messages stay at
DeepSeek; switching only points the next prompt at that session, so the model has the whole history and the
rules of the first prompt. Only conversations started here are listed. After every task the screen output (`Terminal::output`: JSON with the lines and the
foldable blocks) is saved to `<home>/sessions/<id>.log` (`Paths::transcript`); a switch shows it again with
working folds (`Terminal::show_output`). `/new` and
`/model` clear `current` and the screen.

`--session=<code>` (the session id): the chat starts in that conversation like a switch (`resume`, saved screen,
`first = false`); a single task continues it with `next_prompt`; an unknown code is an error line (chat) or an
error exit (single task). When the chat ends with a conversation in use, it prints
`dsc --session=<id>` after the terminal is restored. The tab title (`Terminal::set_title`) is `dsc`, or
`dsc · <title>` once the conversation has a session. A session
deleted on the website fails at the next task with DeepSeek's error.

No local transcript (`TASKS.md`). Inputs are kept in `<home>/history.json` (`Paths::history`).
A single task (`dsc "<task>"`) uses `Terminal::new(None, yes)` and exits with code 1 when the run failed, 130 when it
was interrupted. Both modes take their `Interrupts` from `Terminal::interrupts`.

## Config (`src/config.rs`)

`Paths`: home = `$DSC_HOME`, else `~/.config/deep_seek_cli_free` (created); `config()`, `auth()`, `history()`, `sessions()`, `cache()`.
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
| `[deepseek]` | `Deepseek` | `api` (`deepseek` or `mock`, see `/model`), `model_type`, `thinking`, `search`, `locale`, `timeout_secs` |
| `[throttle]` | `Throttle` | see `../deepseek-client/SKILL.md` |
| `[agent]` | `Agent` | `max_steps`, `max_output_bytes`, `command_timeout_secs` |
| `[ui]` | `Ui` | `mouse_scroll` (the app handles the mouse: wheel, click to fold, drag to select, right click to copy or paste; `Terminal::set_mouse`) |
| `[mock]` | `MockSpeed` | `wait_ms` (pause before an answer), `word_ms` (pause between words): the built-in mock is slow on purpose, to watch the UI |
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

`relogin(paths, settings, terminal, reason)` in `src/main.rs`: `Terminal::token_dialog` → `Auth::new(token, "", old wasm_url)` →
`Auth::save` → a `Client` for it; `None` when the user cancelled. It runs in the chat when there is no token at
the start (cancel = exit), when `check_token` finds the saved one dead at the start (one `create_challenge`
call, no prompt; it goes through `throttle::acquire`, and it is skipped when the token worked in the last
`TOKEN_TRUST_SECS` = 15 minutes, see `throttle::token_ok`, or when `DSC_SKIP_TOKEN_CHECK` is set; cancel = the chat opens with a note; a network error only gives a note), on
`/login`, and after a task failed with `ApiError::TokenDead`: with a new token
`Conversation::set_client` keeps the conversation and the same prompt runs again, until it works or the user
cancels. The token never reaches the input history (`Editor`) or a prompt.
