"""PTY regression checks for the shell-owned background AI task.
Usage: python3 scripts/verify-background-chat.py /path/to/debug/dogesh
Uses a fake key and loopback stub; no paid or live inference.
"""
from background_chat_harness import (Shell, active, server, os, pathlib, tempfile,
                                     threading, json, subprocess, signal, time)

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
            if kind in ['approval_stale', 'edit_auto', 'edit_empty', 'deny']:
                active.update(tool='edit', args={'path': 'file', 'contents': 'updated'})
            if kind == 'unsupported':
                active.update(tool='mcp_unknown', args={})
            if kind == 'edit_empty':
                active['args']['contents'] = ''
            profile = 'workspace-edit' if kind in ['edit_auto', 'edit_empty'] else 'read-only'
            shell.command(f'chat_task start --profile {profile} -- local check')
            if kind == 'input_read':
                shell.until(lambda: active['requests'] == 1)
                shell.command("echo FOREGROUND_'READY'")
                shell.until(lambda: b'FOREGROUND_READY\r\n' in shell.data)
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
            if kind == 'input_read':
                shell.command('chat_task output')
                shell.until(lambda: b'LOCAL_BACKGROUND_DONE' in shell.data)
            assert active['requests'] == 2, active
            if kind == 'input_read':
                assert 'original' in active['result'], active
            if kind == 'unsupported':
                assert 'unsupported background tool' in active['result'], active
            if kind == 'edit_auto':
                assert (root / 'file').read_text() == 'updated', active
            if kind == 'edit_empty':
                assert (root / 'file').read_text() == '', active
            if kind == 'deny':
                assert (root / 'file').read_text() == 'original', active
            if kind == 'approval_stale':
                assert (root / 'file').read_text() == 'changed externally', active
            print(kind, 'PASS', flush=True)
        finally:
            if active.get('first_gate'):
                active['first_gate'].set()
            shell.close()
for kind in ['input_read', 'edit_auto', 'edit_empty', 'deny', 'approval_stale', 'unsupported']:
    scenario(kind)

def process_gone(pid):
    try:
        os.kill(pid, 0)
        return False
    except ProcessLookupError:
        return True

def background_worker_pid(shell):
    workers = []
    for line in subprocess.check_output(['ps', '-axo', 'pid=,ppid=,command='], text=True).splitlines():
        parts = line.strip().split(None, 2)
        if len(parts) == 3 and parts[1] == str(shell.pid) and '--__dsh-background-worker' in parts[2]:
            workers.append(int(parts[0]))
    assert len(workers) == 1, workers
    return workers[0]

for pending_kind in ['native_write', 'guidance', 'owned_job']:
    active.clear()
    active.update(requests=0, tool='edit', args={'path': 'file', 'contents': 'must not be written'})
    if pending_kind == 'owned_job':
        active.update(tool='execute', args={'command': 'sleep 30'}, next_tool=('edit', {'path': 'file', 'contents': 'must not be written'}))
    with tempfile.TemporaryDirectory(prefix='background-pending-crash-', dir=os.environ.get('TMPDIR', '/var/tmp')) as tmp:
        root = pathlib.Path(tmp)
        if pending_kind == 'guidance':
            (root / 'AGENTS.md').write_text('Use the fixture file for this local test.\n')
        shell = Shell(root, {'AI_CHAT_PROJECT_INSTRUCTIONS': '1'} if pending_kind == 'guidance' else None)
        job_pid = None
        try:
            shell.command('chat_task start --profile read-only -- pending worker loss check')
            status = shell.wait_state('input_required')
            if pending_kind == 'owned_job':
                assert status['approval']['tool'] == 'execute', status
                shell.command(f"chat_task approve {status['task_id']} {status['approval']['request_id']}")
                shell.until(lambda: 'result' in active)
                job_pid = json.loads(active['result'])['pid']
                status = shell.wait_state('input_required')
                assert status['approval']['tool'] == 'edit', status
                assert not process_gone(job_pid), 'managed job should be live before worker loss'
            elif pending_kind == 'guidance':
                assert status['approval']['tool'] == 'project_guidance', status
                assert active['requests'] == 0, 'guidance must be approved before contacting provider'
            else:
                assert status['approval']['tool'] == 'edit', status
            old_task, old_request = status['task_id'], status['approval']['request_id']
            worker_pid = background_worker_pid(shell)
            os.kill(worker_pid, signal.SIGKILL)
            shell.wait_terminal()
            if job_pid is not None:
                shell.until(lambda: process_gone(job_pid), seconds=3)
            shell.until(lambda: process_gone(worker_pid), seconds=3)
            before = len(shell.data)
            shell.command(f'chat_task approve {old_task} {old_request}')
            shell.until(lambda: b'background approval is stale or unknown' in shell.data[before:])
            assert (root / 'file').read_text() == 'original'
            if pending_kind == 'guidance':
                assert active['requests'] == 0, 'dead worker must not resume after stale approval'
            before = len(shell.data)
            shell.command("echo FOREGROUND_'AFTER_WORKER_LOSS'")
            shell.until(lambda: b'FOREGROUND_AFTER_WORKER_LOSS\r\n' in shell.data[before:])
            print('worker_crash_pending_' + pending_kind, 'PASS', flush=True)
        finally:
            # Always exercise parent cleanup, including when an assertion fails.
            shell.close()

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
            worker_pid = background_worker_pid(shell)
            if action == 'cancel':
                shell.command('chat_task cancel')
            elif action == 'worker_crash':
                os.kill(worker_pid, signal.SIGKILL)
            else:
                shell.command('exit')
            shell.until(lambda: process_gone(job_pid))
            shell.until(lambda: process_gone(worker_pid))
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
