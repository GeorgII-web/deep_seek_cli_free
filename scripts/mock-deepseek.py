#!/usr/bin/env python3
"""A local stand-in for the DeepSeek web API, for tests. It never talks to DeepSeek.

Usage: scripts/mock-deepseek.py [port]          (default 8765)
       DSC_API_BASE=http://127.0.0.1:8765 dsc ...
`scripts/pty-drive.py` starts it by itself.

It answers the calls dsc makes, in the real shapes:
  POST /api/v0/chat/create_pow_challenge   a challenge with algorithm "none" (dsc then skips the proof of work)
  POST /api/v0/chat_session/create         a new session id
  POST /api/v0/chat/completion             an answer as the SSE patch stream
  POST /api/v0/file/upload_file            multipart field "file": a file id, status PENDING
  GET  /api/v0/file/fetch_files?file_ids=  the files with their status: PENDING twice, then SUCCESS
  GET  /_stats                             {"requests": N, "completions": N, "uploads": N, "polls": N,
                                            "last": {thinking_enabled, search_enabled, ref_file_ids}}

The proof of work is checked for its target: the header of a completion must name the completion
path, the header of an upload the upload path (each is a separate challenge at DeepSeek).
A completion with ref_file_ids is refused while a file is unknown or not read yet; otherwise the
answer starts with what the mock "read": name, size and first line of each file.
File names steer the upload: "reject" in the name = refused at upload (unsupported type),
"unreadable" = PARSE_FAILED while being read, "slowread" = 6 polls before SUCCESS.

A token that starts with "dead" gets the "invalid token" answer on every call.

What the model "answers" depends on the task (the text after "Task: " in the prompt):
  !<command>        asks to run the shell command; after its result sends the done call
  run <command>     asks to run it; after the result answers with a sentence
  browse <url>      with search_enabled (Browse): reads the page on the model's side and answers
                    (nothing runs locally); without it: asks to fetch the page with curl, like `curl`
  curl <url>        the local way: asks to fetch the page with curl, then answers
  weather <place>   asks to curl wttr.in even with search on (like the real model); dsc then
                    runs a web search instead and the mock answers from its result
  site <host>       asks to curl the site in the other DSML spelling the real model sent (<｜｜DSML｜｜ calls>)
  web <query>       asks for the web tool
A prompt without "Task: " and with search_enabled is a web search of dsc (a chat without the agent
rules): it is answered with three made-up pages and counted in /_stats as "searches".
  read <path>       asks to read the file          write <path>   asks to write "mock\\n" to it
  think <text>      seven lines of reasoning first (with thinking on), then an answer
  slow              a long answer, one word every 0.3 s (to try Esc and scrolling)
  long              80 numbered lines               code           markdown: bold, inline code, a code block, a link
  broken            a shell call without a command  two            two tool calls in one answer
  early             the done call at once (dsc must ask again), then a sentence
  dead              the "invalid token" error       429            HTTP 429 with Retry-After: 1
  anything else     "Mock answer to: <task>"
A tool result as the prompt is answered by what the task planned (done call or a sentence).
Tool calls are written in DSML, DeepSeek's own tool-call markup, like the prompt asks for.
MOCK_DELAY (seconds between chunks, default 0.01) slows everything down.
"""
import base64
import json
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
DELAY = float(os.environ.get('MOCK_DELAY', '0.01'))
FENCE = '```'
RESULT_STARTS = ('Result:', '--- ', 'Written:', 'Denied', 'Error:', 'Your tool call', 'The user interrupted')

COMPLETION_PATH = '/api/v0/chat/completion'
UPLOAD_PATH = '/api/v0/file/upload_file'

stats = {'requests': 0, 'completions': 0, 'uploads': 0, 'polls': 0}
# Per session: the next message id and what to say after a tool result.
sessions = {}
# Uploaded files by id: name, size, first line, how often the status was asked, the polls before SUCCESS.
files = {}


def envelope(biz_data):
    return {'code': 0, 'msg': '', 'data': {'biz_code': 0, 'biz_msg': '', 'biz_data': biz_data}}


def biz_error(code, message):
    """A refusal in the envelope, like DeepSeek sends it: HTTP 200, biz_code not 0."""
    return {'code': 0, 'msg': '', 'data': {'biz_code': code, 'biz_msg': message, 'biz_data': None}}


def pow_target(header):
    """The target_path inside the X-DS-PoW-Response header, '' when missing or unreadable."""
    try:
        return json.loads(base64.b64decode(header or '')).get('target_path') or ''
    except (ValueError, AttributeError):
        return ''


