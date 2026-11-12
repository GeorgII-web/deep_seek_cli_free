---
name: architecture
description: Map of deep_seek_cli_free (binary `dsc`) — the design decisions and their reasons, the layers from the chat loop to the model (real DeepSeek or the mock) and to the terminal UI, the `Backend` and `Ui` traits that keep the agent testable, the runtime files under `~/.config/deep_seek_cli_free`, the risks, and which skill covers which module. Use when orienting, adding a module, moving code between layers or changing a design decision.
---

# Architecture

Entry point `src/main.rs`. State (v0.2.0): single tasks and the chat (own screen, docked input box, scrollback
with foldable action output, sessions, token window, real or mock model, deep thinking, DeepSeek's web search)
work; a login through the browser is not built (`TASKS.md`). Not verified against the real API since the mock
exists: web search and thinking streams, the `expert` model.

## Decisions

| Topic | Decision | Why |
|---|---|---|
| Transport | direct HTTP to the web API of chat.deepseek.com (`reqwest`), not a driven browser | one binary, fast, real markdown source and stream; aiproxy's Playwright sidecar needs node + Chromium and reads the rendered DOM |
| Proof of work | run the site's own `sha3_wasm_bg.*.wasm` in an embedded wasm runtime (`wasmi`), file cached in `<home>/cache/` | the hash is a custom variant; the site's wasm stays correct when they change it |
| Browser | none: the token is pasted into the token window (or read from a file); a CDP login is an open task | a token works for a while; no browser automation in the binary |
| Web pages | DeepSeek's own search by default; `curl` on the user's machine only when asked or for local addresses | the user's machine does not fetch what the model can read itself |
| Tests | never the real API: scripted model, built-in mock, local HTTP mock; `client::base()` refuses the real address in tests and with a custom `DSC_HOME` | the free account was blocked by test traffic and by 25 requests an hour |
| Throttle | 30 to 45 s between requests, 2 a minute, 12 an hour, growing pauses after a rate limit | the account is blocked quickly |
| Screen | the chat runs on the alternate screen and keeps its own output (`ui::scrollback::View`) | the input box stays at the bottom while only the output scrolls, folds and can be selected |
| Tool calls | emulated in text: the model answers with one fenced action block | the web chat has no function calling and no system role |
| History | kept by DeepSeek (`chat_session_id` + `parent_message_id`); each turn sends only the new text | no resend of the whole context |
| Safety | every action passes `policy`; default ask | a free web model runs shell on a real machine |
| Fallback | `Backend` trait; a browser backend can be added if the HTTP path gets blocked | the web API is unofficial and may change |

## Layers

```
main (cli, chat loop)  →  agent::run (loop)  →  Backend trait  →  model::Model
        │                       │                                   ├─ deepseek::{client, pow, stream, session, throttle}
        │                       │                                   └─ mock::Mock (no network)
        │                       ├─ agent::protocol    prompt text, parser of the model's action block
        │                       ├─ agent::policy      allow / ask / deny per action
        │                       ├─ agent::tools       shell (live output), read_file, write_file
        │                       ├─ agent::interrupt   Esc = stop the task, Ctrl-C twice = quit
        │                       └─ Ui trait  →  ui::Terminal
        │                                         ├─ ui::scrollback   the output: scroll, fold, select
        │                                         ├─ ui::editor       the input box, command menu
        │                                         ├─ ui::dialog       token window, list window
        │                                         └─ ui::spinner, render, style, banner, clipboard
        └─ config, auth, sessions (files under <home>)
```

`Backend` (`src/backend.rs`): `async fn ask(&mut self, prompt, sink: &mut dyn FnMut(&Event)) -> Result<String>`;
`Event` = `Thinking(String) | Text(String) | Status(String)` (progress label). One `Backend` value = one conversation; the returned string is the
answer without the reasoning. `fn attach(&mut self, files: Vec<Attachment>)` gives files to the next `ask` only
(default: ignored); `Conversation` uploads them, `Mock` pretends to.

`Ui` (`src/agent/run.rs`): what the loop shows and asks; `ui::Terminal` is the real one. The loop knows only the
two traits, so its tests script both.

| Module | Skill |
|---|---|
| `src/main.rs`, `src/cli.rs`, `src/config.rs`, `src/auth.rs` | `../../artifact/cli-config-auth/SKILL.md` |
| `src/backend.rs`, `src/deepseek/*` | `../../artifact/deepseek-client/SKILL.md` |
| `src/model.rs`, `src/mock.rs`, `src/sessions.rs` | `../../artifact/cli-config-auth/SKILL.md` |
| `src/agent/protocol.rs`, `run.rs`, `tools.rs` | `../../artifact/agent-loop/SKILL.md` |
| `src/ui/mod.rs`, `spinner.rs`, `render.rs`, `style.rs`, `banner.rs` | `../../artifact/terminal-ui/SKILL.md` |
| `src/ui/scrollback.rs` | `../../artifact/scrollback/SKILL.md` |
| `src/ui/editor.rs`, `dialog.rs`, `clipboard.rs` | `../../artifact/input-box/SKILL.md` |
| `src/attach.rs`, dropped files from the box to the upload | `../../artifact/attachments/SKILL.md` |
| `src/agent/interrupt.rs` | `../../artifact/agent-loop/SKILL.md` |
| `src/agent/policy.rs`, the approval question | `../../artifact/approval-policy/SKILL.md` |
| gates, live checks, release, publishing | `../../quality-gate/build-run-check/SKILL.md` |

## Files

| Path | Content |
|---|---|
| `<home>` | `$DSC_HOME`, else `~/.config/deep_seek_cli_free` |
| `<home>/config.toml` | `version`, `deepseek.*` (`api` real or mock, model type, thinking, search, locale, timeout), `throttle.*`, `agent.*`, `ui.*`, `mock.*`, `policy.*` (the saved mode, allow, deny); rewritten when the mode, the model or thinking is switched |
| `<home>/auth.json` | secret, 0600 |
| `<home>/history.json` | the last 200 chat inputs |
| `<home>/sessions.json` | the last 50 conversations started here: remote session id, last answer id, title, folder |
| `<home>/sessions/<id>.log` | the screen output of a conversation with its foldable blocks (JSON), shown again after a switch |
| `<home>/cache/sha3.wasm`, `sha3.url` | proof-of-work module and the URL it came from; refetched when `wasm_url` differs |
| `<home>/cache/throttle.json` | send times of the last hour |

## Risks

- Unofficial API: headers, wasm URL and SSE shape change without notice → all of it sits in `src/deepseek/`,
  `dsc doctor` checks each step.
- The account can be rate limited or blocked by DeepSeek for automated use.
- The model can ignore the format or loop: step limit, format reminder on a parse error.
