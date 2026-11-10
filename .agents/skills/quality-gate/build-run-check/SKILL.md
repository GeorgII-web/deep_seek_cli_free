---
name: build-run-check
description: How to check, run and publish deep_seek_cli_free — the gates (`cargo test`, `cargo fmt --check`, clippy with `-D warnings`), the opt-in solver test against the site wasm, a throwaway home with `DSC_HOME`, live checks and their limits, recording a new SSE fixture, the build profiles, and the rules of the public GitHub repo (noreply commit email, no personal data, push with each change). Use when reporting a change done, testing against the live site, or before a commit or push.
---

# Build, run, check

## Gates before "done"

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

No test calls DeepSeek: the loop runs against `Script` and `FakeUi` (`src/agent/run.rs`), the parser against
`src/deepseek/fixtures/`.

Solver against the real module (opt-in, needs the file once):

```bash
curl -s -o var/tmp/sha3.wasm https://fe-static.deepseek.com/chat/static/sha3_wasm_bg.7b9ca65ddd.wasm
DSC_TEST_WASM=$PWD/var/tmp/sha3.wasm cargo test -- --ignored --nocapture
```

## Build profiles (`Cargo.toml`)

- `wasmi` needs the feature `portable-dispatch`: its default tail-call dispatch overflows the stack in debug builds.
- `[profile.dev.package."*"] opt-level = 3`: unoptimized, the interpreter needs minutes per proof of work.
- Release: `lto`, `codegen-units = 1`, `panic = "abort"`, `strip`.

## Live checks

| Check | Command | Note |
|---|---|---|
| each step, no prompt | `DSC_HOME=var/home cargo run -- doctor` | throwaway home; needs `var/home/auth.json` (`dsc login --token` there) |
| read-only task | `cargo run -- "how much disk space is free on /"` | uses an allowed command |
| approval question | a task that needs an unlisted command, in a real terminal or with `scripts/pty-drive.py` | without a terminal every question is a no |
| input box, drawn screen | `scripts/pty-drive.py '["/help\r", "abc", "\x1b\r", "def"]'` | pseudo-terminal, scripted keys, prints the screen; sends a request only when a task line is submitted |

- Never run `dsc --yes` against the live account from an agent session.
- Every live prompt counts against the user's account: a few per change, the throttle stays on.
- Record a new stream shape: put a temporary dump of `chunk` into the loop of `Conversation::ask`, save the body
  under `src/deepseek/fixtures/`, add a parser test, remove the dump. A fixture holds only the test prompt's answer.

## Public repo

- Remote `origin` = `github.com/GeorgII-web/deep_seek_cli_free`, branch `main`, MIT.
- Commits use the repo-local GitHub noreply email; never another address.
- No personal paths, names, tokens or data in tracked files; work files go to git-ignored `var/`.
- A code change, its skills, `AGENTS.md` and `README.md` go in the same commit, pushed right away.