def multipart_file(content_type, body):
    """(file name, bytes) of the form field "file", or None."""
    marker = 'boundary='
    if marker not in content_type:
        return None
    boundary = content_type.split(marker, 1)[1].split(';')[0].strip().strip('"').encode()
    for part in body.split(b'--' + boundary):
        head, sep, data = part.partition(b'\r\n\r\n')
        if not sep or b'name="file"' not in head:
            continue
        name = 'file'
        for item in head.decode(errors='replace').split(';'):
            item = item.strip()
            if item.startswith('filename='):
                name = item[len('filename='):].split('\r\n')[0].strip('"')
        return name, data.removesuffix(b'\r\n')
    return None


def file_status(entry):
    if 'unreadable' in entry['name']:
        return 'PARSE_FAILED' if entry['polls'] >= 1 else 'PENDING'
    return 'SUCCESS' if entry['polls'] >= entry['ready_after'] else 'PENDING'


def file_view(file_id):
    entry = files[file_id]
    return {'id': file_id, 'file_name': entry['name'], 'file_size': entry['size'], 'status': file_status(entry),
            'error_code': 'PARSE_FAILED' if file_status(entry) == 'PARSE_FAILED' else None}


def block(action):
    """One tool call in DSML, like `protocol::dsml`."""
    params = ''.join(
        f'<｜DSML｜parameter name="{key}" string="{"true" if isinstance(value, str) else "false"}">'
        f'{value if isinstance(value, str) else json.dumps(value)}</｜DSML｜parameter>\n'
        for key, value in action.items() if key != 'tool')
    return (f'<｜DSML｜function_calls>\n<｜DSML｜invoke name="{action["tool"]}">\n{params}'
            '</｜DSML｜invoke>\n</｜DSML｜function_calls>')


def plan(prompt, session, search=True):
    """Returns (thinking, answer, seconds between chunks) for a prompt. `search` = the
    request's search_enabled (Browse): without it `browse` fetches the page locally."""
    if prompt.startswith(RESULT_STARTS):
        after = session.pop('after', 'Mock: finished.')
        return '', after, DELAY
    if prompt.startswith('You sent the done call'):
        return '', 'Mock: a real answer after the early done.', DELAY
    # A web search of dsc: a plain question in a chat without the agent rules, search on.
    if search and 'Task: ' not in prompt:
        stats['searches'] = stats.get('searches', 0) + 1
        return '', (f'Mock web search for "{prompt.strip()}": found 3 web pages.\n'
                    '1. Example Domain: this is what the first page says.\n'
                    '2. Example News: what the second one says.\n3. Example Wiki: and the third.'), DELAY
    task = prompt.rsplit('Task: ', 1)[-1].strip()
    done = block({'tool': 'done'})
    if task == 'early':
        return '', done, DELAY
    if task.startswith('!'):
        session['after'] = done
        return '', block({'tool': 'shell', 'cmd': task[1:].strip(), 'why': 'mock: the task asked for it'}), DELAY
    if task.startswith('run '):
        session['after'] = 'Mock: the command finished.'
        return '', 'I will run it.\n\n' + block({'tool': 'shell', 'cmd': task[4:], 'why': 'mock: run then tell'}), DELAY
    if (task == 'browse' or task.startswith('browse ')) and not search:
        url = task[len('browse'):].strip() or 'https://example.org'
        session['after'] = f'Mock: Browse is off, so that is the page at {url} as fetched from your machine.'
        quoted = "'" + url.replace("'", "'\\''") + "'"
        command = f'curl -sSL --max-time 20 {quoted} | head -n 40'
        return '', 'Browse is off: I will fetch the page from your machine.\n\n' + block({'tool': 'shell', 'cmd': command, 'why': 'mock: no web search'}), DELAY
    if task == 'browse' or task.startswith('browse '):
        url = task[len('browse'):].strip() or 'https://example.org'
        return '', (f'Mock: I opened {url} with the browser on my side (nothing ran on your machine).\n\n'
                    '# Example page\nThis is what the page says, as the model read it. See also https://example.org/more.'), DELAY
    if task == 'curl' or task.startswith('curl '):
        url = task[len('curl'):].strip() or 'https://example.org'
        session['after'] = f'Mock: that is the start of the page at {url}, fetched from your machine.'
        quoted = "'" + url.replace("'", "'\\''") + "'"
        command = f'curl -sSL --max-time 20 {quoted} | head -n 40'
        return '', 'I will fetch the page from your machine.\n\n' + block({'tool': 'shell', 'cmd': command, 'why': 'mock: the task asked for it'}), DELAY
    if task.startswith('weather '):
        # Like the real model: curl even with its own search on; dsc searches instead.
        place = task[len('weather '):].strip().removeprefix('in ').strip()
        session['after'] = f'Mock: that is the weather in {place}.'
        url = f"https://wttr.in/{place.replace(' ', '+')}?format=3"
        return '', block({'tool': 'shell', 'cmd': f"curl -s '{url}'", 'why': 'mock: the weather'}), DELAY
    if task.startswith('site '):
        # Like the real model on "check ya.ru": a curl, in the other DSML spelling it sent.
        site = task[len('site '):].strip()
        session['after'] = f'Mock: that is what is on {site}.'
        return '', ('<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ invoke name="shell">\n'
                    f'<｜｜DSML｜｜ parameter name="cmd" string="true">curl -sL \'https://{site}\' | head -c 2000</｜｜DSML｜｜ parameter>\n'
                    f'<｜｜DSML｜｜ parameter name="why" string="true">fetch {site}</｜｜DSML｜｜ parameter>\n'
                    '</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>'), DELAY
    if task.startswith('web '):
        session['after'] = 'Mock: that is what the web search found.'
        return '', block({'tool': 'web', 'query': task[len('web '):].strip(), 'why': 'mock: look it up'}), DELAY
    if task.startswith('read '):
        session['after'] = done
        return '', block({'tool': 'read_file', 'path': task[5:], 'why': 'mock: read'}), DELAY
    if task.startswith('write '):
        session['after'] = 'Mock: written.'
        return '', block({'tool': 'write_file', 'path': task[6:], 'content': 'mock\n', 'why': 'mock: write'}), DELAY
    if task.startswith('think'):
        return 'The user wants a thought.\nFirst: what is asked?\nA sum of two numbers.\nTwo plus two.\nCount: one, two, three, four.\nCheck it again: four.\nSo the answer is four.', 'Mock thought: 4.', DELAY
    if task == 'slow':
        return '', ' '.join(f'word{n}' for n in range(1, 41)), 0.3
    if task == 'long':
        return '', '\n'.join(f'line {n} of the long mock answer' for n in range(1, 81)), DELAY
    if task == 'code':
        return '', ('Here is **bold**, `inline code` and a link https://example.org/docs.\n\n'
                    f'{FENCE}bash\necho "hello"\nls -la\n{FENCE}\n\n# Heading\nDone.'), DELAY
    if task == 'broken':
        return '', block({'tool': 'shell', 'why': 'mock: no command'}), DELAY
    if task == 'two':
        session['after'] = 'Mock: finished.'
        one = block({'tool': 'shell', 'cmd': 'echo first', 'why': 'mock'})
        return '', f'{one}\n\n{block({"tool": "shell", "cmd": "echo second", "why": "mock"})}', DELAY
    return '', f'Mock answer to: {task}', DELAY


