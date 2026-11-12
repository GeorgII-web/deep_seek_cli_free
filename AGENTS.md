# deep_seek_cli_free

Rust CLI for Linux (binary `dsc`): an agent that runs commands on this machine, driven by the free DeepSeek web
account (chat.deepseek.com), no paid API key. One static binary, no node, no browser at runtime.

`README.md` is the user manual (install, login, flags, approvals, config); keep it in sync with behaviour changes.
The repo is public: no personal paths, names or data in any tracked file.

## Layout (target; `[ ]` = not written yet, see `TASKS.md`)

| Path | Role |
|---|---|
| `src/main.rs` | entry (tokio): parse CLI, `Paths`, `Settings`, dispatch; a task → `agent::run::run` with a `Conversation` and `ui::Terminal`; `ModeSaver` (writes `policy.mode` when the user switched the mode); `check_token` (the saved token is tested when the chat starts, at most every 15 minutes), `relogin` (token window → `auth.json` → new `Client`; when the token is missing or dead at the start, dies during a task, or on `/login`); no task or `chat` → `chat()` (tasks in one conversation, `/new`, `/sessions`, `/model`, `/think`, `/permissions`, `/login`, `/help`, `/exit`; ends on `Interrupt::Quit`); `login()`, logout, `doctor()` (paths, token, wasm, proof of work; sends no prompt) |
| `src/backend.rs` | `Backend` trait (`ask(prompt, sink) -> answer text`, one value = one conversation) and `Event` (`Thinking`, `Text`, `Status`); the agent knows only this |
| `src/mock.rs` | `Mock`: the fake model inside the app (scripted answers by task, no network, paced by `[mock]` so the UI can be watched); `TASKS` lists what it understands |
| `src/model.rs` | `Model`: `Deepseek(Conversation)` or `Mock`, the `Backend` the chat uses; `/model` switches it |
| `src/sessions.rs` | `Session`, `Store` (`<home>/sessions.json`: conversations that can be continued), `title`, `age` |
| `src/cli.rs` | clap: `Cli` = optional `Command` (`chat`, `login --import/--token`, `logout`, `doctor`) or `RunArgs` (task words, `--yes`, `--think`, `--search` / `--no-search`, `--expert`, `--session`) |
| `src/config.rs` | `Paths` (`$DSC_HOME`, else `~/.config/deep_seek_cli_free`; `config()`, `auth()`, `cache()`), `migrate` (one-time move of the old folder `~/.config/dsc`), `Settings` = `[deepseek]`, `[throttle]`, `[agent]`, `[policy]` (with `mode`) with defaults, unknown keys fail; `load_or_create` writes the defaults when the file is missing, `save` rewrites the file |
| `src/auth.rs` | `Auth` (`token`, `cookie`, `wasm_url`): `load`, `require`, `import` (FreeDeepseekAPI file), `new`, `save` (0600), `remove`; browser login over CDP `[ ]` |
| `src/deepseek/client.rs` | `Client`: headers, `create_session`, `create_challenge`, `completion` (returns the SSE response), `fetch_wasm`; `ApiError` (`TokenDead`, `RateLimited`), envelope check `biz_data`; `base()` = `DSC_API_BASE` or DeepSeek; `requests()` = calls sent so far |
| `src/deepseek/pow.rs` | `solve(wasm, challenge)` in `wasmi`, `header()` = `X-DS-PoW-Response` value |
| `src/deepseek/session.rs` | `Conversation`: session id, `parent_message_id`, wasm cache, `pow_header()`, `send()` with throttle and 429 retries, `impl Backend` (`ask` = send + parse + keep the answer id as next parent), `state` / `resume` (continue an earlier conversation), `set_client`; built with the `TurnOpts` of the run |
| `src/deepseek/throttle.rs` | `delay_ms` (gap, per-minute and per-hour caps), `acquire` (wait + record in `<home>/cache/throttle.json`), `cooldown` (pause after a 429, kept across runs) |
| `src/deepseek/stream.rs` | `Parser`: `feed(bytes) -> Vec<Event>`, `finish()` (error unless status `FINISHED`), `response_message_id`; tests on the recorded streams in `src/deepseek/fixtures/` |
| `src/agent/protocol.rs` | `Action` (`shell`, `read_file`, `write_file`), `first_prompt` (rules + task), `next_prompt` (a further task), `parse` → `Final` / `Done` / `Action` / `Invalid`, `shell_result` text |
| `src/agent/policy.rs` | `check` → `Allow` / `Ask` / `Deny`, `always_key` (what "always" covers) |
| `src/agent/interrupt.rs` | `Interrupt` (`Stop`, `Quit`), `Interrupts` (keys from the UI, or the Ctrl-C signal without a terminal) |
| `src/agent/tools.rs` | `shell` (bash, own process group, timeout, interrupt, output collected and passed on live), `cap`, `read_file`, `write_file` |
| `src/agent/run.rs` | `run` (the loop for one task; takes the prompt, the mode flag `yes` read before every action, the session's "always" set and the interrupts), `act` (show, check, ask, edit), `execute`; `Ui` trait and `Decision`; loop tests with a scripted backend |
| `src/ui/mod.rs` | `Terminal` = `Ui` on the terminal plus `open_chat`, `input`, `begin`, `finish`, `info`, `error`, `help`, `token_dialog`, `interrupts` (key watcher: Esc stops a task, Ctrl-C twice quits, Shift+Tab switches the mode), `mode`, `toggle_mode`; two layouts: inline (single task) and screen (chat: alternate screen, box docked at the bottom, output scrolled by the app); `screen_lock`, `screen_write` |
| `src/ui/banner.rs` | `art`: the start picture "DEEP SEEK CLI FREE" in colored block letters, one or two bands by terminal width |
| `src/ui/dialog.rs` | `ask_token`: the token window (masked field, OK / Cancel); `pick`: a list window (arrows, Enter) |
| `src/ui/clipboard.rs` | `read`: the system clipboard through the desktop's own tool, for a paste by right click |
| `src/ui/editor.rs` | `Buffer`, `Editor` (multiline input box inline or docked with a scroll region above, history, slash-command menu), `command_matches`, `read_choice` (one key) |
| `src/ui/scrollback.rs` | `View`: the chat's output kept by the app, wrapped and scrolled (the input box stays at the bottom), with foldable blocks for the output of actions (click or Ctrl-O), mouse selection with copy, and the progress mark on the step that runs |
| `src/ui/render.rs` | `Markdown`: hides the `dsc` block, trims blank lines, styles inline code, code blocks (bar instead of fences), bold, headings, links |
| `src/ui/spinner.rs`, `style.rs` | progress line with a clock, key hint and mode; every color code, two grays for hints (`key`, `faint`) |
| `.github/workflows/ci.yml`, `release.yml` | `ci`: the checks, by hand only; `release`: on a tag `v<version>` the checks, the Linux binary and the GitHub release. A push starts nothing |
| `.githooks/pre-commit` | raises the patch version when a commit changes `src/` |
| `scripts/mock-deepseek.py` | local HTTP stand-in for the DeepSeek web API (challenge, session, SSE completion; tasks like `src/mock.rs`) |
| `scripts/pty-drive.py` | drives the chat in a pseudo-terminal with scripted keys against that mock (starts it itself) and prints scrollback, screen and cursor; the real API only with `DSC_LIVE=1` |

## Skills (`.agents/skills`, read the one that matches before working)

| Skill | Read when |
|---|---|
| [architecture](.agents/skills/general/architecture/SKILL.md) | orienting, decisions and their reasons, layers, `Backend` and `Ui` traits, runtime files, which skill covers which module |
| [cli-config-auth](.agents/skills/artifact/cli-config-auth/SKILL.md) | commands and flags, `main.rs` dispatch, `doctor`, `config.toml` sections and defaults, `auth.json`, login |
| [deepseek-client](.agents/skills/artifact/deepseek-client/SKILL.md) | DeepSeek calls and headers, error envelope, proof of work, SSE format and parser, conversation, throttle, what to do when the site changes |
| [agent-loop](.agents/skills/artifact/agent-loop/SKILL.md) | action block protocol, first prompt, tools, loop steps, `Ui` trait, adding a tool |
| [terminal-ui](.agents/skills/artifact/terminal-ui/SKILL.md) | look of a run, the two layouts, print rules, progress lines, keys while a task runs, mode and hint lines, markdown rendering, colors |
| [scrollback](.agents/skills/artifact/scrollback/SKILL.md) | the output kept by the app: scrolling, foldable action output, the status line, mouse selection, saved sessions, repaint |
| [input-box](.agents/skills/artifact/input-box/SKILL.md) | the multiline box and its keys, history, command menu, copy and paste, the token window and the list window |
| [approval-policy](.agents/skills/artifact/approval-policy/SKILL.md) | allow, deny, ask, "always", edit, default lists |
| [build-run-check](.agents/skills/quality-gate/build-run-check/SKILL.md) | gates, solver test, live checks, recording a fixture, build profiles, GitHub workflows and release steps, public repo rules |

## Rules

- The version moves with the code: `.githooks/pre-commit` raises the patch number in `Cargo.toml`, `Cargo.lock`
  and the README sample whenever a commit changes `src/` (run `git config core.hooksPath .githooks` once in a
  clone). Do not bump the patch by hand; change minor or major by hand in the same commit when it is deserved.
- Every code piece is described in a skill. A code change updates its skill, this file and `README.md` in the same
  commit, pushed right away; a new module gets a row here and a skill (or a section in one).
- DeepSeek specifics (URLs, headers, wasm URL, model types) live only in `src/deepseek/`; the agent talks to the
  `Backend` trait.
- No action runs without passing `agent::policy`; the default is ask.
- What an action prints goes to DeepSeek: the default allow list has no program that prints file content.
- The code enforces the next rule: `client::base()` refuses the real API in unit tests and in any run with
  `DSC_HOME` set, unless `DSC_LIVE=1`. Never set `DSC_LIVE` without the user's word.
- **No local test or try-out calls the real DeepSeek API.** Use the unit tests, the built-in mock or
  `scripts/mock-deepseek.py` (`scripts/pty-drive.py` does). A live call needs the user's word for that one run:
  test runs got the user's account blocked twice on 2026-11-10.
- Never run `dsc --yes` against the live account from an agent session.
- Secrets (`auth.json`, tokens, cookies) never in the repo, logs or error messages.
- Work and temp files go to git-ignored `var/`.

## Checks

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings
DSC_HOME=var/home cargo run -- doctor    # throwaway home; live calls, no prompt sent
DSC_TEST_WASM=<sha3.wasm> cargo test --release -- --ignored   # solver against the site module
```
