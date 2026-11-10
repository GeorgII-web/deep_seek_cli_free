# deep_seek_cli_free

Rust CLI for Linux (binary `dsc`): an agent that runs commands on this machine, driven by the free DeepSeek web
account (chat.deepseek.com), no paid API key. One static binary, no node, no browser at runtime.

## Layout (target; `[ ]` = not written yet, see `TASKS.md`)

| Path | Role |
|---|---|
| `src/main.rs` | entry (tokio): parse CLI, `Paths`, `Settings`, dispatch; `login()`, logout, `doctor()` (paths, token, wasm, proof of work; sends no prompt), `raw()` = temporary: a task prints the raw SSE of one turn |
| `src/cli.rs` | clap: `Cli` = optional `Command` (`chat --resume`, `login --import/--token`, `logout`, `doctor`) or `RunArgs` (task words, `--yes`, `--think`, `--search`, `--expert`) |
| `src/config.rs` | `Paths` (`$DSC_HOME`, else `~/.config/dsc`; `config()`, `auth()`, `cache()`), `Settings` = `[deepseek]`, `[throttle]`, `[agent]`, `[policy]` with defaults, unknown keys fail; `load_or_create` writes the defaults when the file is missing |
| `src/auth.rs` | `Auth` (`token`, `cookie`, `wasm_url`): `load`, `require`, `import` (FreeDeepseekAPI file), `new`, `save` (0600), `remove`; browser login over CDP `[ ]` |
| `src/deepseek/client.rs` | `Client`: headers, `create_session`, `create_challenge`, `completion` (returns the SSE response), `fetch_wasm`; `ApiError` (`TokenDead`, `RateLimited`), envelope check `biz_data` |
| `src/deepseek/pow.rs` | `solve(wasm, challenge)` in `wasmi`, `header()` = `X-DS-PoW-Response` value |
| `src/deepseek/session.rs` | `Conversation`: session id, `parent_message_id`, wasm cache, `pow_header()`, `send()` with throttle and 429 retries |
| `src/deepseek/throttle.rs` | `delay_ms` (pure rule), `acquire` (wait + record in `<home>/cache/throttle.json`) |
| `src/deepseek/stream.rs` `[ ]` | SSE parser → `Event` |
| `src/agent/` `[ ]` | `protocol.rs` prompt + action parser, `run.rs` loop, `tools.rs` shell/read/write, `policy.rs` approval |
| `src/ui.rs` `[ ]` | streamed output, approval prompt, colors |

## Skills (`.agents/skills`, read the one that matches before working)

| Skill | Read when |
|---|---|
| [architecture](.agents/skills/general/architecture/SKILL.md) | orienting, any new module, the DeepSeek web protocol, the agent protocol, approval rules |

## Rules

- Every code piece is described in a skill; update the skill in the same change.
- DeepSeek specifics (URLs, headers, wasm URL, model types) live only in `src/deepseek/`; the agent talks to the
  `Backend` trait.
- No command runs without passing `agent::policy`; the default is ask.
- Secrets (`auth.json`, tokens, cookies) never in the repo, logs or error messages.
- Work and temp files go to git-ignored `var/`.

## Checks

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings
DSC_HOME=var/home cargo run -- doctor    # throwaway home; live calls, no prompt sent
DSC_TEST_WASM=<sha3.wasm> cargo test --release -- --ignored   # solver against the site module
```
