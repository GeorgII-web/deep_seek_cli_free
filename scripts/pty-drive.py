#!/usr/bin/env python3
"""Drives `dsc` chat in a pseudo-terminal with scripted keys and prints the screen.

It runs against the local mock of DeepSeek (`scripts/mock-deepseek.py`, started and
stopped here): no request leaves the machine. The real API is used only with
DSC_LIVE=1, and that needs the user's word (every live run counts against the account).

Usage: scripts/pty-drive.py '<python list of keys>' [rows] [columns]
A key is a string, or (string, seconds to wait after it). Examples:
  scripts/pty-drive.py '["/help\\r", "abc", "\\x1b\\r", "def", "\\x1b[A", "X"]'
  scripts/pty-drive.py '[("!ls -la\\r", 2), ("long\\r", 2), "\\x1b[5~"]' 30 100      # mock tasks, then PgUp
Keys: \\r Enter, \\n Ctrl-J (new line), \\x1b[A up, \\x1b[B down, \\x1b[5~ PgUp, \\x1b[6~ PgDn,
\\x1b[<64;1;1M wheel up, \\x1b[<65;1;1M wheel down, \\x1b Esc, \\x03 Ctrl-C, \\x04 Ctrl-D, \\x1b[Z Shift+Tab.
Mock tasks: see scripts/mock-deepseek.py (`!<cmd>`, `run <cmd>`, `long`, `slow`, `code`, `dead`, ...).
Home: DSC_HOME, default var/mock-home (made here: a mock token and a config without throttle).
MOCK_TOKEN sets the token of a new home (`dead...` = the mock rejects it). target/debug/dsc is run.
Default screen: 24 rows, 60 columns. The output shows the lines that scrolled off the top, then the
screen (`-` output area, `#` dock), the cursor, and what the mock was asked.
"""
import fcntl, json, os, pty, re, select, socket, struct, subprocess, sys, termios, time, urllib.request

keys = eval(sys.argv[1])
H = int(sys.argv[2]) if len(sys.argv) > 2 else 24
W = int(sys.argv[3]) if len(sys.argv) > 3 else 60

LIVE = os.environ.get('DSC_LIVE') == '1'
home = os.path.abspath(os.environ.get('DSC_HOME', 'var/mock-home'))
mock = None
if LIVE:
    print('!!! DSC_LIVE=1: this run talks to the real DeepSeek API', file=sys.stderr)
    os.environ.pop('DSC_API_BASE', None)
else:
    # A free port, the mock on it, and a home that cannot hold a real token.
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        port = probe.getsockname()[1]
    mock = subprocess.Popen([sys.executable, os.path.join(os.path.dirname(os.path.abspath(__file__)), 'mock-deepseek.py'), str(port)],
                            stdout=subprocess.PIPE, text=True)
    mock.stdout.readline()
    os.environ['DSC_API_BASE'] = f'http://127.0.0.1:{port}'
    os.makedirs(home, exist_ok=True)
    if not os.path.exists(os.path.join(home, 'auth.json')):
        with open(os.path.join(home, 'auth.json'), 'w') as f:
            json.dump({'token': os.environ.get('MOCK_TOKEN', 'mock-token')}, f)
    if not os.path.exists(os.path.join(home, 'config.toml')):
        with open(os.path.join(home, 'config.toml'), 'w') as f:
            f.write('[throttle]\nmin_interval_ms = 0\njitter_ms = 0\nmax_per_minute = 0\nmax_per_hour = 0\nwait_on_429_secs = 1\n')

pid, fd = pty.fork()
if pid == 0:
    os.environ['DSC_HOME'] = home
    os.execv('./target/debug/dsc', ['dsc'])
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', H, W, 0, 0))

# A small terminal: enough of VT100 for what dsc writes.
rows = [[' '] * W for _ in range(H)]
scrollback = []
top, bottom, y, x, saved = 0, H - 1, 0, 0, (0, 0)
pending = ''
main_screen = None


def line_feed():
    global y
    if y == bottom:
        line = rows.pop(top)
        if top == 0:
            scrollback.append(line)
        rows.insert(bottom, [' '] * W)
    elif y < H - 1:
        y += 1


