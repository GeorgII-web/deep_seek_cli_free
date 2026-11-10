#!/usr/bin/env python3
"""Drives `dsc` chat in a pseudo-terminal with scripted keys and prints the screen.

Usage: scripts/pty-drive.py '<python list of keys>'
A key is a string, or (string, seconds to wait after it). Examples:
  scripts/pty-drive.py '["/help\\r", "abc", "\\x1b\\r", "def", "\\x1b[A", "X"]'    # no request is sent
  scripts/pty-drive.py '[("how much disk space is free on /\\r", 14), ("\\x04", 1)]' # one live task
Keys: \\r Enter, \\x1b\\r Alt+Enter, \\x1b[A up, \\x1b[B down, \\x03 Ctrl-C, \\x04 Ctrl-D, \\x15 Ctrl-U.
Uses DSC_HOME (default var/home) and target/debug/dsc; the screen is 60 columns wide.
"""
import os, pty, sys, time, select, re, struct, fcntl, termios
keys = eval(sys.argv[1])
pid, fd = pty.fork()
if pid == 0:
    os.environ['DSC_HOME'] = os.path.abspath(os.environ.get('DSC_HOME', 'var/home'))
    os.execv('./target/debug/dsc', ['dsc'])
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 60, 0, 0))
out = b''
def pump(t):
    global out
    end = time.time() + t
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try: out += os.read(fd, 65536)
            except OSError: return
pump(0.8)
for k in keys:
    w = 0.25
    if isinstance(k, tuple): k, w = k
    os.write(fd, k.encode()); pump(w)
pump(0.5)
# minimal screen emulation: rows x cols, handles \r \n, CSI A (up), G (column), J (clear down), K(2), m
rows = [[' '] * 60 for _ in range(200)]; y = x = 0
i = 0; s = out.decode('utf8', 'replace')
while i < len(s):
    c = s[i]
    if c == '\x1b':
        m = re.match(r'\x1b\[(\?)?([0-9;]*)([A-Za-z])', s[i:])
        if not m: i += 1; continue
        n = int(m.group(2).split(';')[0] or 1) if m.group(2).split(';')[0].isdigit() or not m.group(2) else 1
        k = m.group(3)
        if not m.group(1):
            if k == 'A': y = max(0, y - n)
            elif k == 'G': x = n - 1
            elif k == 'J':
                rows[y][x:] = [' '] * (60 - x)
                for r in rows[y + 1:]: r[:] = [' '] * 60
            elif k == 'K': rows[y][:] = [' '] * 60
        i += m.end(); continue
    if c == '\r': x = 0
    elif c == '\n': y += 1
    elif c >= ' ':
        if x < 60: rows[y][x] = c
        x += 1
    i += 1
last = max(j for j, r in enumerate(rows) if ''.join(r).strip())
print('\n'.join(''.join(r).rstrip() for r in rows[:last + 1]))
print('cursor', y, x)
