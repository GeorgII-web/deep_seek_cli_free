---
name: build-run-check
description: How to check, run and publish deep_seek_cli_free — the gates (`cargo test`, `cargo fmt --check`, clippy with `-D warnings`), the opt-in solver test against the site wasm, a throwaway home with `DSC_HOME`, live checks and their limits, recording a new SSE fixture, the build profiles, the GitHub workflows (checks on push, release on a version tag), and the rules of the public GitHub repo (noreply commit email, no personal data, push with each change). Use when reporting a change done, testing against the live site, or before a commit or push.
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

## Trying it without DeepSeek

**No local test or try-out calls the real API.** The account was blocked twice on 2026-11-10 by test runs.

Guards in the code (`deepseek::client::base`), so a mistake cannot reach the account:

- the unit tests never get the real address (`cfg!(test)`);
- a run with `DSC_HOME` set (only tests set it) is refused the real API unless `DSC_LIVE=1`; `DSC_API_BASE`
  (the mock) always wins.

Proof that nothing leaves the machine, in a network namespace that has only loopback:

```bash
unshare -rn cargo test
unshare -rn sh -c 'ip link set lo up; scripts/pty-drive.py "[(\"\", 1), (\"long\\r\", 2)]" 30 100'
```

| Stand-in | What it replaces | Use |
|---|---|---|
| `Script` + `FakeUi` (`src/agent/run.rs` tests) | model and terminal | `cargo test`: the loop |
| `src/mock.rs` (`deepseek.api = "mock"`, `/model`) | the model, inside the app | UI, keys, approvals; no network at all |
| `scripts/mock-deepseek.py [port]` | the DeepSeek web API over HTTP | client, stream parser, token and rate-limit paths: `DSC_API_BASE=http://127.0.0.1:<port>`; a challenge with algorithm `none` skips the proof of work; a token starting with `dead` is rejected |
| `scripts/pty-drive.py '<keys>' [rows] [columns]` | a person at a terminal | starts the HTTP mock on a free port, a home `var/mock-home` (mock token, no throttle), runs `target/debug/dsc` in a pseudo-terminal, prints scrollback, screen (`-` output, `#` dock), cursor and the mock's request counts with the switches of the last request (`thinking_enabled`, `search_enabled`) |

Both mocks understand the same tasks (`mock::TASKS`): `!<cmd>`, `run <cmd>`, `browse <url>` (answers as if read on the model's side),
`curl <url>` (the local fetch: approving it does fetch that site, not DeepSeek), `read`, `write`, `think`, `slow`,
`long`, `code`, `broken`, `two`, `early` (done at once), `dead`, `429`; anything else is echoed.

```bash
scripts/pty-drive.py '[("", 1), ("!echo hi\r", 2), ("y", 2)]' 24 100        # a task, the question, yes
scripts/pty-drive.py '[("", 1), ("long\r", 3), "\x1b[<64;1;1M"]' 16 100     # scroll while it answers
MOCK_TOKEN=dead DSC_HOME=var/mock-dead scripts/pty-drive.py '[("", 2)]'     # the token window at the start
```

## Live checks

Only on the user's word for that one run (`DSC_LIVE=1 scripts/pty-drive.py …`, or `dsc` with the real home).

- Never run `dsc --yes` against the live account from an agent session.
- A changed stream shape: put a temporary dump of `chunk` into the loop of `Conversation::ask`, let the user run
  one task, save the body under `src/deepseek/fixtures/`, add a parser test, remove the dump, and teach
  `scripts/mock-deepseek.py` the new shape.

## GitHub workflows

| File | Trigger | Does |
|---|---|---|
| `.github/workflows/ci.yml` | by hand only (`workflow_dispatch`) | `cargo fmt --check`, clippy `-D warnings`, `cargo test` on `ubuntu-24.04` |
| `.github/workflows/release.yml` | pushed tag `v*` | checks the tag equals `version` in `Cargo.toml`, the gates, `cargo build --release --locked` on `ubuntu-22.04` (the binary then needs glibc 2.35+), packs `dsc-x86_64-linux.tar.gz` (`dsc`, `README.md`, `LICENSE`) with a `.sha256`, publishes a release with generated notes |

Version: `.githooks/pre-commit` (enabled per clone with `git config core.hooksPath .githooks`) raises the patch
number in `Cargo.toml`, `Cargo.lock` and the README sample in every commit that changes a file under `src/`, and
stages them; a commit that already changes the `version` line is left alone (minor or major bumps are made by
hand). So a release needs no version edit: tag what `Cargo.toml` says.

A push starts no workflow: every push used to run `ci`, and the runs piled up in GitHub's queue (2026-11-11).
Run the gates locally before a push. Release steps: run the gates locally, push, then tag the version of `Cargo.toml`:
`v=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1); git tag v$v && git push origin v$v`; the
release workflow runs the gates itself before it publishes. The asset name is fixed, because the README
download command uses `releases/latest/download/dsc-x86_64-linux.tar.gz`. A tag is published work: only on the
user's word. Run state without a login: `https://api.github.com/repos/GeorgII-web/deep_seek_cli_free/actions/runs`.

## Public repo

- Remote `origin` = `github.com/GeorgII-web/deep_seek_cli_free`, branch `main`, MIT.
- Commits use the repo-local GitHub noreply email; never another address.
- No personal paths, names, tokens or data in tracked files; work files go to git-ignored `var/`.
- A code change, its skills, `AGENTS.md` and `README.md` go in the same commit, pushed right away.
- Look at `git status --short` before every commit and stage what belongs to the change. A blind `git add -A`
  once committed a downloaded 8 MB binary that lay in the repo folder (2026-11-12); `/dsc` is ignored since then.
