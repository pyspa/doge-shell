"""Approval display security regressions. Fake key, loopback provider, real PTY.
Usage: python3 scripts/verify-approval-display.py /path/to/debug/dogesh
"""
from background_chat_harness import Shell, active, server, os, pathlib, tempfile


def key(shell, value):
    os.write(shell.fd, value)
    shell.pump(0.1)


def approval_menu(shell):
    before = len(shell.data)
    shell.command('!&')
    shell.until(lambda: b'[q] close' in shell.data[before:])
    key(shell, b'a')


try:
    for case in ['collision', 'controls', 'literal_escape', 'clipped']:
        active.clear()
        active.update(requests=0, tool='edit', args={'path':'file', 'contents':'updated'})
        with tempfile.TemporaryDirectory(prefix='approval-security-', dir=os.environ.get('TMPDIR', '/var/tmp')) as tmp:
            root = pathlib.Path(tmp).resolve()
            shell = Shell(root, {'AI_CHAT_API_KEY':'TOKEN'} if case == 'collision' else None)
            try:
                original = 'original'
                target = root / 'file'
                if case == 'collision':
                    active['args']['contents'] = 'TOKEN=private-dummy-value'
                if case in ['controls', 'literal_escape']:
                    name = 'safe\u202e\x08.txt' if case == 'controls' else 'safe\\u{202e}\\u{8}.txt'
                    target = root / name
                    target.write_text(original)
                    (root / 'safe.txt').write_text('sibling')
                    active['args']['path'] = name
                    if case == 'controls':
                        active['args']['contents'] = 'updated\n\0\u2066\\u{8}'
                if case == 'clipped':
                    original = 'x' * 2049
                    target.write_text(original)
                shell.command('!& propose the fixture edit')
                status = shell.wait_state('input_required')
                request = status['approval']['request_id']
                task = status['task_id']
                if case in ['collision', 'clipped']:
                    assert status['approval'].get('preview_complete', True) is False, status
                    if case == 'collision':
                        assert status['approval']['preview_redacted']
                        assert b'private-dummy-value' not in shell.data
                    shell.command(f'chat_task approve {task} {request}')
                    assert shell.status()['state'] == 'input_required'
                    assert target.read_text() == original
                    before = len(shell.data)
                    approval_menu(shell)
                    shell.until(lambda: b'incomplete preview' in shell.data[before:])
                    assert b'[y] approve once' not in shell.data[before:]
                    key(shell, b'y')
                    assert target.read_text() == original
                    key(shell, b'q')
                    shell.command(f'chat_task deny {task} {request}')
                else:
                    shown = status['approval']['targets'][0]
                    assert '\u202e' not in shown and '\x08' not in shown
                    expected = 'safe\\u{202e}\\u{8}.txt' if case == 'controls' else 'safe\\\\u{202e}\\\\u{8}.txt'
                    assert shown.endswith(expected), shown
                    assert status['approval']['preview_complete']
                    shell.command(f'chat_task approve wrong-task {request}')
                    assert shell.status()['state'] == 'input_required'
                    before = len(shell.data)
                    approval_menu(shell)
                    shell.until(lambda: b'[y] approve once' in shell.data[before:])
                    key(shell, b'y')
                    key(shell, b'q')
                shell.wait_state('completed')
                assert target.read_text() == (active['args']['contents'] if case in ['controls', 'literal_escape'] else original)
                if case in ['controls', 'literal_escape']:
                    assert (root / 'safe.txt').read_text() == 'sibling'
                shell.command("echo SECURITY_'RESTORED'")
                shell.until(lambda: b'SECURITY_RESTORED\r\n' in shell.data)
                print(case, 'PASS', flush=True)
            finally:
                shell.close()
finally:
    server.shutdown()
