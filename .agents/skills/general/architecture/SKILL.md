---
name: architecture
description: Architecture of deep_seek_cli_free (binary `dsc`) — the layers (CLI, agent loop, tools and approval policy, `Backend` trait, DeepSeek web client), the DeepSeek web protocol (auth token and cookie, proof of work, chat session, completion SSE), the text protocol that emulates tool calls, login methods, file locations and the decisions behind them. Use when orienting, adding a module, changing the protocol, the approval rules or the login.
---

# Architecture

State: design, code not written yet (`TASKS.md`). Update this file with every change.

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

`Backend`: `async fn send(&mut self, text: &str, opts: &TurnOpts) -> Result<EventStream>`;
`Event` = `Thinking(String) | Text(String) | Done | Error`. One `Backend` value = one conversation.

## DeepSeek web protocol (reference: `~/projects/dsfree/FreeDeepseekAPI/client.js`, `server.js`)

Base `https://chat.deepseek.com`. Headers on every call: `Authorization: Bearer <token>`, `Cookie`,
`x-client-platform: web`, `x-client-version` and `x-app-version` (`2.0.0`), `x-client-locale`,
`x-client-timezone-offset`, `Origin`, `Referer`, a normal Chrome `User-Agent`.

| Step | Call | Notes |
|---|---|---|
| 1 | `POST /api/v0/chat_session/create` `{}` | once per conversation → session id |
| 2 | `POST /api/v0/chat/create_pow_challenge` `{"target_path":"/api/v0/chat/completion"}` | per turn → `algorithm, challenge, salt, difficulty, expire_at, signature` |
| 3 | solve | wasm export `wasm_solve(sp, challenge, len, prefix, len, difficulty)`, prefix = `<salt>_<expire_at>_`; answer = f64 at `sp+8` |
| 4 | `POST /api/v0/chat/completion` + header `X-DS-PoW-Response` = base64 of JSON `{algorithm, challenge, salt, answer, signature, target_path}` | body: `chat_session_id, parent_message_id, model_type, prompt, ref_file_ids: [], thinking_enabled, search_enabled` |
| 5 | SSE `data: {..}` | JSON patches: `p` = path (sticky until the next `p`), `v` = value; `response/content` = answer text, `response/thinking_content` = reasoning; the response message id becomes the next `parent_message_id` |

`model_type`: `default` (fast), `expert`; `thinking_enabled` and `search_enabled` are flags on top.
HTTP 401/403 = token dead → `dsc login`; 429 → honor `Retry-After`, else wait and retry once.

## Auth

`<home>/auth.json` (0600): `token` (the site's `localStorage.userToken.value`), `cookie`, `wasm_url`.

| `dsc login` mode | How |
|---|---|
| `--import <file>` | reads FreeDeepseekAPI's `deepseek-auth.json` (`token`, `cookie`, `wasmUrl`) |
| `--token` | token pasted on a hidden prompt, cookie optional |
| (default) | starts the system Chrome with `--remote-debugging-port` and profile `<home>/chrome-profile`, the user logs in, the CLI reads `userToken` and cookies over CDP, closes it; needs WSLg or a desktop |

## Agent protocol

First message of a conversation = instructions (role, OS facts, cwd, the action format, "one action per answer,
then stop") + the user's task. The model answers either plain text (final answer) or ends with exactly one block:

````
```dsc
{"tool":"shell","cmd":"ls -la","why":"see the files"}
```
````

Tools: `shell` (`cmd`, `timeout_s`), `read_file` (`path`), `write_file` (`path`, `content`). The parser takes
the last `dsc` fence; broken JSON → the error text goes back to the model. The result goes back as the next
message: exit code, stdout and stderr, each capped (head + tail) by `agent.max_output_bytes`. The loop ends on
an answer without a block, `agent.max_steps`, or Ctrl-C (first = stop the running command, second = quit).

## Approval policy (`agent::policy`)

Order: deny list → `--yes` or allow list → ask. Ask shows the command and `why`: `y` once, `a` always for this
program in the run, `n` no (the model gets "denied by user" and may try another way), `e` edit before running.
Config: `policy.allow` (program names run without asking, e.g. `ls`, `cat`, `rg`, `git status`), `policy.deny`
(patterns never run, even with `--yes`). `shell` runs `bash -lc` in the cwd of the CLI with a timeout and its
own process group (killed as a tree).

## Files

| Path | Content |
|---|---|
| `<home>` | `$DSC_HOME`, else `~/.config/dsc` |
| `<home>/config.toml` | model type, thinking, search, locale, `agent.*`, `policy.*` |
| `<home>/auth.json` | secret, 0600 |
| `<home>/cache/sha3.wasm` | proof-of-work module, refetched when `wasm_url` changes |
| `<home>/sessions/<id>.jsonl` | transcript of each run (prompts, actions, results) for `dsc chat --resume` |

## Risks

- Unofficial API: headers, wasm URL and SSE shape change without notice → all of it sits in `src/deepseek/`,
  `dsc doctor` checks each step.
- The account can be rate limited or blocked by DeepSeek for automated use.
- The model can ignore the format or loop: step limit, format reminder on a parse error.
