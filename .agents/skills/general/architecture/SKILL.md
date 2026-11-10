---
name: architecture
description: Architecture of deep_seek_cli_free (binary `dsc`) — the layers (CLI, agent loop, tools and approval policy, `Backend` trait, DeepSeek web client), the DeepSeek web protocol (auth token and cookie, proof of work, chat session, completion SSE), the text protocol that emulates tool calls, login methods, file locations and the decisions behind them. Use when orienting, adding a module, changing the protocol, the approval rules or the login.
---

# Architecture

State: the agent works live end to end (`dsc "<task>"`); `chat`, transcripts and the browser login are design (`TASKS.md`).

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

## DeepSeek web protocol (reference: `~/projects/dsfree/FreeDeepseekAPI/client.js`, `server.js`)

Base `https://chat.deepseek.com`. Headers on every call: `Authorization: Bearer <token>`, `Cookie`,
`x-client-platform: web`, `x-client-version` and `x-app-version` (`2.0.0`), `x-client-locale`,
`x-client-timezone-offset`, `Origin`, `Referer`, a normal Chrome `User-Agent`.

| Step | Call | Notes |
|---|---|---|
| 1 | `POST /api/v0/chat_session/create` `{}` | once per conversation → session id |
| 2 | `POST /api/v0/chat/create_pow_challenge` `{"target_path":"/api/v0/chat/completion"}` | per turn → `algorithm, challenge, salt, difficulty, expire_at, signature` |
| 3 | solve (`pow::solve`: own thread with a 64 MB stack; `wasmi` with feature `portable-dispatch`, because its default tail-call dispatch overflows the stack in debug builds; worst case 1.8 s release, 3.6 s debug with dependencies at `opt-level = 3`) | wasm export `wasm_solve(sp, challenge, len, prefix, len, difficulty)`, prefix = `<salt>_<expire_at>_`; answer = f64 at `sp+8` |
| 4 | `POST /api/v0/chat/completion` + header `X-DS-PoW-Response` = base64 of JSON `{algorithm, challenge, salt, answer, signature, target_path}` | body: `chat_session_id, parent_message_id, model_type, prompt, ref_file_ids: [], thinking_enabled, search_enabled` |
| 5 | SSE, see below | the `ready` event carries `response_message_id` = the next `parent_message_id` |

SSE (`stream::Parser`, recorded samples in `src/deepseek/fixtures/`): `event: <name>` applies to the next
`data:` line; events `ready`, `update_session`, `title`, `close`; data lines without an event are patches to one
`response` object: `p` = path (kept until the next `p`), `o` = operation (`APPEND`, `SET`, `BATCH`), `v` = value.

| Patch | Meaning |
|---|---|
| no `p`, `v.response` | the whole object at the start: `status: WIP`, `fragments: [{id, type, content}]` |
| `response/fragments/-1/content` + `APPEND`, then bare `{"v":"text"}` | text appended to the last fragment |
| `response/fragments` with a list | repeats the old fragments and adds new ones (thinking → answer); only unseen ids count |
| `response` + `BATCH`, `response/status` + `SET` | `FINISHED` at the end; anything else at the end of the body is an error |

Fragment `type`: `THINK` → `Event::Thinking`, `RESPONSE` → `Event::Text`, others (search) are dropped.

Verified live on 2026-11-10: the token alone is enough (no cookie); plain and thinking answers parse. A second turn in
one conversation works (agent run). Not yet verified live: search mode, the `expert` model.

`model_type`: `default` (fast), `expert`; `thinking_enabled` and `search_enabled` are flags on top.
Answers of the JSON calls are an envelope `{code, msg, data: {biz_code, biz_msg, biz_data}}` (`client::biz_data`):
`code` 40002/40003 or HTTP 401/403 = `ApiError::TokenDead` → `dsc login` (seen live: HTTP 200 with
`40003 Authorization Failed (invalid token)`); HTTP 429 = `ApiError::RateLimited`. A completion error also comes
as this envelope instead of a stream. The static wasm host is called without the token.

## Throttle (`deepseek::throttle`, config `[throttle]`)

