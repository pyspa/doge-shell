"""Manual PTY smoke tests; use a built dogesh, Python stdlib and loopback only.

Usage: python3 scripts/verify-background-chat.py /path/to/debug/dogesh
No real model/credentials, external service, or user configuration is used.
"""
import http.server
import json
import os
import pathlib
import pty
import select
import signal
import subprocess
import sys
import tempfile
import threading
import time
BIN = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/dogesh')
active = {}

class Stub(http.server.BaseHTTPRequestHandler):

    def log_message(self, *_):
        pass

    def do_POST(self):
        req = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        active['requests'] += 1
        active['prompt'] = next(m['content'] for m in reversed(req['messages']) if m['role'] == 'user')
        system = req['messages'][0]['content']
        assert 'Use only the tools provided' in system
        assert not any(f'- `{name}`:' in system for name in ['search', 'shell_context', 'shell_history', 'skill_manage'])
        names = [tool['function']['name'] for tool in req['tools']]
        assert len(names) == len(set(names)), 'duplicate tool schemas'
        assert set(names) <= {'read_file', 'ls', 'edit', 'str_replace', 'execute', 'observation_read', 'job_status', 'job_output', 'job_cancel'}
        results = [m for m in req['messages'] if m.get('role') == 'tool']
        if not results:
            if active.get('first_gate'):
                active['first_gate'].wait(10)
            msg = {'role': 'assistant', 'content': None, 'tool_calls': [{'id': 'call1', 'type': 'function', 'function': {'name': active['tool'], 'arguments': json.dumps(active['args'])}}]}
            finish = 'tool_calls'
        else:
            active['result'] = results[-1]['content']
            if active.get('hold'):
                active['gate'].wait(10)
            msg = {'role': 'assistant', 'content': 'LOCAL_BACKGROUND_DONE'}
            finish = 'stop'
            if active.get('next_tool'):
                tool, args = active['next_tool']
                msg = {'role': 'assistant', 'content': None, 'tool_calls': [{'id': 'next_call', 'type': 'function', 'function': {'name': tool, 'arguments': json.dumps(args)}}]}
                finish = 'tool_calls'
            if active.get('poll'):
                result = json.loads(active['result'])
                if result['status'] == 'running' or not result['stdout_complete']:
                    time.sleep(0.05)
                    assert active['requests'] < 20, 'bounded job polling exceeded'
                    msg = {'role': 'assistant', 'content': None, 'tool_calls': [{'id': f"poll{active['requests']}", 'type': 'function', 'function': {'name': 'job_status', 'arguments': json.dumps({'job_id': result['job_id'], 'wait_ms': 1000})}}]}
                    finish = 'tool_calls'
        body = json.dumps({'id': 'stub', 'object': 'chat.completion', 'created': 0, 'model': 'local-stub', 'choices': [{'index': 0, 'message': msg, 'finish_reason': finish}], 'usage': {'prompt_tokens': 1, 'completion_tokens': 1, 'total_tokens': 2}}).encode()
        try:
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except BrokenPipeError:
            pass
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Stub)
threading.Thread(target=server.serve_forever, daemon=True).start()

class Shell:

    def __init__(self, root, env_overrides=None):
        self.root = root
        self.data = bytearray()
        for d in ['config/dogesh', 'state', 'cache', 'data', 'bin']:
            (root / d).mkdir(parents=True, exist_ok=True)
        (root / '.git').mkdir()
        (root / 'file').write_text('original')
        env = {'PATH': str(root / 'bin') + ':/usr/bin:/bin', 'TERM': 'xterm-256color', 'HOME': str(root), 'XDG_CONFIG_HOME': str(root / 'config'), 'XDG_STATE_HOME': str(root / 'state'), 'XDG_CACHE_HOME': str(root / 'cache'), 'XDG_DATA_HOME': str(root / 'data'), 'AI_CHAT_API_KEY': 'local-stub-key', 'AI_CHAT_PROVIDER': 'api_key', 'AI_CHAT_MODEL': 'local-stub', 'AI_CHAT_BASE_URL': f'http://127.0.0.1:{server.server_port}/v1', 'AI_CHAT_ALLOW_INSECURE_HTTP': '1', 'AI_CHAT_STREAM': '0', 'AI_CHAT_PROJECT_INSTRUCTIONS': 'off', 'AI_CHAT_PROJECT_SKILLS': '0', 'AI_CHAT_SESSION_TTL_SECS': '0'}
        env.update(TASK_TEST_ENV='frozen', UNSELECTED_TEST_ENV='ambient-value')
        env.update(env_overrides or {})
        self.env = env
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.chdir(root)
            os.execve(BIN, [BIN], env)
        self.pump(0.5)

    def pump(self, seconds=0.1):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if select.select([self.fd], [], [], min(0.05, max(0, end - time.monotonic())))[0]:
                try:
                    b = os.read(self.fd, 65536)
                except OSError:
                    return
                if not b:
                    return
                self.data.extend(b)

    def command(self, text):
        os.write(self.fd, text.encode() + b'\r')
        self.pump(0.15)

    def until(self, predicate, seconds=10):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            self.pump(0.05)
            if predicate():
                return
        raise AssertionError(bytes(self.data[-6000:]).decode(errors='replace'))

    def status(self):
        before = len(self.data)
        self.command('chat_task status')
        self.pump(0.1)
        text = bytes(self.data[before:]).decode(errors='replace')
        start = text.find('{')
        if start < 0:
            return None
        try:
            return json.JSONDecoder().raw_decode(text[start:])[0]
        except ValueError:
            return None

    def wait_state(self, state):
        for _ in range(50):
            status = self.status()
            if status and status['state'] == state:
                return status
            self.pump(0.05)
        raise AssertionError(bytes(self.data[-6000:]).decode(errors='replace'))

    def wait_terminal(self, seconds=3):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            status = self.status()
            if status and status['state'] not in ['running', 'input_required']:
                assert status['approval'] is None, status
                return status
        raise AssertionError('worker loss did not promptly terminate task: ' + bytes(self.data[-6000:]).decode(errors='replace'))

    def close(self):
        self.command('exit')
        self.pump(0.3)
        for _ in range(30):
            done, _ = os.waitpid(self.pid, os.WNOHANG)
            if done:
                os.close(self.fd)
                return
            self.pump(0.05)
        os.kill(self.pid, signal.SIGKILL)
        os.waitpid(self.pid, 0)
        os.close(self.fd)

