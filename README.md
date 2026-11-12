# Deep Seek CLI free

`dsc` is a command line agent for Linux. You give it a task in plain words; it asks DeepSeek what to do, runs the
commands on your machine after your approval, and answers. It uses your **free DeepSeek web account**
(chat.deepseek.com), not the paid API. One Rust binary, no node, no browser at runtime.

```console
$ dsc

  ███  ████ ████ ███    ███ ████ ████ █  █   ███ █    ███  ████ ███  ████ ████
  █  █ █    █    █  █  █    █    █    █ █   █    █     █   █    █  █ █    █
  █  █ ███  ███  ███    ██  ███  ███  ██    █    █     █   ███  ███  ███  ███
  █  █ █    █    █        █ █    █    █ █   █    █     █   █    █ █  █    █
  ███  ████ ████ █     ███  ████ ████ █  █   ███ ████ ███  █    █  █ ████ ████

  ✻ dsc 0.4.10 · DeepSeek web · default

 > how much disk space is free on / ?

● Run df -h /
  check free disk space on /
  ⎿ ✓ exit code 0 · 2 lines  ▾ hide
    Filesystem      Size  Used Avail Use% Mounted on
    /dev/sdd       1007G  147G  809G  16% /

There is 809G free on / (out of 1007G, 16% used).

 ✓ 8.1s · 2 answers · 1 action · 2 requests

╭──────────────────────────────────────────────────────────────────────────────╮
│ > which command did you run for that?                                        │
╰──────────────────────────────────────────────────────────────────────────────╯
  ◆ DeepSeek · ✦ Think · ⏵ ask · Shift + Tab        Ctrl + C twice quit · ↑↓ history
```

The last line of a task shows what it took: the time, the model's answers, the actions that ran, and the number
of requests sent to DeepSeek: one per answer the model had to give, which is what the throttle limits count.

The chat runs on its own screen: the input box stays at the bottom, the results scroll above it, a command's
output folds to one line, and `/` lists the commands. Every input is a task in the same conversation.

> **Unofficial.** This talks to the private web API of chat.deepseek.com. It can break when the site changes, and
> DeepSeek blocks an account quickly when requests come too fast; the defaults are slow for that reason. Use your
> own account, at your own risk.

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

`dsc` needs the login token of your DeepSeek web account. Start `dsc` (or `dsc login`): a window asks for it and
shows these steps.

1. Open <https://chat.deepseek.com> in your browser and log in.
2. Open the console: **F12** or **Ctrl+Shift+J**.
3. Click into the console input, type `allow pasting`, press **Enter**. (Chrome asks for this once before it
   lets you paste.)
4. Paste this line there and press **Enter**. It copies the token to your clipboard and prints `undefined`:
   ```js
   copy(JSON.parse(localStorage.getItem('userToken')).value)
   ```
5. Back in the `dsc` window, paste the token (right click, or **Ctrl+Shift+V**) and press **Enter**. It is shown
   as dots only.

In that window the mouse belongs to the terminal: the address in step 1 is a link you can click, and you can
select the line of step 4 with the mouse to copy it.

Check it with `dsc doctor`:

```console
token     ok (challenge DeepSeekHashV1, difficulty 144000)
wasm      ok (26612 bytes)
pow       ok (870 ms)
```

The token is stored in `~/.config/deep_seek_cli_free/auth.json` with mode 0600. Treat it like a password: it gives full access
to your DeepSeek account. `dsc logout` removes it.

The chat checks the token when it starts (at most once in 15 minutes) and opens the same window when it is
missing or dead. When the token
dies in the middle of a chat, the window opens by itself and with the new token the task runs again;
`/login` opens it at any time. The token is saved in `auth.json` only: it is never printed, never kept in the
input history and never sent as a message to the model.

In a script (no terminal) use `dsc login --token`, which reads it from a hidden prompt.

