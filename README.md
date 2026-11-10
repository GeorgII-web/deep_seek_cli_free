# deep_seek_cli_free

`dsc` is a command line agent for Linux. You give it a task in plain words; it asks DeepSeek what to run, runs the
commands on your machine after your approval, and answers.

It uses your **free DeepSeek web account** (chat.deepseek.com), not the paid API. One Rust binary, no node, no
browser at runtime: the browser is needed once, to log in.

```console
$ dsc "how much disk space is free on the root filesystem?"

● Run df -h /
  check free disk space on root filesystem
  ⎿ exit code 0
    Filesystem      Size  Used Avail Use% Mounted on
    /dev/sdd       1007G  146G  810G  16% /

Root filesystem: 810G available (1007G total, 16% used).
✓ 8.1s · 2 answers · 1 action
```

> **Unofficial.** This talks to the private web API of chat.deepseek.com. It can break when the site changes, and
> DeepSeek may rate limit or block an account for automated use. Use your own account, at your own risk.

## Status

Early, works end to end. Done: login by token, direct HTTP client, proof of work, streamed answers, reasoning
mode, the agent loop with approvals, interactive chat with a multiline input box, request throttling. Open:
resume of a chat after a restart, login through the browser. See [TASKS.md](TASKS.md).

## Install

Needs a Rust toolchain (edition 2024, Rust 1.88 or newer) and a C compiler.

```bash
git clone https://github.com/GeorgII-web/deep_seek_cli_free.git
cd deep_seek_cli_free
cargo install --path .        # installs `dsc` into ~/.cargo/bin
```

## Log in

1. Open <https://chat.deepseek.com> in your browser and log in.
2. Open DevTools → Console and run:
   ```js
   copy(JSON.parse(localStorage.getItem('userToken')).value)
   ```
   This copies your login token to the clipboard. (Chrome asks you to type `allow pasting` first.)
3. Run `dsc login --token`, paste the token at the hidden prompt, press Enter at the cookie prompt.
4. Run `dsc doctor`:
   ```console
   token     ok (challenge DeepSeekHashV1, difficulty 144000)
   wasm      ok (26612 bytes)
   pow       ok (870 ms)
   ```

The token is stored in `~/.config/dsc/auth.json` with mode 0600. Treat it like a password: it gives full access
to your DeepSeek account. `dsc logout` removes it. When `doctor` says `token dead`, repeat the login.