Every completion call goes through `throttle::acquire` (in `Conversation::send`); `doctor` calls are not counted.

| Key | Default | Meaning |
|---|---|---|
| `min_interval_ms` | 3000 | minimum gap between two requests |
| `jitter_ms` | 2000 | random extra gap, 0..=value |
| `max_per_hour` | 120 | requests in any 60 minutes, 0 = no cap; when reached the CLI waits and says so |
| `retries_on_429` | 2 | retries after HTTP 429 |
| `wait_on_429_secs` | 60 | wait before such a retry when there is no `Retry-After` |

Send times live in `<home>/cache/throttle.json`, so the limits also hold across separate `dsc` runs.

## Auth

`<home>/auth.json` (0600): `token` (the site's `localStorage.userToken.value`), `cookie`, `wasm_url`.

| `dsc login` mode | How |
|---|---|
| `--import <file>` | reads FreeDeepseekAPI's `deepseek-auth.json` (`token`, `cookie`, `wasmUrl`) |
| `--token` | token pasted on a hidden prompt, cookie optional |
| (default) | starts the system Chrome with `--remote-debugging-port` and profile `<home>/chrome-profile`, the user logs in, the CLI reads `userToken` and cookies over CDP, closes it; needs WSLg or a desktop |

## Agent protocol (`agent::protocol`, `agent::run`)

First message of a conversation = `first_prompt`: role, the action format, the tools, the rules (one action per
answer, never invent the result, no interactive commands, prefer read-only, plain text when done), the working
directory, the task. The model answers either plain text (final answer, the loop ends) or with a block:

````
```dsc
{"tool":"shell","cmd":"ls -la","why":"see the files"}
```
````

| Tool | Fields | Result message |
|---|---|---|
| `shell` | `cmd`, `timeout_s` (optional, capped by `agent.command_timeout_secs`), `why` | `Result: exit code N` / `timed out, killed` / `killed by a signal`, then `--- stdout ---`, `--- stderr ---` or `(no output)` |
| `read_file` | `path`, `why` | `--- <path> ---` + content |
| `write_file` | `path`, `content` (whole file), `why` | `Written: <path> (N bytes)` |

- `parse`: lines between a `` ```dsc `` line and a `` ``` `` line; an unclosed block counts. The **first** block
  runs; further blocks are ignored and the model is told so. Broken JSON, an unknown tool or field → `Invalid`,
  its text goes back to the model.
- Output is capped per stream to `agent.max_output_bytes` (`tools::cap`: head + tail, cut on character borders).
- `tools::shell`: `bash -c`, stdin closed, own process group; timeout or Ctrl-C kills the group; output of a
  finished command is collected for 0.5 s more at most (a background child may hold the pipe).
- The loop ends on a final answer, after `agent.max_steps` answers, or on Ctrl-C while waiting for the model
  (Ctrl-C while a command runs only kills the command and tells the model).
- The loop knows only the `Backend` and `Ui` traits; `run.rs` tests script both.

## Approval policy (`agent::policy`)

`check` order: deny list → `--yes` → allowed "always" in this run → allow list → ask.

| Rule | Detail |
|---|---|
| `policy.deny` | substring of the command or file path (whitespace squeezed); never runs, also with `--yes` and after an edit; the model is told not to retry |
| `policy.allow` | a program or a program with its first arguments (`git status`), matched on whole words; only for a simple command, i.e. none of `; & | > < $ ( ) { }` backtick or newline |
| files | `read_file` and `write_file` always ask without `--yes` |
| ask (`ui::Terminal::approve`) | `y` once, `a` always in this run (the program of a simple command, or the tool; not offered for compound commands), `n` no (the model may try another way), `e` edit the command, then only the deny list is checked again |
| no terminal on stdin | a question counts as no |

Defaults: allow = `ls pwd df du uname whoami date which stat wc file`, `git status`; nothing that prints file
content, because every result is sent to DeepSeek. Deny = destructive patterns plus `.ssh/id_`,
`.aws/credentials`, `dsc/auth.json`. The deny list is a plain substring guard, not a sandbox.

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