`dsc login --import <file>` reads the `deepseek-auth.json` of
[FreeDeepseekAPI](https://github.com/ForgetMeAI/FreeDeepseekAPI) instead.

## Use

```bash
dsc "which process uses the most memory?"
dsc --think "why does cargo build fail here?"    # reasoning mode
dsc --yes "create a .gitignore for a Rust project"
```

### Chat

`dsc` without a task (or `dsc chat`) opens a chat on its own screen: nothing of your terminal's earlier history
is visible, the input box stays fixed at the bottom, and only the output above it scrolls: mouse wheel, PgUp /
PgDn, Ctrl+Home, Ctrl+End. Drag over the output to select text: it is highlighted and stays selected. A right click or Ctrl-C copies it;
with nothing selected, a right click pastes the copied text into the input box, like Ctrl-V (Shift+drag uses
the terminal's own selection instead). A mark on the box shows when you are scrolled up; new output does not move the
view until you go back down. When you quit, the conversation is printed into your normal terminal, so it is
still there afterwards. Every input is a task in the same conversation, so the model remembers what it did
before (the screen is shown at the top of this page).

The step in progress is marked right where it happens: a spinner and the seconds under a running command
(`⎿ ⠧ running… 2s`), and a line of its own at the start of the answer (`⠧ Waiting for DeepSeek… 3s`, then `⠧ Answering… 5s`)
that disappears when the answer is complete. A command's output appears line by line while it runs. When it has finished, a long output folds to one line
(`⎿ ✓ exit code 0 · 23 lines ▸ show`; a failed one is red with `✗`); click that line, or press Ctrl-O for the last one, to open and close it. Up
to five lines stay open, and so does an output that is itself the answer to your task.

While `dsc` works, the line under the box shows what it waits for and for how long
(model and mode stay at the left as when idle; at the right edge the icon with a short word that changes now and
then and `Esc interrupt`; the step itself and its seconds are shown in the output). The last line of a task shows the seconds it took on a colored bar (green when it worked, red when it failed or was stopped);
your prompts stand out on a bar of their own. The box grows
with your text, up to ten rows. A single task (`dsc "<task>"`) does not clear the screen: it prints below your
shell prompt.

The line under the box shows only the mode with its key, how to quit and the history keys; `/help` lists every
key.

| Key in the input box | Effect |
|---|---|
| Enter | send |
| `/` | list the commands: Up / Down choose, Enter runs the chosen one at once, Tab only completes it in the box, Esc closes the list |
| Shift+Enter, Ctrl-J, or `\` then Enter | new line (Shift+Enter needs a terminal that reports it; Ctrl-J always works) |
| PgUp / PgDn, mouse wheel | scroll the output; Ctrl+Home = top, Ctrl+End = bottom |
| Click on `▸ show` / `▾ hide`, Ctrl-O | unfold or fold the output of a command (Ctrl-O: the last one) |
| Drag with the mouse | select text in the output (nothing is copied yet) |
| Right click, or Ctrl-C | copy the selected text to the clipboard |
| Right click (nothing selected), or Ctrl-V | paste the clipboard into the input box, also text copied in other windows |
| Up / Down | move in the text, then through earlier inputs |
| Ctrl-A, Ctrl-E, Ctrl-W, Ctrl-U, Ctrl-K | start, end, delete word, delete to start, delete to end |
| Shift+Tab | switch the mode: `ask` ⇄ `allow`, also while a task runs |
| Esc | clear the box; while a task runs: stop the task |
| Ctrl-C | with text selected: copies it. Otherwise clears the box and asks to press it again; twice within two seconds quits, also while a task runs |
| Ctrl-D | quit (empty box) |

Pasting several lines works. `/new` starts a fresh conversation, `/sessions` lists the earlier ones and switches to the one you pick, `/model`
chooses the real account or the mock, `/think` switches deep thinking, `/browse` DeepSeek's web search, `/permissions` switches the mode, `/login` asks for a new token, `/help` lists the
commands and keys, `/exit` quits. Flags
work here too (`dsc --think`). What you allowed with `a` holds until you quit. Inputs are remembered in
`~/.config/deep_seek_cli_free/history.json`. After a restart the chat begins a new conversation; `/sessions` goes back to an
earlier one.

In answers, `inline code` is colored, code blocks are shown with a bar on the left, and links (`https://…`) are
blue and real terminal links: the terminal underlines one under the mouse and opens it on click (Ctrl+click in
Windows Terminal and most others). Colors switch off with
`NO_COLOR=1` or when the output is not a terminal.

### Queue

While a task runs, the input box stays usable: write the next prompt (and drop files) and press Enter. It goes
into a queue shown above the box, one row per prompt with its number, and runs when the tasks before it are done.
Up to 5 prompts wait; with a full queue the text stays in the box. Alt+1…5 (or Ctrl+1…5 where the terminal reports
it) drops that prompt. The tab title shows how many wait (`⏳ +2 dsc · …`). Esc stops only the running task, the queue goes on; Ctrl-C twice
quits. While an approval question is shown, keys answer the question.

### Attach files

Drop files from your file manager onto the chat box (or paste their full paths). They are not typed into the box:
they appear in its top border (`📎 report.pdf · 120 KB`) and go with your next message. Files alone are a
message too (the model is asked what they contain). Backspace in an empty box drops the last file, Esc drops all.
Up to 50 files, 100 MB each.

dsc uploads them to DeepSeek the way the web page does (each file gets its own proof of work), waits until
DeepSeek has read them, and sends their ids with the prompt. What DeepSeek can read (PDF, Office files, text,
images) is up to DeepSeek. Uploads do not use up the request budget of the throttle.

Files that look private get a warning before anything leaves your machine: SSH keys and anything in `~/.ssh`,
`~/.aws`, `~/.gnupg`, `.env` files, `.pem` / `.key` / `.kdbx` files, names with `password`, `secret` or `token`,
and text with a private key block, a known token shape or a `password = …` line. You choose: `y` sends them,
`s` sends only the other files, `n` cancels the message. The check is a heuristic: a file it misses is sent
without a question.

In Windows Terminal with a WSL profile a drop pastes the Linux path (`/mnt/c/...`); Windows paths (`C:\...`) and
`file://` links are understood too.

### Models: the real account or the mock

`/model` opens a choice of who answers:

| Model | What it is |
|---|---|
| DeepSeek | the real account with your token; every task counts against it |
| Mock | a fake model inside `dsc`: scripted answers, no network, no account |

The choice is saved (`deepseek.api` in `config.toml`) and used on the next start. The model in use is always
shown at the bottom, next to the mode: `◆ DeepSeek` or `◆ Mock`. Use the mock to try the app, its
keys and approvals without touching your account. It answers slowly on purpose, so you can watch the interface
work: two seconds of "waiting", then word by word (`[mock]` in the config sets the speed). When the chat starts
on the mock, it lists the tasks it understands: `!<command>` asks to run the
command, `browse <url>` answers as if it had read the page on its side, `curl <url>` asks to fetch it with curl on your
machine, `run <command>` runs it and then answers, `long` prints 80 lines, `slow` answers slowly (try Esc and
scrolling), `code` shows formatting, `dead` shows the token window; anything else is echoed back. Dropped files
are "uploaded" and "read" without any network (`reject` or `unreadable` in a file name shows the failures).

### Deep thinking and Browse

`/think` switches deep thinking on and off. With it on, the model reasons before it answers and the reasoning is
shown dimmed above the answer; answers take longer.

`/browse` switches Browse on and off: DeepSeek's own web search and browser (the Search button of the site). With
it on, the model reads web pages on its side; with it off, a task that needs a page fetches it from your machine
(you approve the `curl`).

DeepSeek does not do both at once: turning one on turns the other off. Both can be off. The bottom line shows
them: `✦ Think` and `◉ Browse` are pink while on and gray while off. A click on either (or on the mode, `⏵ ask` /
`⏵⏵ allow`) switches it like its command; while a task runs only the mode switches. The choice is saved (`deepseek.thinking`,
`deepseek.search`) and used on the next start.

### Sessions

Every conversation you start is remembered after its first task. `/sessions` opens a list (title = the first
task, number of tasks, age, folder); Up / Down choose, Enter switches, Esc closes. After a switch the screen
shows that conversation's earlier output again and the model remembers it, also after a restart of `dsc`, because the messages themselves are kept by DeepSeek:
When you quit, `dsc` prints the command that brings you back to exactly this conversation, with its screen as you
left it:

```console
Continue this conversation:
  dsc --session=6a1f0c9e-…
```

The terminal tab is named after the conversation (`dsc · <first task>`), so several chats are easy to tell
apart, and an icon in front shows the state: `⏳` while a task is in progress, `❓` while it waits for your yes or
no, `✻` when nothing runs. Your own tab title is put back when you quit. `sessions.json` holds only the session id, the place to continue at and the title, and `sessions/<id>.log` the
screen output. `/new` starts a conversation on a clean screen. The list contains the
conversations started with `dsc`, the last 50; chats from the DeepSeek website are not in it, since they lack the
agent rules. The mock has sessions of its own, so `/sessions` can be tried without the real account: each model
lists only its own conversations.

### Flags

| Flag | Meaning |
|---|---|
| `-y`, `--yes` | start in the mode `allow`: actions run without asking (the deny list still applies) |
| `--think` | deep thinking for this run; the reasoning is shown dimmed. In the chat, `/think` switches it |
| `--no-search` | switch DeepSeek's own web search off for this run (pages are then fetched from this machine) |
| `--expert` | the "expert" model instead of the fast one (not tested yet) |
| `--session=<code>` | continue an earlier conversation; `dsc` prints the code when it exits |

| Command | Meaning |
|---|---|
| `dsc`, `dsc chat` | interactive chat |
| `dsc login` | store the token: the token window; `--token` = hidden prompt, `--import <file>` = from a file |
| `dsc logout` | remove it |
| `dsc doctor` | check paths, token and proof of work; sends no prompt |

### Modes

| Mode | Shown as | Meaning |
|---|---|---|
| ask (default) | `⏵ ask` | actions outside the allow list need your yes |
| allow | `⏵⏵ allow` | every action runs without a question; only the deny list stops one |

The mode is shown under the input box and on the progress line (`ask` green, `allow` orange). Shift+Tab or
`/permissions` switches it at any time, also in the middle of a task. The mode you chose is saved in `config.toml`
(`policy.mode`) and used on the next start; `--yes` starts in allow for that run only.

### Approvals

The model can ask for three actions: run a shell command, read a file, write a file. Before each one `dsc` decides:

1. It matches `policy.deny` → never runs, also with `--yes`.
2. The mode is `allow`, or the command is on `policy.allow` → runs.
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
- The mode `allow` (`--yes`, Shift+Tab, `/permissions`) lets a free chat model run anything not on the deny list. Use it only for tasks and folders where that
  is acceptable, ideally in a container or VM.
- The model sees only what you send: the task, the working directory, the results of approved actions and the files
  you drop on the chat box (files that look private need a yes first).

## Configuration

Everything lives in `~/.config/deep_seek_cli_free/`: `config.toml`, `auth.json`, `history.json`, `sessions.json`,
`cache/`
(`$DSC_HOME` changes the folder; a folder `~/.config/dsc` of an earlier version is moved there on the first start).
`config.toml` is written with the defaults on the first run. `dsc` rewrites it when you switch the mode, so
comments in it are not kept.

```toml
[deepseek]
api = "deepseek"            # or "mock": the fake model inside dsc; `/model` switches and saves it
model_type = "default"      # or "expert"
thinking = false
search = true               # Browse (/browse): DeepSeek's web search; off: pages are fetched from this machine
locale = "en"
timeout_secs = 180

[throttle]                  # keeps the request rate very low, so the account is not blocked
min_interval_ms = 30000     # minimum gap between two requests, also across runs
jitter_ms = 15000           # random extra gap: 30 to 45 s in all
max_per_minute = 2          # 0 = no cap
max_per_hour = 12           # 0 = no cap
retries_on_429 = 2          # retries when DeepSeek says "too many requests"
wait_on_429_secs = 300      # pause before the first retry; doubled for each further one

[agent]
max_steps = 25              # answers of the model per task
max_output_bytes = 8000     # cap of stdout and of stderr sent back (head + tail kept)
command_timeout_secs = 120

[ui]
mouse_scroll = true         # the mouse works inside dsc: wheel scrolls, click folds, drag selects, right click copies or pastes

[mock]                      # speed of the built-in mock model
wait_ms = 2000              # pause before an answer starts
word_ms = 150               # pause between two words; the `slow` task takes four times as long

[policy]
mode = "ask"                # or "allow_all"; the last mode you chose is saved here and used on the next start
allow = ["ls", "pwd", "df", "du", "uname", "whoami", "date", "which", "stat", "wc", "file", "git status"]
deny = ["rm -rf /", "rm -rf ~", "mkfs", "dd of=/dev/", "shutdown", "reboot", ":(){", ".ssh/id_", ".aws/credentials", "deep_seek_cli_free/auth.json", "dsc/auth.json"]
```

These limits are slow on purpose: the free account was blocked at about 25 requests an hour. A task that runs
a command needs two or three requests, so expect a minute or more per task; raise the numbers at your own risk.
When `dsc` has to wait for one of these limits, the progress line says so. A pause after "too many requests" is
remembered, so a new start does not run straight into the limit again. A config that still has the faster
defaults of an earlier version is moved to these values automatically; numbers you changed yourself are kept.

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
- **Web pages**: DeepSeek's own web search (the Search button of the site) is on by default. For "open this
  site" or "look this up" the model reads the page on its side and answers; nothing is fetched from your
  machine. It uses `curl` here only when you ask for that ("fetch it locally", "with curl") or when the address
  is only reachable from your machine (localhost). `/browse`, `--no-search` or `search = false` turns the
  search off.
- **No repeated output**: you see the output of every command. When that output already is the answer (a file
  listing, a file's content), the model is told to send a silent "done" block instead of printing it again; a
  long output is then shown in full instead of the first 8 lines.
- **Proof of work**: each request needs a solved challenge. `dsc` downloads the site's own wasm module once and
  runs it in an embedded interpreter ([wasmi](https://github.com/wasmi-labs/wasmi)), about 1 s per request.
- **History** stays on DeepSeek's side; each turn sends only the new text.

Details: [architecture](.agents/skills/general/architecture/SKILL.md).

## Development

The version grows by itself: a commit that changes code under `src/` raises the patch number (a git hook in
`.githooks/`; enable it once in a clone with `git config core.hooksPath .githooks`). Raise the minor or major
number by hand in `Cargo.toml` when a change deserves it.

Pushing does not start anything on GitHub. The checks, the build and the publishing all run in the release
workflow: set `version` in `Cargo.toml`, push it, then push the tag `v<version>`
(`.github/workflows/release.yml`). `.github/workflows/ci.yml` runs the checks alone when started by hand from the
Actions tab.

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings
DSC_HOME=var/home cargo run -- doctor      # throwaway home
```

No test and no local try-out may call DeepSeek: the account gets blocked quickly. The code enforces it: the
unit tests cannot get the real address, and a run with `DSC_HOME` set is refused the real API unless `DSC_LIVE=1`. There are three stand-ins:
the unit tests script the model (`cargo test`); the mock inside the app (`/model`, `deepseek.api = "mock"`)
answers without any network; `scripts/mock-deepseek.py` is a local HTTP server in the shape of the DeepSeek
web API (`DSC_API_BASE=http://127.0.0.1:8765 dsc`), for the client code. `scripts/pty-drive.py` drives the chat
in a pseudo-terminal against that server and prints the screen.
`AGENTS.md` is the map for coding agents.

## Status

Version 0.2. It works end to end: the agent loop with approvals, a chat on its own screen (input box at the
bottom, scrollable and foldable output, mouse selection), sessions you can return to, a token window, a built-in
mock model to try everything without the account, deep thinking, DeepSeek's own web search, and very careful
request throttling. Not built yet: login through the browser, a static and an ARM binary. See
[TASKS.md](TASKS.md).

DeepSeek blocks the free account quickly when requests come too fast; that is why the defaults are slow.

## Credits

The web protocol was learned from [FreeDeepseekAPI](https://github.com/ForgetMeAI/FreeDeepseekAPI).

## License

[MIT](LICENSE)
