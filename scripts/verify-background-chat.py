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
            if active.get('poll'):
                result = json.loads(active['result'])
                if result['status'] == 'running' or not result['stdout_complete']:
                    time.sleep(0.05)
                    assert active['requests'] < 20, 'bounded job polling exceeded'
                    msg = {'role': 'assistant', 'content': None, 'tool_calls': [{'id': f"poll{active['requests']}", 'type': 'function', 'function': {'name': 'job_status', 'arguments': json.dumps({'job_id': result['job_id']})}}]}
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

    def __init__(self, root):
        self.root = root
        self.data = bytearray()
        for d in ['config/dogesh', 'state', 'cache', 'data', 'bin']:
            (root / d).mkdir(parents=True, exist_ok=True)
        (root / '.git').mkdir()
        (root / 'file').write_text('original')
        env = {'PATH': str(root / 'bin') + ':/usr/bin:/bin', 'TERM': 'xterm-256color', 'HOME': str(root), 'XDG_CONFIG_HOME': str(root / 'config'), 'XDG_STATE_HOME': str(root / 'state'), 'XDG_CACHE_HOME': str(root / 'cache'), 'XDG_DATA_HOME': str(root / 'data'), 'AI_CHAT_API_KEY': 'local-stub-key', 'AI_CHAT_PROVIDER': 'api_key', 'AI_CHAT_MODEL': 'local-stub', 'AI_CHAT_BASE_URL': f'http://127.0.0.1:{server.server_port}/v1', 'AI_CHAT_ALLOW_INSECURE_HTTP': '1', 'AI_CHAT_STREAM': '0', 'AI_CHAT_PROJECT_INSTRUCTIONS': 'off', 'AI_CHAT_PROJECT_SKILLS': '0', 'AI_CHAT_SESSION_TTL_SECS': '0'}
        env.update(TASK_TEST_ENV='frozen', UNSELECTED_TEST_ENV='ambient-value')
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

def scenario(kind):
    active.clear()
    active.update(requests=0, tool='read_file', args={'path': 'file'}, delay=0)
    with tempfile.TemporaryDirectory(prefix='background-pty-', dir=os.environ.get('TMPDIR', '/var/tmp')) as tmp:
        root = pathlib.Path(tmp).resolve()
        shell = Shell(root)
        try:
            if kind == 'input_read':
                shell.command('chat_task start --profile sandboxed-test -- unavailable')
                shell.until(lambda: b'no fallback' in shell.data)
                hook_path = root / 'config/dogesh/ai-hooks.json'
                hook_path.write_text(json.dumps({'version': 1, 'hooks': [{'id': 'mandatory', 'events': ['pre-tool-use'], 'command': ['/bin/false']}]}))
                shell.command('chat_task start --profile read-only -- hooks check')
                shell.until(lambda: b'cannot run with active AI hooks' in shell.data)
                assert active['requests'] == 0, 'failed submission contacted provider'
                hook_path.unlink()
                active['first_gate'] = threading.Event()
            if kind in ['approval_stale', 'edit_auto', 'deny']:
                active.update(tool='edit', args={'path': 'file', 'contents': 'updated'})
            if kind == 'unsupported':
                active.update(tool='mcp_unknown', args={})
            profile = 'workspace-edit' if kind == 'edit_auto' else 'read-only'
            shell.command(f'chat_task start --profile {profile} -- local check')
            if kind == 'input_read':
                shell.until(lambda: active['requests'] == 1)
            shell.command('echo FOREGROUND_READY')
            shell.until(lambda: b'FOREGROUND_READY\r\n' in shell.data)
            if kind == 'input_read':
                (root / 'moved').mkdir()
                shell.command('cd moved')
                active['first_gate'].set()
            if kind in ['approval_stale', 'deny']:
                status = shell.wait_state('input_required')
                approval = status['approval']
                task = status['task_id']
                assert (root / 'file').read_text() == 'original'
                if kind == 'approval_stale':
                    (root / 'file').write_text('changed externally')
                    shell.command(f"chat_task approve {task} {approval['request_id']}")
                else:
                    shell.command(f"chat_task deny {task} {approval['request_id']}")
            shell.wait_state('completed')
            shell.command('chat_task output')
            shell.until(lambda: b'LOCAL_BACKGROUND_DONE' in shell.data)
            assert active['requests'] == 2, active
            if kind == 'input_read':
                assert 'original' in active['result'], active
            if kind == 'unsupported':
                assert 'unsupported background tool' in active['result'], active
            if kind == 'edit_auto':
                assert (root / 'file').read_text() == 'updated', active
            if kind == 'deny':
                assert (root / 'file').read_text() == 'original', active
            if kind == 'approval_stale':
                assert (root / 'file').read_text() == 'changed externally', active
            print(kind, 'PASS', flush=True)
        finally:
            if active.get('first_gate'):
                active['first_gate'].set()
            shell.close()
