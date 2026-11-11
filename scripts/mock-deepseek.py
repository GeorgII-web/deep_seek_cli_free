#!/usr/bin/env python3
"""A local stand-in for the DeepSeek web API, for tests. It never talks to DeepSeek.

Usage: scripts/mock-deepseek.py [port]          (default 8765)
       DSC_API_BASE=http://127.0.0.1:8765 dsc ...
`scripts/pty-drive.py` starts it by itself.

It answers the three calls dsc makes, in the real shapes:
  POST /api/v0/chat/create_pow_challenge   a challenge with algorithm "none" (dsc then skips the proof of work)
  POST /api/v0/chat_session/create         a new session id
  POST /api/v0/chat/completion             an answer as the SSE patch stream
  GET  /_stats                             {"requests": N, "completions": N}

A token that starts with "dead" gets the "invalid token" answer on every call.

What the model "answers" depends on the task (the text after "Task: " in the prompt):
  !<command>        asks to run the shell command; after its result sends the done block
  run <command>     asks to run it; after the result answers with a sentence
  browse <url>      reads the page on the model's side and answers (nothing runs locally)
  curl <url>        the local way: asks to fetch the page with curl, then answers
  read <path>       asks to read the file          write <path>   asks to write "mock\\n" to it
  think <text>      reasoning first, then an answer
  slow              a long answer, one word every 0.3 s (to try Esc and scrolling)
  long              80 numbered lines               code           markdown: bold, inline code, a code block, a link
  broken            a dsc block that is not JSON    two            two action blocks in one answer
  early             the done block at once (dsc must ask again), then a sentence
  dead              the "invalid token" error       429            HTTP 429 with Retry-After: 1
  anything else     "Mock answer to: <task>"
A tool result as the prompt is answered by what the task planned (done block or a sentence).
MOCK_DELAY (seconds between chunks, default 0.01) slows everything down.
"""
import json
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
DELAY = float(os.environ.get('MOCK_DELAY', '0.01'))
FENCE = '```'
RESULT_STARTS = ('Result:', '--- ', 'Written:', 'Denied', 'Error:', 'Your action block', 'The user interrupted')

stats = {'requests': 0, 'completions': 0}
# Per session: the next message id and what to say after a tool result.
sessions = {}


def envelope(biz_data):
    return {'code': 0, 'msg': '', 'data': {'biz_code': 0, 'biz_msg': '', 'biz_data': biz_data}}


def block(action):
    return f'{FENCE}dsc\n{json.dumps(action)}\n{FENCE}'


def plan(prompt, session):
    """Returns (thinking, answer, seconds between chunks) for a prompt."""
    if prompt.startswith(RESULT_STARTS):
        after = session.pop('after', 'Mock: finished.')
        return '', after, DELAY
    if prompt.startswith('You sent the done block'):
        return '', 'Mock: a real answer after the early done.', DELAY
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
    if task.startswith('read '):
        session['after'] = done
        return '', block({'tool': 'read_file', 'path': task[5:], 'why': 'mock: read'}), DELAY
    if task.startswith('write '):
        session['after'] = 'Mock: written.'
        return '', block({'tool': 'write_file', 'path': task[6:], 'content': 'mock\n', 'why': 'mock: write'}), DELAY
    if task.startswith('think'):
        return 'The user wants a thought. Two plus two is four.', 'Mock thought: 4.', DELAY
    if task == 'slow':
        return '', ' '.join(f'word{n}' for n in range(1, 41)), 0.3
    if task == 'long':
        return '', '\n'.join(f'line {n} of the long mock answer' for n in range(1, 81)), DELAY
    if task == 'code':
        return '', ('Here is **bold**, `inline code` and a link https://example.org/docs.\n\n'
                    f'{FENCE}bash\necho "hello"\nls -la\n{FENCE}\n\n# Heading\nDone.'), DELAY
    if task == 'broken':
        return '', f'{FENCE}dsc\nthis is not json\n{FENCE}', DELAY
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
        self.send_json(stats if self.path == '/_stats' else {'error': 'not found'}, 200 if self.path == '/_stats' else 404)

    def do_POST(self):
        stats['requests'] += 1
        length = int(self.headers.get('Content-Length') or 0)
        body = json.loads(self.rfile.read(length) or b'{}')
        token = (self.headers.get('Authorization') or '').removeprefix('Bearer ')
        if token.startswith('dead') or not token:
            return self.send_json({'code': 40003, 'msg': 'Authorization Failed (invalid token)', 'data': None})
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
        session = sessions.setdefault(body.get('chat_session_id') or 'mock-0', {'next': 1})
        prompt = body.get('prompt') or ''
        task = prompt.rsplit('Task: ', 1)[-1].strip()
        if task == 'dead':
            return self.send_json({'code': 40003, 'msg': 'INVALID_TOKEN', 'data': None})
        if task == '429':
            return self.send_json({'error': 'too many requests'}, 429, {'Retry-After': '1'})
        thinking, answer, delay = plan(prompt, session)
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
