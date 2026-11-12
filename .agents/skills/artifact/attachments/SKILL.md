---
name: attachments
description: Files attached to a prompt by dropping them on the chat box — `src/attach.rs` (paste → paths → `Attachment`, private-file warning, mime type, limits), the box and echo display, the confirm question for flagged files, `Backend::attach`, the DeepSeek upload flow (`upload_file`, `fetch_files`, `ref_file_ids`) and how both mocks play it. Use when a dropped file is not attached, the warning flags too much or too little, an upload or the reading step fails, or the upload API changes.
---

# Attachments

A file dropped on a terminal is not sent to the app as a file: the terminal **pastes its path** (bracketed
paste). Windows Terminal with a WSL profile pastes the Linux path (`pathTranslationStyle: wsl`), quoted when it has
blanks, several paths separated by blanks; GNOME-like terminals paste `'/path'` or `file:///…` URIs.

## From a paste to attachments (`src/attach.rs`)

| Piece | What it does |
|---|---|
| `from_paste(text)` | `Some(Dropped)` only when **every** word is an absolute path, a `file://` URI or a Windows path (`C:\…` → `/mnt/c/…`) of an existing regular file; anything else (text, a relative name, a folder, a missing file) is `None` and the paste goes into the box as text. Pastes over 64 KB are text |
| `split_words` | shell-like: blanks separate, `'…'` / `"…"` group, `\` escapes; a word starting with `C:\` keeps its backslashes |
| `load(path)` | `Attachment { path, name, size, warning }`; an error text for an empty file or one over `MAX_BYTES` (100 MB); reads the first 64 KB for `warning` |
| `warning(path, head)` | why a file looks private, `None` = nothing found. Folders `.ssh .gnupg .aws .kube .docker gcloud .password-store`; names `.env`, `.env.*`, `.netrc`, `.pgpass`, `.git-credentials`, `.npmrc`, `.pypirc`, `.htpasswd`, `credentials`, `shadow`, `auth.json`, SSH keys `id_rsa/id_dsa/id_ecdsa/id_ed25519*` but not `*.pub`; extensions `pem key p12 pfx jks keystore kdbx ppk ovpn`; names containing `secret password passwd credential private token`; content: a `PRIVATE KEY` block, token shapes (`AKIA…`, `ghp_`, `github_pat_`, `xoxb-`, `xoxp-`, `sk-…`), and `password/passwd/pwd/secret/api_key/apikey/access_key/token/private_key` followed by `:` or `=` and a value (not `<…>`, `${…}`, `{`, empty) |
| `mime(name)` | the content type of the upload by extension; `application/octet-stream` otherwise |
| `size_label`, `Attachment::label` | `512 B`, `12 KB`, `3.4 MB`; `name · size` |
| `MAX_FILES` (50), `MAX_BYTES` (100 MB) | the limits the DeepSeek web page shows (not verified against the API) |

A warning never blocks: the user decides (below).

## In the box and the output (`src/ui/editor.rs`, `src/ui/mod.rs`)

- `Editor::read` (chat prompt only, `remember = true`): `Event::Paste` → `from_paste` → `add_files` (no path twice,
  at most `MAX_FILES`; what was left out becomes `note`, shown in the hint line until the next key).
- The files sit in the box's top border (`files_title` in `box_lines`): `╭─ 📎 a.pdf · 12 KB  ⚠ id_rsa · 2 KB ───╮`,
  flagged ones in the action color; when they do not fit, `📎 N files · total · ⚠ K private`.
- Backspace in an empty box drops the last file (hint `Backspace drops a file`); Esc and the first Ctrl-C clear
  text and files; Enter sends files without text too.
- On Enter the files move from `files` to `sent` before `erase` draws the empty box, so the box shows them no
  longer while the task runs; `Terminal::input` takes them (`Editor::take_files`). A slash command takes no files: they are
  dropped with a note. The echo shows one row per file on the prompt bar (`📎` / `⚠ name · size`), after an empty bar row when there is text.
- `Terminal::confirm_files`: with a flagged file, each warning is printed (`⚠ <name> looks private: <why>`) and
  `Send to DeepSeek? y yes · s skip the flagged · n cancel` asks (`s` only when unflagged files are there too);
  `n` or Esc cancels the whole prompt (`input` returns an empty line, nothing is sent).
- `Terminal::take_files` hands them to `chat()` in `src/main.rs`: files without text get the task
  `FILES_ONLY_TASK`; `protocol::with_files` puts `Attached files (…): a, b` right before the `Task:` line, so the
  model does not look for them on the machine; `Backend::attach(files)` before every `run` (again on a rerun
  after a new login).

## Upload (`Backend::attach`, `deepseek::session`, `deepseek::client`)

`Backend::attach(files)` replaces the files for the next `ask` only (default: ignored, as in the loop tests).
`Conversation::ask` calls `upload_pending` first, which takes the pending files:

| Step | Call | Notes |
|---|---|---|
| 1 | `POST /api/v0/chat/create_pow_challenge` `{"target_path":"/api/v0/file/upload_file"}` | a proof of work **per file**, for the upload path (`pow_header(UPLOAD_PATH)`); the header's `target_path` names it |
| 2 | `POST /api/v0/file/upload_file`, multipart field `file` (file name, `mime`), header `x-ds-pow-response` | envelope `biz_data` → `id` (also `file_id`, `file.id`: `uploaded_id`), status `PENDING` |
| 3 | `GET /api/v0/file/fetch_files?file_ids=<id>&file_ids=…` every `POLL_EVERY` (2 s), at most `READ_TIMEOUT` (180 s) | `biz_data.files[]` (also a bare list or a map by id: `remote_files`) with `status`: `SUCCESS` = read; `FAIL`/`ERROR` in it = failure with `error_reason` / `error_code` |
| 4 | completion with `ref_file_ids: [ids]` | the 429 retry of the completion reuses the ids |

Progress lines: `Uploading <name · size>`, `DeepSeek is reading <names>`. Errors: `DeepSeek could not read <name>
(<why>)`, `DeepSeek did not finish reading <names> in 180 s`, the envelope's `biz_code` message of a refused upload.
Uploads are not prompts: they take no place in the throttle (`requests()` does not count them), but a 429 on an
upload or a poll sets the same cooldown as one on a prompt (`pause_on_429`).

Source of the flow: two open-source clients of the web API (2026-11), not yet a recording of the browser and not
yet verified live. The browser may send extra headers on the upload (`x-file-size`, `x-thinking-enabled`,
`x-model-type`); if the real API refuses the upload, record one upload in the browser (DevTools → Network) and
compare. Whether images are read as text (OCR) or seen by the model depends on DeepSeek.

## Mocks

- `scripts/mock-deepseek.py`: `upload_file` (checks that the proof of work names the upload path, reads the
  multipart field `file`), `fetch_files` (`PENDING` twice, then `SUCCESS`), a completion refuses unknown or unread
  `ref_file_ids` and checks its own proof-of-work path; the answer starts with `Mock read N attached file(s): name
  (size, first line)`. Names steer it: `reject` = refused at upload, `unreadable` = `PARSE_FAILED`, `slowread` =
  6 polls. `/_stats` counts `uploads`, `polls` and shows the last `ref_file_ids`.
- `mock::Mock` (`/model`, no network): same progress lines and answer start, `reject` and `unreadable` the same.
- Drop in `scripts/pty-drive.py`: send the path as a bracketed paste, e.g.
  `'[("\x1b[200~/abs/path/a.txt\x1b[201~", 1), ("what is it\r", 6)]'`.
