# Tasks

Order = build order. Each task ends with the checks in `AGENTS.md` and an update of the skills. Delete a task
when done.

| # | Task | Done when |
|---|---|---|
| 1 | Skeleton: deps (`clap`, `tokio`, `reqwest` rustls + stream, `serde`, `toml`, `anyhow`, `wasmi`, `base64`), `cli.rs`, `config.rs` (`Paths`, `config.toml` defaults, `auth.json` 0600) | `dsc --help` lists the commands, `dsc doctor` prints the paths |
| 2 | `dsc login --import <file>` and `--token` | `auth.json` written from `~/projects/dsfree/FreeDeepseekAPI/deepseek-auth.json`; `dsc logout` removes it |
| 3 | `deepseek::client` + `session`: headers, session create, challenge fetch, error mapping (401/403/429) | `dsc doctor` shows "token ok" or "token dead" against the live site |
| 4 | `deepseek::pow`: wasm download + cache, `wasm_solve` in `wasmi` | unit test with a recorded challenge/answer pair; solve time printed by `doctor` |
| 5 | `deepseek::stream` + `Backend` impl: completion call, SSE patches → `Event`, `parent_message_id` | `dsc ask "2+2"` streams the answer; a second turn in the same session remembers the first; parser unit tests on recorded SSE |
| 6 | `agent::protocol`: instruction text, action block parser | unit tests: plain answer, one block, broken JSON, several blocks |
| 7 | `agent::tools`: `shell` (bash -lc, timeout, process group kill, output cap), `read_file`, `write_file` | unit tests incl. timeout and cap |
| 8 | `agent::policy` + `ui`: allow/deny lists, ask prompt (`y/a/n/e`), `--yes` | deny wins over `--yes`; tests on the matcher |
| 9 | `agent::run`: the loop, step limit, Ctrl-C handling, transcript in `sessions/` | `dsc "how much disk space is free"` asks, runs `df`, answers |
| 10 | `dsc chat` REPL with `--resume`, flags `--think`, `--search`, `--expert` | several tasks in one conversation |
| 11 | `dsc login` over CDP (own Chrome profile) | fresh login without FreeDeepseekAPI |
| 12 | Skills per artifact (`deepseek-client`, `agent-loop`, `policy`, `build-run-check`), README, release build (musl static), GitHub remote | `cargo build --release --target x86_64-unknown-linux-musl` gives one binary |

## Open questions

- Binary name `dsc` ok?
- Auth token of 2026-07-12 in dsfree may be dead; then task 11 moves before task 3.
