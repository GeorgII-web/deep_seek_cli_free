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

### Download (Linux x86_64)

```bash
curl -fsSL https://github.com/GeorgII-web/deep_seek_cli_free/releases/latest/download/dsc-x86_64-linux.tar.gz | tar -xz dsc
./dsc --version
```

That gives one file, `dsc`, in the current folder (glibc 2.35 or newer: Ubuntu 22.04+, Debian 12+, Fedora 36+,
WSL). To have it everywhere:

```bash
mkdir -p ~/.local/bin && mv dsc ~/.local/bin/     # ~/.local/bin must be in your PATH
```

Then [log in](#log-in) once and run `dsc`.

All versions and checksums: [releases](https://github.com/GeorgII-web/deep_seek_cli_free/releases).

### Build from source

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

The token is stored in `~/.config/deep_seek_cli_free/auth.json` with mode 0600. Treat it like a password: it gives full access
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

`dsc` without a task (or `dsc chat`) opens a chat: the screen is cleared, the input box stays at the bottom and
the output scrolls above it. Earlier output is in your terminal's scrollback (mouse wheel, Shift+PgUp). Every
input is a task in the same conversation, so the model remembers what it did before:

```console
$ dsc

  ███  ████ ████ ███    ███ ████ ████ █  █   ███ █    ███  ████ ███  ████ ████
  █  █ █    █    █  █  █    █    █    █ █   █    █     █   █    █  █ █    █
  █  █ ███  ███  ███    ██  ███  ███  ██    █    █     █   ███  ███  ███  ███
  █  █ █    █    █        █ █    █    █ █   █    █     █   █    █ █  █    █
  ███  ████ ████ █     ███  ████ ████ █  █   ███ ████ ███  █    █  █ ████ ████

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
  ⏵ ask · Shift+Tab mode · Enter send · Alt+Enter new line · Ctrl-C twice quit · ↑↓ history · /help
```

While `dsc` works, the line under the box shows what it waits for and for how long
(`⠹ Waiting for DeepSeek… 3s · Esc to stop · ⏵ ask`). The last line of a task shows the seconds it took, drawn as a rule across the screen; your prompts stand out on a background
bar. The box grows
with your text, up to ten rows. A single task (`dsc "<task>"`) does not clear the screen: it prints below your
shell prompt.

| Key in the input box | Effect |
|---|---|
| Enter | send |
| `/` | list the commands: Up / Down choose, Enter runs the chosen one at once, Tab only completes it in the box, Esc closes the list |
| Alt+Enter, Ctrl-J, or `\` then Enter | new line |
| Up / Down | move in the text, then through earlier inputs |
| Ctrl-A, Ctrl-E, Ctrl-W, Ctrl-U, Ctrl-K | start, end, delete word, delete to start, delete to end |
| Shift+Tab | switch the mode: `ask` ⇄ `allow all`, also while a task runs |
| Esc | clear the box; while a task runs: stop the task |
| Ctrl-C | clears the box and asks to press it again; twice within two seconds quits, also while a task runs |
| Ctrl-D | quit (empty box) |

Pasting several lines works. `/new` starts a fresh conversation, `/mode` switches the mode, `/help` lists the
commands and keys, `/exit` quits. Flags
work here too (`dsc --think`). What you allowed with `a` holds until you quit. Inputs are remembered in
`~/.config/deep_seek_cli_free/history.json`; the conversation itself is not saved, after a restart the model starts from zero.

In answers, `inline code` is colored, code blocks are shown with a bar on the left, and links (`https://…`) are
blue and real terminal links: the terminal underlines one under the mouse and opens it on click (Ctrl+click in
Windows Terminal and most others). Colors switch off with
`NO_COLOR=1` or when the output is not a terminal.

### Flags

| Flag | Meaning |
|---|---|
| `-y`, `--yes` | start in the mode `allow all`: actions run without asking (the deny list still applies) |
| `--think` | reasoning mode; the reasoning is shown dimmed |
| `--search` | web search (not tested yet) |
| `--expert` | the "expert" model instead of the fast one (not tested yet) |

| Command | Meaning |
|---|---|
| `dsc`, `dsc chat` | interactive chat |
| `dsc login --token` / `--import <file>` | store the token |
| `dsc logout` | remove it |
| `dsc doctor` | check paths, token and proof of work; sends no prompt |

### Modes

| Mode | Shown as | Meaning |
|---|---|---|
| ask (default) | `⏵ ask` | actions outside the allow list need your yes |
| allow all | `⏵⏵ allow all` | every action runs without a question; only the deny list stops one |

The mode is shown under the input box and on the progress line (`ask` green, `allow all` orange). Shift+Tab or
`/mode` switches it at any time, also in the middle of a task. The mode you chose is saved in `config.toml`
(`policy.mode`) and used on the next start; `--yes` starts in allow all for that run only.

### Approvals

The model can ask for three actions: run a shell command, read a file, write a file. Before each one `dsc` decides:

1. It matches `policy.deny` → never runs, also with `--yes`.
2. The mode is `allow all`, or the command is on `policy.allow` → runs.
3. Otherwise you are asked:

```console
● Run head -n 1 Cargo.toml
  show the first line
  Allow? y yes · a always in this session · n no · e edit
```

One key answers. `a` allows that program (or tool) until you quit; `e` lets you change the command first; `n` tells the
model it was denied, so it can try another way. Without a terminal on stdin every question counts as no.

Esc stops the running task: a running command is killed and you are back at the prompt (a single task ends).
One Ctrl-C never closes `dsc`; press it twice within two seconds to quit. Without a terminal (piped input) Ctrl-C
stops the task.

## Safety and privacy

- **Everything an action prints is sent to DeepSeek.** The default allow list therefore has only programs that do
  not print file content (`ls`, `df`, `stat`, `git status`, ...). Think before you approve `cat` on a private file.
- The deny list is a plain substring check on the command or path. It stops obvious mistakes (`rm -rf /`, SSH
  keys, the token file); it is **not a sandbox**.
- The mode `allow all` (`--yes`, Shift+Tab, `/mode`) lets a free chat model run anything not on the deny list. Use it only for tasks and folders where that
  is acceptable, ideally in a container or VM.
- The model sees only what you send: the task, the working directory and the results of approved actions.

## Configuration

Everything lives in `~/.config/deep_seek_cli_free/`: `config.toml`, `auth.json`, `history.json`, `cache/`
(`$DSC_HOME` changes the folder; a folder `~/.config/dsc` of an earlier version is moved there on the first start).
`config.toml` is written with the defaults on the first run. `dsc` rewrites it when you switch the mode, so
comments in it are not kept.

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
mode = "ask"                # or "allow_all"; the last mode you chose is saved here and used on the next start
allow = ["ls", "pwd", "df", "du", "uname", "whoami", "date", "which", "stat", "wc", "file", "git status"]
deny = ["rm -rf /", "rm -rf ~", "mkfs", "dd of=/dev/", "shutdown", "reboot", ":(){", ".ssh/id_", ".aws/credentials", "deep_seek_cli_free/auth.json", "dsc/auth.json"]
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
- **No repeated output**: you see the output of every command. When that output already is the answer (a file
  listing, a file's content), the model is told to send a silent "done" block instead of printing it again; a
  long output is then shown in full instead of the first 8 lines.
- **Proof of work**: each request needs a solved challenge. `dsc` downloads the site's own wasm module once and
  runs it in an embedded interpreter ([wasmi](https://github.com/wasmi-labs/wasmi)), about 1 s per request.
- **History** stays on DeepSeek's side; each turn sends only the new text.

Details: [architecture](.agents/skills/general/architecture/SKILL.md).

## Development

Every push runs the checks on GitHub (`.github/workflows/ci.yml`). A release: set `version` in `Cargo.toml`, push
it, then push the tag `v<version>`; `.github/workflows/release.yml` builds the binary and publishes the release.

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
