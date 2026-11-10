# Tasks

Order = build order. Each task ends with the checks in `AGENTS.md` and an update of the skills. Delete a task
when done.

| # | Task | Done when |
|---|---|---|
| 5 | needs a live token (`dsc login --token`); confirm `doctor` shows token, wasm, pow ok; record a raw SSE (`dsc "2+2"` prints it now); `deepseek::stream` + `Backend` impl: SSE patches → `Event`, `parent_message_id`; replace `raw()` in `main.rs` | `dsc ask "2+2"` streams the answer; a second turn in the same session remembers the first; parser unit tests on recorded SSE |
| 6 | `agent::protocol`: instruction text, action block parser | unit tests: plain answer, one block, broken JSON, several blocks |
| 7 | `agent::tools`: `shell` (bash -lc, timeout, process group kill, output cap), `read_file`, `write_file` | unit tests incl. timeout and cap |
| 8 | `agent::policy` + `ui`: allow/deny lists, ask prompt (`y/a/n/e`), `--yes` | deny wins over `--yes`; tests on the matcher |
| 9 | `agent::run`: the loop, step limit, Ctrl-C handling, transcript in `sessions/` | `dsc "how much disk space is free"` asks, runs `df`, answers |
| 10 | `dsc chat` REPL with `--resume`, flags `--think`, `--search`, `--expert` | several tasks in one conversation |
| 11 | `dsc login` over CDP (own Chrome profile) | fresh login without FreeDeepseekAPI |
| 12 | Skills per artifact (`deepseek-client`, `agent-loop`, `policy`, `build-run-check`), README, release build (musl static), GitHub remote | `cargo build --release --target x86_64-unknown-linux-musl` gives one binary |

## Open questions

- Binary name `dsc` ok?
- Proof of work is only checked against a made-up challenge (module loads, full search runs); a real pair needs a live token.
- Is the token alone enough, or is the cookie needed too? Check with the first live token.
