"""Literal !& input and menu/approval PTY regressions. Loopback stub only.
Usage: python3 scripts/verify-ai-task-shortcut.py /path/to/debug/dogesh
"""
from background_chat_harness import (Shell, active, server, os, pathlib, tempfile,
                                     threading)
import fcntl
import signal
import struct
import sys
import termios


def menu(shell, alt=False):
    before = len(shell.data)
    os.write(shell.fd, b'!&' + (b'\x1b\r' if alt else b'\r'))
    shell.until(lambda: b'[q] close' in shell.data[before:])


def key(shell, value):
    os.write(shell.fd, value)
    shell.pump(0.15)


for kind in ['resize', 'literal', 'edit', 'abbreviation', 'approve', 'deny', 'stale', 'truncated', 'close_stop', 'alt_menu', 'compat_background', 'invalid']:
    if len(sys.argv) > 2 and kind != sys.argv[2]:
        continue
    active.clear()
    active.update(requests=0, tool='read_file', args={'path': 'file'})
    with tempfile.TemporaryDirectory(prefix='shortcut-pty-', dir=os.environ.get('TMPDIR', '/var/tmp')) as tmp:
        root = pathlib.Path(tmp).resolve()
        if kind in ['abbreviation', 'resize']:
            config = root / 'config/dogesh'
            config.mkdir(parents=True)
            (config / 'config.lisp').write_text('(pref-status-line t)\n(abbr "co" "checkout")\n')
        shell = Shell(root)
        if kind == 'resize':
            fcntl.ioctl(shell.fd, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
            os.kill(shell.pid, signal.SIGWINCH)
            shell.until(lambda: b'\x1b[1;23r' in shell.data)
        try:
            if kind == 'compat_background':
                shell.command("echo NORMAL_'BACKGROUND' &")
                shell.until(lambda: b'NORMAL_BACKGROUND' in shell.data)
                before = len(shell.data)
                os.write(shell.fd, b"echo ALT_'BACKGROUND'\x1b\r")
                shell.until(lambda: b'ALT_BACKGROUND' in shell.data[before:])
                shell.command('wait')
                assert active['requests'] == 0
            elif kind == 'invalid':
                for text in ['!& --edit', '!& --unknown x', '!&foo', '!& --edit --edit x']:
                    shell.command(text)
                shell.command("echo INVALID_'DONE'")
                shell.until(lambda: b'INVALID_DONE\r\n' in shell.data)
                assert active['requests'] == 0
            elif kind in ['literal', 'edit', 'abbreviation']:
                raw = "調査 'broken $HOME $(touch marker) |! query &"
                if kind == 'abbreviation':
                    raw = "co literal text"
                shell.command(('!& --edit ' if kind == 'edit' else '!& ') + raw)
                shell.wait_state('completed')
                assert raw in active['user_prompts'], active.get('user_prompts')
                assert not (root / 'marker').exists()
                if kind == 'literal':
                    menu(shell)
                    before = len(shell.data)
                    key(shell, b'o')
                    shell.until(lambda: b'LOCAL_BACKGROUND_DONE' in shell.data[before:])
                    key(shell, b'q')
                status = shell.status()
                assert status['profile'] == ('workspace_edit' if kind == 'edit' else 'read_only'), status
            elif kind in ['approve', 'deny', 'stale', 'truncated']:
                active.update(tool='edit', args={'path':'file', 'contents':'updated'})
                if kind == 'truncated':
                    (root / 'file').write_text('b' * 2049)
                shell.command('!& change file')
                shell.wait_state('input_required')
                menu(shell)
                before = len(shell.data)
                key(shell, b'a')
                if kind == 'truncated':
                    shell.until(lambda: b'incomplete preview: approval unavailable' in shell.data[before:])
                    key(shell, b'y')
                    assert (root / 'file').read_text() == 'b' * 2049
                    key(shell, b'a')
                else:
                    shell.until(lambda: b'[y] approve once' in shell.data[before:])
                if kind == 'stale':
                    (root / 'file').write_text('external')
                key(shell, b'n' if kind in ['deny', 'truncated'] else b'y')
                key(shell, b'q')
                shell.wait_state('completed')
                assert (root / 'file').read_text() == {'approve':'updated','deny':'original','stale':'external','truncated':'b' * 2049}[kind]
            else:
                active['first_gate'] = threading.Event()
                shell.command('!& delayed read')
                shell.until(lambda: active['requests'] == 1)
                menu(shell, alt=kind == 'alt_menu')
                if kind == 'resize':
                    fcntl.ioctl(shell.fd, termios.TIOCSWINSZ, struct.pack('HHHH', 12, 40, 0, 0))
                    os.kill(shell.pid, signal.SIGWINCH)
                    shell.pump(0.2)
                # Closing must not cancel, including the Alt+Enter path.
                before = len(shell.data)
                key(shell, b'q')
                if kind == 'resize':
                    shell.until(lambda: b'\x1b[1;11r' in shell.data[before:])
                assert shell.status()['state'] == 'running'
                if kind == 'close_stop':
                    for close_key in [b'\x03', b'\x1b', b'\r']:
                        menu(shell)
                        key(shell, close_key)
                        assert shell.status()['state'] == 'running'
                menu(shell)
                before = len(shell.data)
                key(shell, b's')
                shell.until(lambda: b'[y/N]' in shell.data[before:])
                key(shell, b'n')
                key(shell, b'q')
                assert shell.status()['state'] == 'running'
                menu(shell)
                key(shell, b's')
                key(shell, b'y')
                assert shell.wait_terminal()['state'] == 'cancelled'
                active['first_gate'].set()
            shell.command("echo SHORTCUT_'RESTORED'")
            shell.until(lambda: b'SHORTCUT_RESTORED\r\n' in shell.data)
            print(kind, 'PASS', flush=True)
        finally:
            if active.get('first_gate'):
                active['first_gate'].set()
            shell.close()
server.shutdown()
