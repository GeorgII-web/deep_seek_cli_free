# deep_seek_cli_free

Rust CLI for Linux (binary `dsc`): an agent that runs commands on this machine, driven by the free DeepSeek web
account (chat.deepseek.com), no paid API key. One static binary, no node, no browser at runtime.

## Layout (target; `[ ]` = not written yet, see `TASKS.md`)

| Path | Role |
|---|---|
| `src/main.rs` | entry: parse CLI, load config, dispatch |
| `src/cli.rs` `[ ]` | clap commands: `dsc "<task>"`, `dsc chat`, `dsc login`, `dsc logout`, `dsc doctor` |
| `src/config.rs` `[ ]` | `Paths` (`$DSC_HOME`, else `~/.config/dsc`), `config.toml`, `auth.json` (mode 0600) |
| `src/auth.rs` `[ ]` | token + cookie store; login by import file, pasted token, or own Chrome over CDP |
| `src/deepseek/` `[ ]` | `client.rs` HTTP calls, `pow.rs` proof of work, `stream.rs` SSE parser, `session.rs` chat session |
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
```