def chunks(text):
    """Words with their spaces, like the real stream."""
    out, word = [], ''
    for c in text:
        word += c
        if c in ' \n':
            out.append(word)
            word = ''
    if word:
        out.append(word)
    return out or ['']


class Handler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *args):
        pass

    def send_json(self, value, status=200, headers=None):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        for name, text in (headers or {}).items():
            self.send_header(name, text)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == '/_stats':
            return self.send_json(stats)
        stats['requests'] += 1
        if not self.authorized():
            return
        path, _, query = self.path.partition('?')
        if path == '/api/v0/file/fetch_files':
            stats['polls'] += 1
            ids = [item.split('=', 1)[1] for item in query.split('&') if item.startswith('file_ids=')]
            listed = []
            for file_id in ids:
                if file_id in files:
                    files[file_id]['polls'] += 1
                    listed.append(file_view(file_id))
            return self.send_json(envelope({'files': listed}))
        self.send_json({'error': 'not found'}, 404)

    def authorized(self):
        """False (and the answer sent) when the token is missing or "dead"."""
        token = (self.headers.get('Authorization') or '').removeprefix('Bearer ')
        if token.startswith('dead') or not token:
            self.send_json({'code': 40003, 'msg': 'Authorization Failed (invalid token)', 'data': None})
            return False
        return True

    def upload(self, raw):
        stats['uploads'] += 1
        target = pow_target(self.headers.get('X-Ds-Pow-Response'))
        if target != UPLOAD_PATH:
            return self.send_json(biz_error(40301, f'mock: proof of work for {target or "nothing"}, expected {UPLOAD_PATH}'))
        found = multipart_file(self.headers.get('Content-Type') or '', raw)
        if found is None:
            return self.send_json(biz_error(40001, 'mock: no multipart field "file"'))
        name, data = found
        if 'reject' in name:
            return self.send_json(biz_error(40005, f'mock: unsupported file type: {name}'))
        file_id = f'file-{len(files) + 1}'
        first = data.split(b'\n', 1)[0][:60].decode(errors='replace').strip()
        files[file_id] = {'name': name, 'size': len(data), 'first': first, 'polls': 0,
                          'ready_after': 6 if 'slowread' in name else 2}
        self.send_json(envelope({'id': file_id, 'status': 'PENDING', 'file_name': name, 'file_size': len(data)}))

    def do_POST(self):
        stats['requests'] += 1
        length = int(self.headers.get('Content-Length') or 0)
        raw = self.rfile.read(length)
        if not self.authorized():
            return
        if self.path == UPLOAD_PATH:
            return self.upload(raw)
        body = json.loads(raw or b'{}')
        if self.path == '/api/v0/chat/create_pow_challenge':
            challenge = {'algorithm': 'none', 'challenge': 'mock', 'salt': 'mock', 'difficulty': 0,
                         'expire_at': int(time.time() * 1000) + 300000, 'signature': 'mock'}
            return self.send_json(envelope({'challenge': challenge}))
        if self.path == '/api/v0/chat_session/create':
            session_id = f'mock-{len(sessions) + 1}'
            sessions[session_id] = {'next': 1}
            return self.send_json(envelope({'id': session_id}))
        if self.path == '/api/v0/chat/completion':
            return self.completion(body)
        self.send_json({'error': 'not found'}, 404)

    def completion(self, body):
        stats['completions'] += 1
        # What the last request asked for, so a test can see the switches.
        refs = body.get('ref_file_ids') or []
        stats['last'] = {'thinking_enabled': body.get('thinking_enabled'), 'search_enabled': body.get('search_enabled'),
                         'ref_file_ids': refs}
        target = pow_target(self.headers.get('X-Ds-Pow-Response'))
        if target != COMPLETION_PATH:
            return self.send_json(biz_error(40301, f'mock: proof of work for {target or "nothing"}, expected {COMPLETION_PATH}'))
        for file_id in refs:
            if file_id not in files or file_status(files[file_id]) != 'SUCCESS':
                return self.send_json(biz_error(40004, f'mock: file {file_id} is unknown or not read yet'))
        session = sessions.setdefault(body.get('chat_session_id') or 'mock-0', {'next': 1})
        prompt = body.get('prompt') or ''
        task = prompt.rsplit('Task: ', 1)[-1].strip()
        if task == 'dead':
            return self.send_json({'code': 40003, 'msg': 'INVALID_TOKEN', 'data': None})
        if task == '429':
            return self.send_json({'error': 'too many requests'}, 429, {'Retry-After': '1'})
        thinking, answer, delay = plan(prompt, session, bool(body.get('search_enabled')))
        if refs:
            seen = ', '.join(f"{files[i]['name']} ({files[i]['size']} bytes, first line: {files[i]['first']!r})" for i in refs)
            answer = f'Mock read {len(refs)} attached file(s): {seen}.\n\n{answer}'
        if not body.get('thinking_enabled'):
            thinking = ''
        request_id, response_id = session['next'], session['next'] + 1
        session['next'] += 2

        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Connection', 'close')
        self.end_headers()

        def send(data, event=None):
            text = (f'event: {event}\n' if event else '') + f'data: {json.dumps(data)}\n\n'
            self.wfile.write(text.encode())
            self.wfile.flush()

        try:
            send({'request_message_id': request_id, 'response_message_id': response_id, 'model_type': 'default'}, 'ready')
            parts = chunks(answer)
            if thinking:
                think = chunks(thinking)
                first = {'id': 1, 'type': 'THINK', 'content': think[0]}
                send({'v': {'response': {'message_id': response_id, 'parent_id': request_id, 'status': 'WIP', 'fragments': [first]}}})
                send({'p': 'response/fragments/-1/content', 'o': 'APPEND', 'v': think[1] if len(think) > 1 else ''})
                for piece in think[2:]:
                    time.sleep(delay)
                    send({'v': piece})
                send({'p': 'response/fragments', 'v': [{'id': 1, 'type': 'THINK', 'content': thinking},
                                                       {'id': 2, 'type': 'RESPONSE', 'content': parts[0]}]})
            else:
                first = {'id': 1, 'type': 'RESPONSE', 'content': parts[0]}
                send({'v': {'response': {'message_id': response_id, 'parent_id': request_id, 'status': 'WIP', 'fragments': [first]}}})
            if len(parts) > 1:
                time.sleep(delay)
                send({'p': 'response/fragments/-1/content', 'o': 'APPEND', 'v': parts[1]})
            for piece in parts[2:]:
                time.sleep(delay)
                send({'v': piece})
            send({'p': 'response', 'o': 'BATCH', 'v': [{'p': 'accumulated_token_usage', 'v': 1}, {'p': 'quasi_status', 'v': 'FINISHED'}]})
            send({'p': 'response/status', 'o': 'SET', 'v': 'FINISHED'})
            send({'click_behavior': 'none'}, 'close')
        except (BrokenPipeError, ConnectionResetError):
            # The client stopped the task.
            pass
        self.close_connection = True


if __name__ == '__main__':
    server = ThreadingHTTPServer(('127.0.0.1', PORT), Handler)
    print(f'mock DeepSeek on http://127.0.0.1:{PORT}', flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
