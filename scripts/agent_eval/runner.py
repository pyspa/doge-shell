"""Bounded runner process and artifact redaction shared by agent adapters."""

from __future__ import annotations

import os
import signal
import subprocess
import tempfile
import time
from pathlib import Path


def agent_environment(runner: str, extra_keys: list[str] | None = None) -> dict[str, str]:
    """Pass the CLI its selected credential and ordinary toolchain settings only."""
    keep = {
        "HOME", "PATH", "USER", "LOGNAME", "SHELL", "TMPDIR", "TMP", "TEMP",
        "LANG", "LANGUAGE", "TZ", "SSL_CERT_FILE", "SSL_CERT_DIR",
        "HTTPS_PROXY", "HTTP_PROXY", "NO_PROXY", "https_proxy", "http_proxy", "no_proxy",
        "CARGO_HOME", "CARGO_TARGET_DIR", "RUSTUP_HOME", "RUSTFLAGS", "RUSTC_WRAPPER",
    }
    env = {key: value for key, value in os.environ.items() if key in keep or key.startswith("LC_") or key.startswith("XDG_")}
    credentials = {"codex": "OPENAI_API_KEY", "claude": "ANTHROPIC_API_KEY"}
    credential = credentials.get(runner)
    if credential and credential in os.environ:
        env[credential] = os.environ[credential]
    for key in extra_keys or []:
        if key in os.environ:
            env[key] = os.environ[key]
    env.setdefault("CARGO_TARGET_DIR", str(Path(tempfile.gettempdir()) / "doge-shell-agent-eval-target"))
    return env


def redact_artifacts(artifact: Path, extra_keys: list[str] | None = None) -> None:
    names = set(extra_keys or [])
    secrets = [value for key, value in os.environ.items() if (key in names or key.endswith("_API_KEY") or key.endswith("_TOKEN")) and value]
    if not secrets:
        return
    for path in artifact.iterdir():
        if path.is_file() and path.suffix in (".json", ".jsonl", ".log", ".txt", ".patch"):
            content = path.read_text(encoding="utf-8", errors="replace")
            for secret in secrets:
                content = content.replace(secret, "[REDACTED]")
            path.write_text(content, encoding="utf-8")


def run_process(args: list[str], prompt: str, cwd: Path, events: Path, stderr: Path, timeout: int, extra_env: list[str] | None = None) -> tuple[int, int]:
    started = time.monotonic()
    env = agent_environment(Path(args[0]).name, extra_env)
    with events.open("w", encoding="utf-8") as out, stderr.open("w", encoding="utf-8") as err:
        proc = subprocess.Popen(args, cwd=cwd, stdin=subprocess.PIPE, stdout=out, stderr=err, text=True, start_new_session=True, env=env)
        try:
            proc.communicate(prompt, timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.communicate()
            return 124, int((time.monotonic() - started) * 1000)
        except KeyboardInterrupt:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.communicate()
            raise
    return proc.returncode, int((time.monotonic() - started) * 1000)
