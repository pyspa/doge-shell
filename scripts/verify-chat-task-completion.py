"""Live TAB completion: dummy key, loopback provider and real PTY only.
Usage: python3 scripts/verify-chat-task-completion.py /path/to/debug/dogesh
"""
from background_chat_harness import Shell, active, server, os, pathlib, tempfile


try:
    with tempfile.TemporaryDirectory(prefix='task-completion-', dir=os.environ.get('TMPDIR', '/var/tmp')) as tmp:
        root = pathlib.Path(tmp).resolve()
        shell = Shell(root)
        previous = None
        try:
            for attempt in range(2):
                active.clear()
                active.update(requests=0, tool='edit', args={'path': 'file', 'contents': 'must not be written'})
                shell.command('chat_task start --profile read-only -- completion fixture')
                status = shell.wait_state('input_required')
                task, request = status['task_id'], status['approval']['request_id']
                assert task != previous
                if attempt == 0:
                    os.write(shell.fd, b'chat_task approve chat-\t')
                    shell.pump(0.3)
                    assert (root / 'file').read_text() == 'original'
                    assert active['requests'] == 1
                    # Discard the completed line without approving anything.
                    os.write(shell.fd, b'\x15')
                    shell.pump(0.1)
                os.write(shell.fd, b'chat_task deny chat-\t')
                shell.pump(0.3)
                assert (root / 'file').read_text() == 'original'
                assert active['requests'] == 1
                # A stale cached ID or wrong argument replacement leaves the
                # current approval pending, so completion cannot falsely pass.
                os.write(shell.fd, f' {request}\r'.encode())
                shell.wait_state('completed')
                assert (root / 'file').read_text() == 'original'
                previous = task
                print('pending_id' if attempt == 0 else 'replaced_id', 'PASS', flush=True)
        finally:
            shell.close()
finally:
    server.shutdown()
