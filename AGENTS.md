# deep_seek_cli_free

Rust CLI for Linux (binary `dsc`): an agent that runs commands on this machine, driven by the free DeepSeek web
account (chat.deepseek.com), no paid API key. One static binary, no node, no browser at runtime.

`README.md` is the user manual (install, login, flags, approvals, config); keep it in sync with behaviour changes.
The repo is public: no personal paths, names or data in any tracked file.

## Layout (target; `[ ]` = not written yet, see `TASKS.md`)

| Path | Role |
|---|---|
| `src/main.rs` | entry (tokio): parse CLI, `Paths`, `Settings`, dispatch; a task → `agent::run::run` with a `Conversation` and `ui::Terminal`; `login()`, logout, `doctor()` (paths, token, wasm, proof of work; sends no prompt); `chat` bails "not implemented yet" |
| `src/backend.rs` | `Backend` trait (`ask(prompt, sink) -> answer text`, one value = one conversation) and `Event` (`Thinking`, `Text`); the agent knows only this |
| `src/cli.rs` | clap: `Cli` = optional `Command` (`chat --resume`, `login --import/--token`, `logout`, `doctor`) or `RunArgs` (task words, `--yes`, `--think`, `--search`, `--expert`) |
| `src/config.rs` | `Paths` (`$DSC_HOME`, else `~/.config/dsc`; `config()`, `auth()`, `cache()`), `Settings` = `[deepseek]`, `[throttle]`, `[agent]`, `[policy]` with defaults, unknown keys fail; `load_or_create` writes the defaults when the file is missing |
| `src/auth.rs` | `Auth` (`token`, `cookie`, `wasm_url`): `load`, `require`, `import` (FreeDeepseekAPI file), `new`, `save` (0600), `remove`; browser login over CDP `[ ]` |
| `src/deepseek/client.rs` | `Client`: headers, `create_session`, `create_challenge`, `completion` (returns the SSE response), `fetch_wasm`; `ApiError` (`TokenDead`, `RateLimited`), envelope check `biz_data` |
| `src/deepseek/pow.rs` | `solve(wasm, challenge)` in `wasmi`, `header()` = `X-DS-PoW-Response` value |
| `src/deepseek/session.rs` | `Conversation`: session id, `parent_message_id`, wasm cache, `pow_header()`, `send()` with throttle and 429 retries, `impl Backend` (`ask` = send + parse + keep the answer id as next parent); built with the `TurnOpts` of the run |
| `src/deepseek/throttle.rs` | `delay_ms` (pure rule), `acquire` (wait + record in `<home>/cache/throttle.json`) |
| `src/deepseek/stream.rs` | `Parser`: `feed(bytes) -> Vec<Event>`, `finish()` (error unless status `FINISHED`), `response_message_id`; tests on the recorded streams in `src/deepseek/fixtures/` |
| `src/agent/protocol.rs` | `Action` (`shell`, `read_file`, `write_file`), `first_prompt` (rules + task), `parse` → `Final` / `Action` / `Invalid`, `shell_result` text |
| `src/agent/policy.rs` | `check` → `Allow` / `Ask` / `Deny`, `always_key` (what "always" covers) |
| `src/agent/tools.rs` | `shell` (bash, own process group, timeout, Ctrl-C, output collected), `cap`, `read_file`, `write_file` |
| `src/agent/run.rs` | `run` (the loop), `act` (check, ask, edit), `execute`; `Ui` trait and `Decision`; loop tests with a scripted backend |
| `src/ui.rs` | `Terminal` = `Ui` on the terminal: streamed answer, approval question, result preview, notes |

## Skills (`.agents/skills`, read the one that matches before working)

| Skill | Read when |
|---|---|
| [architecture](.agents/skills/general/architecture/SKILL.md) | orienting, decisions and their reasons, layers, `Backend` and `Ui` traits, runtime files, which skill covers which module |
| [cli-config-auth](.agents/skills/artifact/cli-config-auth/SKILL.md) | commands and flags, `main.rs` dispatch, `doctor`, `config.toml` sections and defaults, `auth.json`, login |
| [deepseek-client](.agents/skills/artifact/deepseek-client/SKILL.md) | DeepSeek calls and headers, error envelope, proof of work, SSE format and parser, conversation, throttle, what to do when the site changes |
| [agent-loop](.agents/skills/artifact/agent-loop/SKILL.md) | action block protocol, first prompt, tools, loop steps, terminal output, adding a tool |
| [approval-policy](.agents/skills/artifact/approval-policy/SKILL.md) | allow, deny, ask, "always", edit, default lists |
| [build-run-check](.agents/skills/quality-gate/build-run-check/SKILL.md) | gates, solver test, live checks, recording a fixture, build profiles, public repo rules |

## Rules

- Every code piece is described in a skill. A code change updates its skill, this file and `README.md` in the same
  commit, pushed right away; a new module gets a row here and a skill (or a section in one).
- DeepSeek specifics (URLs, headers, wasm URL, model types) live only in `src/deepseek/`; the agent talks to the
  `Backend` trait.
- No action runs without passing `agent::policy`; the default is ask.
- What an action prints goes to DeepSeek: the default allow list has no program that prints file content.
- Never run `dsc --yes` against the live account from an agent session; test the loop with the scripted backend.
- Secrets (`auth.json`, tokens, cookies) never in the repo, logs or error messages.
- Work and temp files go to git-ignored `var/`.

## Checks

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings
DSC_HOME=var/home cargo run -- doctor    # throwaway home; live calls, no prompt sent
DSC_TEST_WASM=<sha3.wasm> cargo test --release -- --ignored   # solver against the site module
```
