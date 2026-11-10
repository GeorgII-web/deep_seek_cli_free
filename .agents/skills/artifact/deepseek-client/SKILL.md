---
name: deepseek-client
description: The DeepSeek web backend in `src/deepseek/` — headers and calls of `client.rs`, the JSON envelope and `ApiError`, proof of work in `pow.rs` (site wasm in `wasmi`), the SSE patch format parsed by `stream.rs`, `Conversation` in `session.rs` (session id, message chain, `impl Backend`) and the request throttle. Use when a DeepSeek call fails, the site changed its API or stream, answers come back empty or cut, or limits must change.
---

# DeepSeek client

Everything specific to chat.deepseek.com lives in `src/deepseek/`; the rest of the code sees only the `Backend`
trait (`src/backend.rs`), implemented by `session::Conversation`. Reference for the protocol: `client.js`,
`server.js` of github.com/ForgetMeAI/FreeDeepseekAPI.

| File | Content |
|---|---|
| `client.rs` | `Client` (`create_session`, `create_challenge`, `completion`, `fetch_wasm`), `Challenge`, `TurnOpts`, `ApiError`, `biz_data`; constants `BASE`, `COMPLETION_PATH`, `DEFAULT_WASM_URL`, `CLIENT_VERSION`, `USER_AGENT` |
| `pow.rs` | `solve`, `header` |
| `stream.rs` | `Parser` (`feed`, `finish`, `response_message_id`) |
| `session.rs` | `Conversation` (`wasm`, `pow_header`, `send`, `send_once`, `ask`); reports progress to the sink as `Event::Status` ("Pausing N s (throttle)" for a wait of a second or more, "Opening conversation", "Solving proof of work", "Waiting for DeepSeek", "Rate limited by DeepSeek, retry in N s") |
| `throttle.rs` | `delay_ms` and `window_wait` (pure rules), `acquire` (calls `on_wait` before it sleeps), `cooldown` |
| `fixtures/` | recorded SSE bodies used by the parser tests |

## Calls

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

## Throttle (`throttle.rs`, config `[throttle]`)

Every completion call goes through `throttle::acquire` (in `Conversation::send`); `doctor` calls are not counted.

| Key | Default | Meaning |
|---|---|---|
| `min_interval_ms` | 6000 | minimum gap between two requests |
| `jitter_ms` | 4000 | random extra gap, 0..=value |
| `max_per_minute` | 5 | requests in any 60 seconds, 0 = no cap |
| `max_per_hour` | 60 | requests in any 60 minutes, 0 = no cap; when a cap is reached the CLI waits and shows it in the progress line |
| `retries_on_429` | 2 | retries after HTTP 429 |
| `wait_on_429_secs` | 120 | pause before the first such retry without `Retry-After`; doubled for each further one |

State in `<home>/cache/throttle.json` (`State`: `sent` = send times of the last hour, `cooldown_until`), so the
limits also hold across separate `dsc` runs; the first versions' plain list is still read. Every 429 calls
`throttle::cooldown`, also the last one that fails the task: the next request, even of a new run, waits it out.
`delay_ms` = the largest of the gap and the two window waits (`window_wait`).

The first defaults (3 s gap, 120 per hour) got the account blocked within hours (2026-11-10).
`Settings::slow_down` replaces a config that still holds exactly those values with the current defaults.

## When the site changes

- `dsc doctor` shows which step fails (token, wasm, proof of work) without sending a prompt.
- New stream shape: record a body (see `../../quality-gate/build-run-check/SKILL.md`), add it to `fixtures/`
  with a test, then change `Parser::on_patch`.
- New wasm file name: `auth.json` `wasm_url`, else `DEFAULT_WASM_URL`; the allocator export
  `__wbindgen_export_0` may be renamed with it (`pow::put`).
- Client version headers: `CLIENT_VERSION` (the `expert` model needs a current one).