`dsc login --import <file>` reads the `deepseek-auth.json` of
[FreeDeepseekAPI](https://github.com/ForgetMeAI/FreeDeepseekAPI) instead.

## Use

```bash
dsc "which process uses the most memory?"
dsc --think "why does cargo build fail here?"    # reasoning mode
dsc --yes "create a .gitignore for a Rust project"
```

### Chat

`dsc` without a task (or `dsc chat`) opens a chat. Every input is a task in the same conversation, so the model
remembers what it did before:

```console
$ dsc
✻ dsc 0.1.0 · DeepSeek web · default

> how much disk space is free on / ?

● Run df -h /
  check free disk space on /
  ⎿ exit code 0
    Filesystem      Size  Used Avail Use% Mounted on
    /dev/sdd       1007G  147G  809G  16% /

There is 809G free on / (out of 1007G, 16% used).
✓ 8.1s · 2 answers · 1 action

╭──────────────────────────────────────────────────────────────╮
│ > which command did you run for that?                        │
╰──────────────────────────────────────────────────────────────╯
  Enter send · Alt+Enter or \ + Enter new line · ↑↓ history · /help · Ctrl-D quit
```

While `dsc` waits, a progress line shows what for and for how long (`⠹ Waiting for DeepSeek… 3s`). The last line
of a task shows the seconds it took.

| Key in the input box | Effect |
|---|---|
| Enter | send |
| Alt+Enter, Ctrl-J, or `\` then Enter | new line |
| Up / Down | move in the text, then through earlier inputs |
| Ctrl-A, Ctrl-E, Ctrl-W, Ctrl-U, Ctrl-K | start, end, delete word, delete to start, delete to end |
| Ctrl-C | clear the box (quit when it is empty) |
| Ctrl-D | quit |

Pasting several lines works. `/new` starts a fresh conversation, `/help` lists the commands, `/exit` quits. Flags
work here too (`dsc --think`). What you allowed with `a` holds until you quit. Inputs are remembered in
`~/.config/dsc/history.json`; the conversation itself is not saved, after a restart the model starts from zero.

Colors switch off with `NO_COLOR=1` or when the output is not a terminal.

### Flags

| Flag | Meaning |
|---|---|
| `-y`, `--yes` | run actions without asking (the deny list still applies) |
| `--think` | reasoning mode; the reasoning is shown dimmed |
| `--search` | web search (not tested yet) |
| `--expert` | the "expert" model instead of the fast one (not tested yet) |

| Command | Meaning |
|---|---|
| `dsc`, `dsc chat` | interactive chat |
| `dsc login --token` / `--import <file>` | store the token |
| `dsc logout` | remove it |
| `dsc doctor` | check paths, token and proof of work; sends no prompt |

### Approvals

The model can ask for three actions: run a shell command, read a file, write a file. Before each one `dsc` decides:

1. It matches `policy.deny` → never runs, also with `--yes`.
2. `--yes` is set, or the command is on `policy.allow` → runs.
3. Otherwise you are asked:

```console
● Run head -n 1 Cargo.toml
  show the first line
  Allow? y yes · a always in this session · n no · e edit
```

One key answers. `a` allows that program (or tool) until you quit; `e` lets you change the command first; `n` tells the
model it was denied, so it can try another way. Without a terminal on stdin every question counts as no.

Ctrl-C while a command runs kills only the command; Ctrl-C while waiting for the model ends the task (in a chat
you are back at the prompt).

## Safety and privacy

- **Everything an action prints is sent to DeepSeek.** The default allow list therefore has only programs that do
  not print file content (`ls`, `df`, `stat`, `git status`, ...). Think before you approve `cat` on a private file.
- The deny list is a plain substring check on the command or path. It stops obvious mistakes (`rm -rf /`, SSH
  keys, the token file); it is **not a sandbox**.
- `--yes` lets a free chat model run anything not on the deny list. Use it only for tasks and folders where that
  is acceptable, ideally in a container or VM.
- The model sees only what you send: the task, the working directory and the results of approved actions.

## Configuration

`~/.config/dsc/config.toml` is written with the defaults on the first run (`$DSC_HOME` changes the folder).

```toml
[deepseek]
model_type = "default"      # or "expert"
thinking = false
search = false
locale = "en"
timeout_secs = 180

[throttle]                  # keeps the request rate low
min_interval_ms = 3000      # minimum gap between two requests, also across runs
jitter_ms = 2000            # random extra gap
max_per_hour = 120          # 0 = no cap
retries_on_429 = 2
wait_on_429_secs = 60

[agent]
max_steps = 25              # answers of the model per task
max_output_bytes = 8000     # cap of stdout and of stderr sent back (head + tail kept)
command_timeout_secs = 120

[policy]
allow = ["ls", "pwd", "df", "du", "uname", "whoami", "date", "which", "stat", "wc", "file", "git status"]
deny = ["rm -rf /", "rm -rf ~", "mkfs", "dd of=/dev/", "shutdown", "reboot", ":(){", ".ssh/id_", ".aws/credentials", "dsc/auth.json"]
```

An `allow` entry is a program, or a program with its first arguments (`git status`). It applies only to simple
commands: anything with `; & | > < $ ( )` asks.

## How it works

```
cli → agent loop → Backend trait → DeepSeek web client (HTTP, proof of work, SSE)
          ├─ protocol   the rules sent to the model, parser of its action block
          ├─ policy     allow / ask / deny
          └─ tools      shell, read_file, write_file
```

- **No function calling in the web chat**, so the model is told to end its answer with one fenced block:
  ````
  ```dsc
  {"tool":"shell","cmd":"df -h /","why":"check free space"}
  ```
  ````
  `dsc` runs it and sends the result as the next message. An answer without a block is the final answer.
- **Proof of work**: each request needs a solved challenge. `dsc` downloads the site's own wasm module once and
  runs it in an embedded interpreter ([wasmi](https://github.com/wasmi-labs/wasmi)), about 1 s per request.
- **History** stays on DeepSeek's side; each turn sends only the new text.

Details: [architecture](.agents/skills/general/architecture/SKILL.md).

## Development

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings
DSC_HOME=var/home cargo run -- doctor      # throwaway home
```

The agent loop is tested with a scripted model, the stream parser with recorded answers; no test calls DeepSeek.
`AGENTS.md` is the map for coding agents.

## Credits

The web protocol was learned from [FreeDeepseekAPI](https://github.com/ForgetMeAI/FreeDeepseekAPI).

## License

[MIT](LICENSE)
