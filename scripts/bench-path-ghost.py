#!/usr/bin/env python3
"""PTY key-to-frame and key-to-ghost measurements with isolated HOME/XDG.

Example: python3 scripts/bench-path-ghost.py /tmp/dogesh-before --output /tmp/before.json
Build release binaries separately. Fixture files are temporary; no user shell/config is run.
The app cache is cold on each launch, while OS filesystem caches may be warm.
"""
import argparse
import errno
import fcntl
import json
import os
import pathlib
import pty
import select
import signal
import statistics
import struct
import tempfile
import termios
import time


def until(fd, needle, timeout=3):
    data = b""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if select.select([fd], [], [], max(0, deadline-time.monotonic()))[0]:
            try:
                chunk = os.read(fd, 1024*1024)
            except OSError as error:
                if error.errno == errno.EIO:
                    break
                raise
            if not chunk:
                break
            data += chunk
            if needle in data:
                return data, time.monotonic()
    raise RuntimeError(f"timeout waiting for {needle!r}: {data[-400:]!r}")


def drain(fd):
    while select.select([fd], [], [], 0.002)[0]:
        try:
            os.read(fd, 1024*1024)
        except OSError:
            return


def stop(pid, fd):
    try:
        os.write(fd, b"\x03")
        time.sleep(0.02)
        os.write(fd, b"exit\r")
        end = time.monotonic()+1
        while time.monotonic() < end:
            if os.waitpid(pid, os.WNOHANG)[0]:
                return
            if select.select([fd], [], [], 0.01)[0]:
                try:
                    os.read(fd, 1024*1024)
                except OSError:
                    pass
        os.kill(pid, signal.SIGTERM)
        os.waitpid(pid, 0)
    finally:
        os.close(fd)


def summary(values):
    values = sorted(values)
    return {"n": len(values), "median_ms": statistics.median(values),
            "p95_ms": values[max(0, int(len(values)*0.95)-1)],
            "min_ms": min(values), "max_ms": max(values)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=pathlib.Path)
    parser.add_argument("--trials", type=int, default=20)
    parser.add_argument("--dirs", type=int, default=64)
    parser.add_argument("--entries", type=int, default=1024)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    args = parser.parse_args()
    binary = str(args.binary.resolve())
    rows = []
    with tempfile.TemporaryDirectory(prefix="dogesh-pty-ghost-") as scratch:
        root = pathlib.Path(scratch)
        paths = []
        for i in range(args.dirs):
            folder = root / f"bin-{i}"
            folder.mkdir()
            for j in range(args.entries):
                file = folder / f"fixture-command-{j:04}"
                file.touch()
                file.chmod(0o755)
            paths.append(str(folder))
        target = pathlib.Path(paths[-1]) / "qz-ghost-target"
        target.touch()
        target.chmod(0o755)
        for trial in range(args.trials):
            home = root / f"home-{trial}"
            (home/"config/dogesh").mkdir(parents=True)
            (home/"config/dogesh/config.lisp").write_text("")
            env = {"HOME": str(home), "PATH": ":".join(paths), "TERM": "xterm-256color",
                   "LANG": "C.UTF-8", "DOGESH_LOG": "off",
                   "XDG_CONFIG_HOME": str(home/"config"), "XDG_DATA_HOME": str(home/"data"),
                   "XDG_STATE_HOME": str(home/"state"), "XDG_CACHE_HOME": str(home/"cache")}
            start = time.monotonic()
            pid, fd = pty.fork()
            if pid == 0:
                os.chdir(home)
                os.execve(binary, [binary], env)
            fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
            try:
                _, prompt_at = until(fd, b"\x1b]133;B")
                drain(fd)
                disk_ready_at_key = (home/"data/dogesh/executable_names.json").exists()
                key_at = time.monotonic()
                os.write(fd, b"q")
                frame, frame_at = until(fd, b"\x1b[?25h")
                while b"q" not in frame:
                    more, frame_at = until(fd, b"\x1b[?25h")
                    frame += more
                if b"ghost-target" in frame:
                    ghost_at = frame_at
                else:
                    _, ghost_at = until(fd, b"ghost-target")
                # A legacy cold synchronous prefix can render before the full
                # background projection settles. Do not mislabel those as warm.
                ready_deadline = time.monotonic()+3
                while not (home/"data/dogesh/executable_names.json").exists():
                    if time.monotonic() >= ready_deadline:
                        raise RuntimeError("PATH projection disk publication did not settle")
                    time.sleep(0.001)
                warm = []
                for _ in range(10):
                    drain(fd)
                    os.write(fd, b"\x15")
                    until(fd, b"\x1b[?25h")
                    drain(fd)
                    tick = time.monotonic()
                    os.write(fd, b"q")
                    _, ready = until(fd, b"ghost-target")
                    warm.append((ready-tick)*1000)
                rows.append({"startup_ms": (prompt_at-start)*1000,
                             "cold_frame_ms": (frame_at-key_at)*1000,
                             "cold_ghost_ms": (ghost_at-key_at)*1000,
                             "disk_ready_at_key": disk_ready_at_key,
                             "warm_ghost_ms": warm})
            finally:
                stop(pid, fd)
    result = {"binary": binary, "path_dirs": args.dirs, "files_per_dir": args.entries,
              "samples": rows,
              "summary": {name: summary([row[name] for row in rows])
                          for name in ["startup_ms", "cold_frame_ms", "cold_ghost_ms"]}}
    result["summary"]["warm_ghost_ms"] = summary([v for row in rows for v in row["warm_ghost_ms"]])
    result["disk_ready_before_key_count"] = sum(row["disk_ready_at_key"] for row in rows)
    args.output.write_text(json.dumps(result, indent=2))
    print(json.dumps({k:v for k,v in result.items() if k != "samples"}, indent=2))


if __name__ == "__main__":
    main()
