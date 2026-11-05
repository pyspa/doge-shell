"""Scratch-PTY cold Tab / editing / PATH publication probe (no external services).

Usage: python3 scripts/verify-cold-tab-path.py BASELINE_BINARY CURRENT_BINARY
Creates a bounded temporary fixture. TASK6_ROOT can reuse its bin directory;
every shell still gets a fresh HOME/XDG directory. Results and transcripts stay
in the printed fixture directory. Numbers include a fixed 10 ms key-send delay;
these are controlled local measurements, not universal latency guarantees.
"""
import fcntl
import hashlib
import json
import os
import pathlib
import pty
import re
import select
import signal
import shutil
import struct
import sys
import tempfile
import termios
import time

ROOT = pathlib.Path(os.environ.get('TASK6_ROOT') or tempfile.mkdtemp(prefix='task6-pty-'))
BIN_DIR = ROOT / 'bin'
BIN_DIR.mkdir(exist_ok=True)
ECHO = os.fsencode(shutil.which("echo"))
COUNT = 40000
for index in range(COUNT):
    path = BIN_DIR / f'load-{index:05d}'
    if not path.exists():
        path.touch()
        path.chmod(0o755)
for name in ['zz-cold-alpha', 'zz-cold-beta']:
    path = BIN_DIR / name
    path.write_text('#!/bin/sh\nexit 0\n')
    path.chmod(0o755)


class Shell:
    def __init__(self, binary, label):
        binary = os.path.abspath(binary)
        self.root = ROOT / f'{label}-{time.time_ns()}'
        self.root.mkdir()
        self.data = bytearray()
        self.transcript = bytearray()
        env = {'PATH': ':'.join([str(BIN_DIR)] * 12), 'HOME': str(self.root),
               'TERM': 'xterm-256color', 'DOGESH_COMPLETION_FRAMEWORK': 'inline'}
        for key, sub in [('XDG_CONFIG_HOME', 'config'), ('XDG_STATE_HOME', 'state'),
                         ('XDG_CACHE_HOME', 'cache'), ('XDG_DATA_HOME', 'data')]:
            (self.root / sub).mkdir()
            env[key] = str(self.root / sub)
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.chdir(self.root)
            os.execve(binary, [binary], env)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 120, 0, 0))
        self.until(lambda: b'\x1b]133;B' in self.data)

    def pump(self, timeout=.005):
        if select.select([self.fd], [], [], timeout)[0]:
            chunk = os.read(self.fd, 262144)
            self.data.extend(chunk)
            self.transcript.extend(chunk)
            # An actual terminal answers DSR. Leaving it unanswered adds a
            # cursor-query timeout unrelated to PATH scanning.
            for _ in range(chunk.count(b'\x1b[6n')):
                self.send(b'\x1b[1;1R')

    def plain(self):
        text = re.sub(rb'\x1b\[[0-9;?]*[A-Za-z]', b'', bytes(self.data))
        return re.sub(rb'\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)', b'', text)

    def until(self, predicate, seconds=20):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.pump()
            if predicate():
                return
        raise AssertionError(bytes(self.data[-2000:]))

    def send(self, data):
        os.write(self.fd, data)

    def wait(self, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.pump()

    def echo_buffer(self, expected):
        self.data.clear()
        self.send(b'\x01' + ECHO + b' \x05\r')
        self.until(lambda: re.search(rb'[\r\n]+' + re.escape(expected) + rb'[\r\n]+', self.plain()))
        self.until(lambda: b'\x1b]133;B' in self.data)

    def query(self):
        self.data.clear()
        self.send(b'zz-cold-')
        self.until(lambda: b'zz-cold-' in self.plain())

    def menu(self):
        self.data.clear()
        start = time.monotonic()
        self.send(b'\t')
        self.until(lambda: b'zz-cold-alpha' in self.data and b'zz-cold-beta' in self.data)
        return (time.monotonic() - start) * 1000

    def close(self):
        (self.root / 'transcript.bin').write_bytes(self.transcript)
        os.kill(self.pid, signal.SIGTERM)
        os.waitpid(self.pid, 0)
        os.close(self.fd)


results = []
for binary in sys.argv[1:]:
    digest = hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest()
    for run in range(3):
        shell = Shell(binary, pathlib.Path(binary).name)
        try:
            shell.query()
            shell.data.clear()
            start = time.monotonic()
            shell.send(b'\t')
            time.sleep(.01)
            shell.send(b'Q')
            shell.until(lambda: b'zz-cold-Q' in shell.plain())
            cold_ms = (time.monotonic() - start) * 1000
            shell.wait(2)
            shell.echo_buffer(b'zz-cold-Q')  # actual buffer, not an old redraw
            shell.query()
            ready_ms = shell.menu()
            shell.send(b'\x1b')
            shell.wait(.15)  # distinguish bare Escape from an Alt chord
            shell.send(b'R')
            shell.until(lambda: b'zz-cold-R' in shell.plain())
            shell.echo_buffer(b'zz-cold-R')
            shell.query()
            hot_ms = shell.menu()
            shell.send(b'\x1b')
            shell.wait(.15)
            result = {'binary': str(pathlib.Path(binary).resolve()), 'sha256': digest,
                      'run': run, 'transcript': str(shell.root / 'transcript.bin'),
                      'cold_tab_edit_ms': round(cold_ms, 3),
                      'ready_tab_display_ms': round(ready_ms, 3),
                      'hot_tab_display_ms': round(hot_ms, 3),
                      'edited_buffer_echo': True, 'escape_buffer_echo': True,
                      'ready_candidates': ['zz-cold-alpha', 'zz-cold-beta']}
            results.append(result)
            print(json.dumps(result), flush=True)
        finally:
            shell.close()

# Exercise a PATH switch while the old startup scan can still be in flight.
shell = Shell(sys.argv[-1], 'path-switch')
try:
    new_bin = shell.root / 'new-bin'
    new_bin.mkdir()
    for name in ['zz-fresh-one', 'zz-fresh-two']:
        path = new_bin / name
        path.write_text('#!/bin/sh\nexit 0\n')
        path.chmod(0o755)
    shell.query()
    shell.send(b'\t')
    time.sleep(.01)
    shell.send(b'\x15PATH=' + str(new_bin).encode() + b'\r')
    shell.until(lambda: b'\x1b]133;D' in shell.data)
    shell.wait(2)
    shell.data.clear()
    shell.send(b'zz-\t')
    shell.until(lambda: b'zz-fresh-one' in shell.data and b'zz-fresh-two' in shell.data)
    assert b'zz-cold-alpha' not in shell.data and b'zz-cold-beta' not in shell.data
    shell.send(b'\x1b')
    shell.wait(.15)
    shell.send(b'Q')
    shell.echo_buffer(b'zz-Q')
    results.append({'path_switch_excludes_old_names': True, 'edited_buffer_echo': True})
finally:
    shell.close()

(ROOT / 'results.json').write_text(json.dumps({'root': str(ROOT), 'entries': COUNT + 2,
    'path_repetitions': 12, 'terminal': 'xterm-256color 120x32', 'results': results}, indent=2))
print(ROOT, flush=True)