for kind in ['input_read', 'edit_auto', 'deny', 'approval_stale', 'unsupported']:
    scenario(kind)

def process_gone(pid):
    try:
        os.kill(pid, 0)
        return False
    except ProcessLookupError:
        return True
for action in ['cancel', 'worker_crash', 'parent_exit']:
    active.clear()
    active.update(requests=0, tool='execute', args={'command': 'sleep 30'}, hold=True, gate=threading.Event())
    with tempfile.TemporaryDirectory(prefix='background-stop-', dir=os.environ.get('TMPDIR', '/var/tmp')) as tmp:
        shell = Shell(pathlib.Path(tmp))
        try:
            shell.command('chat_task start --profile read-only -- local execution check')
            status = shell.wait_state('input_required')
            approval = status['approval']
            assert approval['command_preview'] == 'sleep 30'
            assert 'without OS sandbox' in approval['execution_warning']
            shell.command(f"chat_task approve {status['task_id']} {approval['request_id']}")
            shell.until(lambda: 'result' in active)
            result = json.loads(active['result'])
            job_pid = result['pid']
            worker_pids = []
            for line in subprocess.check_output(['ps', '-axo', 'pid=,ppid=,command='], text=True).splitlines():
                parts = line.strip().split(None, 2)
                if len(parts) == 3 and parts[1] == str(shell.pid) and ('--__dsh-background-worker' in parts[2]):
                    worker_pids.append(int(parts[0]))
            assert len(worker_pids) == 1, worker_pids
            if action == 'cancel':
                shell.command('chat_task cancel')
            elif action == 'worker_crash':
                os.kill(worker_pids[0], signal.SIGKILL)
            else:
                shell.command('exit')
            shell.until(lambda: process_gone(job_pid))
            shell.until(lambda: process_gone(worker_pids[0]))
            print(action, 'PASS', flush=True)
        finally:
            active['gate'].set()
            shell.close()
active.clear()
active.update(requests=0, tool='execute', args={'command': 'printf "%s|%s\\n" "$TASK_TEST_ENV" "$UNSELECTED_TEST_ENV"; pwd'}, poll=True, first_gate=threading.Event())
with tempfile.TemporaryDirectory(prefix='background-env-', dir=os.environ.get('TMPDIR', '/var/tmp')) as tmp:
    root = pathlib.Path(tmp).resolve()
    shell = Shell(root)
    try:
        shell.command('chat_task start --profile read-only --env TASK_TEST_ENV -- environment check')
        shell.until(lambda: active['requests'] == 1)
        (root / 'moved').mkdir()
        shell.command('cd moved')
        shell.command('export TASK_TEST_ENV=changed')
        active['first_gate'].set()
        status = shell.wait_state('input_required')
        shell.command(f"chat_task approve {status['task_id']} {status['approval']['request_id']}")
        shell.wait_state('completed')
        result = json.loads(active['result'])
        assert result['exit_code'] == 0, result
        assert result['stdout'] == f"frozen|\n{root}\n", result
        print('frozen_env PASS', flush=True)
    finally:
        active['first_gate'].set()
        shell.close()
server.shutdown()
