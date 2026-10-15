"""Literal !& input and menu/approval PTY regressions. Loopback stub only.
Usage: python3 scripts/verify-ai-task-shortcut.py /path/to/debug/dogesh
"""
from background_chat_harness import (Shell, active, server, os, pathlib, tempfile,
                                     threading)


def menu(shell, alt=False):
    before = len(shell.data)
    os.write(shell.fd, b'!&' + (b'\x1b\r' if alt else b'\r'))
    shell.until(lambda: b'[q] close' in shell.data[before:])


def key(shell, value):
    os.write(shell.fd, value)
    shell.pump(0.15)


for kind in ['literal', 'edit', 'approve', 'deny', 'stale', 'close_stop', 'alt_menu', 'invalid']:
    active.clear()
    active.update(requests=0, tool='read_file', args={'path': 'file'})
    with tempfile.TemporaryDirectory(prefix='shortcut-pty-', dir=os.environ.get('TMPDIR', '/var/tmp')) as tmp:
        root = pathlib.Path(tmp).resolve()
        shell = Shell(root)
        try:
            if kind == 'invalid':
                for text in ['!& --edit', '!& --unknown x', '!&foo', '!& --edit --edit x']:
                    shell.command(text)
                shell.command("echo INVALID_'DONE'")
                shell.until(lambda: b'INVALID_DONE\r\n' in shell.data)
                assert active['requests'] == 0
            elif kind in ['literal', 'edit']:
                raw = "調査 'broken $HOME $(touch marker) |! query &"
                shell.command(('!& --edit ' if kind == 'edit' else '!& ') + raw)
                shell.wait_state('completed')
                assert active['prompt'] == raw, active.get('prompt')
                assert not (root / 'marker').exists()
                status = shell.status()
                assert status['profile'] == ('workspace_edit' if kind == 'edit' else 'read_only'), status
            elif kind in ['approve', 'deny', 'stale']:
                active.update(tool='edit', args={'path':'file', 'contents':'updated'})
                shell.command('!& change file')
                shell.wait_state('input_required')
                menu(shell)
                before = len(shell.data)
                key(shell, b'a')
                shell.until(lambda: b'[y] approve once' in shell.data[before:])
                if kind == 'stale':
                    (root / 'file').write_text('external')
                key(shell, b'n' if kind == 'deny' else b'y')
                key(shell, b'q')
                shell.wait_state('completed')
                assert (root / 'file').read_text() == {'approve':'updated','deny':'original','stale':'external'}[kind]
            else:
                active['first_gate'] = threading.Event()
                shell.command('!& delayed read')
                shell.until(lambda: active['requests'] == 1)
                menu(shell, alt=kind == 'alt_menu')
                # Closing must not cancel, including the Alt+Enter path.
                key(shell, b'q')
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