def feed(text):
    global top, bottom, y, x, saved, pending, main_screen
    s = pending + text
    pending = ''
    i = 0
    while i < len(s):
        c = s[i]
        if c == '\x1b':
            if len(s) - i < 2:
                pending = s[i:]
                return
            if s[i + 1] == ']':
                # OSC (hyperlinks): skip up to its end, ESC \\ or BEL.
                ends = [e for e in (s.find('\x1b\\', i + 2), s.find('\x07', i + 2)) if e != -1]
                if not ends:
                    pending = s[i:]
                    return
                end = min(ends)
                i = end + (2 if s[end] == '\x1b' else 1)
                continue
            if s[i + 1] in '78':
                if s[i + 1] == '7':
                    saved = (y, x)
                else:
                    y, x = saved
                i += 2
                continue
            m = re.match(r'\x1b\[([?><=]?)([0-9;]*)([A-Za-z])', s[i:])
            if not m:
                if re.fullmatch(r'\x1b\[[?><=]?[0-9;]*', s[i:]):
                    pending = s[i:]
                    return
                i += 1
                continue
            private, args, kind = m.groups()
            nums = [int(n) if n else None for n in args.split(';')] if args else [None]
            first = nums[0]
            if private in ('>', '<', '='):
                pass
            elif private:
                # Alternate screen: its own content; the main screen and cursor come back after.
                if first == 1049 and kind == 'h':
                    main_screen = ([r[:] for r in rows], y, x)
                    for r in rows:
                        r[:] = [' '] * W
                elif first == 1049 and kind == 'l' and main_screen:
                    saved_rows, y, x = main_screen
                    for r, old in zip(rows, saved_rows):
                        r[:] = old
            elif kind == 'n' and first == 6:
                os.write(fd, f'\x1b[{y + 1};{x + 1}R'.encode())
            elif kind == 'A':
                y = max(0, y - (first or 1))
            elif kind == 'G':
                x = (first or 1) - 1
            elif kind == 'H':
                y = min(H - 1, (first or 1) - 1)
                x = ((nums[1] if len(nums) > 1 else None) or 1) - 1
            elif kind == 'J':
                if (first or 0) == 2:
                    for r in rows:
                        r[:] = [' '] * W
                else:
                    rows[y][x:] = [' '] * (W - x)
                    for r in rows[y + 1:]:
                        r[:] = [' '] * W
            elif kind == 'K':
                if (first or 0) == 2:
                    rows[y][:] = [' '] * W
                else:
                    rows[y][x:] = [' '] * (W - x)
            elif kind == 'r':
                top = (first or 1) - 1
                bottom = ((nums[1] if len(nums) > 1 else None) or H) - 1
                y, x = 0, 0
            i += m.end()
            continue
        if c == '\r':
            x = 0
        elif c == '\n':
            line_feed()
        elif c >= ' ':
            if x >= W:
                x = 0
                line_feed()
            rows[y][x] = c
            x += 1
        i += 1


def pump(seconds):
    end = time.time() + seconds
    while time.time() < end:
        ready, _, _ = select.select([fd], [], [], 0.05)
        if ready:
            try:
                feed(os.read(fd, 65536).decode('utf8', 'replace'))
            except OSError:
                return


pump(1.0)
for key in keys:
    wait = 0.25
    if isinstance(key, tuple):
        key, wait = key
    os.write(fd, key.encode())
    pump(wait)
pump(0.5)

for line in scrollback:
    print('  ^ ' + ''.join(line).rstrip())
for n, line in enumerate(rows):
    mark = '-' if top <= n <= bottom else '#'
    print(f'{n + 1:3}{mark}' + ''.join(line).rstrip())
print(f'cursor row {y + 1} column {x + 1}; scroll region rows {top + 1}-{bottom + 1}')
if mock:
    try:
        with urllib.request.urlopen(os.environ['DSC_API_BASE'] + '/_stats', timeout=2) as response:
            print('mock:', response.read().decode())
    except OSError as error:
        print('mock: no stats', error)
    mock.terminate()
