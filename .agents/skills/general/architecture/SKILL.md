---
name: architecture
description: Map of deep_seek_cli_free (binary `dsc`) — the design decisions and their reasons, the layers from CLI to the DeepSeek web client, the `Backend` and `Ui` traits that keep the agent testable, the runtime files under `~/.config/dsc`, the risks, and which skill covers which module. Use when orienting, adding a module, moving code between layers or changing a design decision.
---

# Architecture

Entry point `src/main.rs`. State: single tasks and the interactive chat work live; transcripts with resume and the
browser login are design (`TASKS.md`).

## Decisions

| Topic | Decision | Why |
|---|---|---|
| Transport | direct HTTP to the web API of chat.deepseek.com (`reqwest`), not a driven browser | one binary, fast, real markdown source and stream; aiproxy's Playwright sidecar needs node + Chromium and reads the rendered DOM |
| Proof of work | run the site's own `sha3_wasm_bg.*.wasm` in an embedded wasm runtime (`wasmi`), file cached in `<home>/cache/` | the hash is a custom variant; the site's wasm stays correct when they change it |
| Browser | only for `dsc login`, optional: system Chrome/Chromium over CDP with an own profile | token lives months; import or paste works without any browser |
| Tool calls | emulated in text: the model answers with one fenced action block | the web chat has no function calling and no system role |
| History | kept by DeepSeek (`chat_session_id` + `parent_message_id`); each turn sends only the new text | no resend of the whole context |
| Safety | every action passes `policy`; default ask | a free web model runs shell on a real machine |
| Fallback | `Backend` trait; a browser backend can be added if the HTTP path gets blocked | the web API is unofficial and may change |

## Layers

```
cli  →  agent::run (loop)  →  Backend trait  →  deepseek::{client, pow, stream, session}
              │
              ├─ agent::protocol   prompt text, parser of the model's action block
              ├─ agent::policy     allow / ask / deny per action
              ├─ agent::tools      shell, read_file, write_file
              └─ ui                stream print, approval prompt
```

`Backend` (`src/backend.rs`): `async fn ask(&mut self, prompt, sink: &mut dyn FnMut(&Event)) -> Result<String>`;
`Event` = `Thinking(String) | Text(String)`. One `Backend` value = one conversation; the returned string is the
answer without the reasoning.

`Ui` (`src/agent/run.rs`): what the loop shows and asks; `ui::Terminal` is the real one. The loop knows only the
two traits, so its tests script both.

| Module | Skill |
|---|---|
| `src/main.rs`, `src/cli.rs`, `src/config.rs`, `src/auth.rs` | `../../artifact/cli-config-auth/SKILL.md` |
| `src/backend.rs`, `src/deepseek/*` | `../../artifact/deepseek-client/SKILL.md` |
| `src/agent/protocol.rs`, `run.rs`, `tools.rs`, `src/ui.rs` | `../../artifact/agent-loop/SKILL.md` |
| `src/agent/policy.rs`, the approval question | `../../artifact/approval-policy/SKILL.md` |
| gates, live checks, release, publishing | `../../quality-gate/build-run-check/SKILL.md` |

## Files

| Path | Content |
|---|---|
| `<home>` | `$DSC_HOME`, else `~/.config/dsc` |
| `<home>/config.toml` | `deepseek.*` (model type, thinking, search, locale, timeout), `throttle.*`, `agent.*`, `policy.*` |
| `<home>/auth.json` | secret, 0600 |
| `<home>/cache/sha3.wasm`, `sha3.url` | proof-of-work module and the URL it came from; refetched when `wasm_url` differs |
| `<home>/cache/throttle.json` | send times of the last hour |
| `<home>/sessions/<id>.jsonl` | planned: transcript of each run for `dsc chat --resume` |

## Risks

- Unofficial API: headers, wasm URL and SSE shape change without notice → all of it sits in `src/deepseek/`,
  `dsc doctor` checks each step.
- The account can be rate limited or blocked by DeepSeek for automated use.
- The model can ignore the format or loop: step limit, format reminder on a parse error.
