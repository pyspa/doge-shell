#!/usr/bin/env python3
"""Warm cached-path highlighting key-to-frame in an actual PTY.

Uses disposable HOME/XDG/config/cwd, never executes fixture commands.
Example: python3 scripts/bench-cached-path-input.py /tmp/dogesh --output /tmp/input.json
"""
import argparse
import fcntl
import importlib.util
import json
import os
import pathlib
import pty
import re
import struct
import tempfile
import termios
import time

spec = importlib.util.spec_from_file_location(
    "pty_helpers", pathlib.Path(__file__).with_name("bench-path-ghost.py"))
helpers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helpers)


ANSI = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)")

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=pathlib.Path)
    parser.add_argument("--compare-with", type=pathlib.Path, help="alternate both binaries within each trial")
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--trials", type=int, default=10)
    parser.add_argument("--repeats", type=int, default=20)
    args = parser.parse_args()
    binaries = [str(args.binary.resolve())]
    if args.compare_with:
        binaries.append(str(args.compare_with.resolve()))
    results = []
    with tempfile.TemporaryDirectory(prefix="dogesh-path-input-") as scratch:
        root = pathlib.Path(scratch)
        for size in [1, 10000]:
            cwd = root / f"files-{size}"
            cwd.mkdir()
            for index in range(size):
                (cwd / f"file{index:06}").touch()
            basename = f"file{size-1:06}".encode()
            line = b"echo ./" + basename + b";"
            samples_by_binary = {binary: [] for binary in binaries}
            for trial in range(args.trials):
                order = binaries if trial % 2 == 0 else list(reversed(binaries))
                for binary in order:
                    samples = samples_by_binary[binary]
                    home = root / f"home-{size}-{trial}-{binaries.index(binary)}"
                    (home / "config/dogesh").mkdir(parents=True)
                    (home / "config/dogesh/config.lisp").write_text("")
                    env = {"HOME": str(home), "PATH": "/usr/bin:/bin", "TERM": "xterm-256color",
                           "LANG": "C.UTF-8", "DOGESH_LOG": "off"}
                    for kind in ["CONFIG", "DATA", "STATE", "CACHE"]:
                        env[f"XDG_{kind}_HOME"] = str(home / kind.lower())
                    pid, fd = pty.fork()
                    if pid == 0:
                        os.chdir(cwd)
                        os.execve(binary, [binary], env)
                    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
                    try:
                        helpers.until(fd, b"\x1b]133;B")
                        helpers.drain(fd)
                        # Ghost discovery warms the real directory listing first.
                        os.write(fd, b"echo ./fi")
                        helpers.until(fd, b"le000000")
                        warmed_at = time.monotonic()
                        helpers.drain(fd)
                        for _ in range(args.repeats):
                            os.write(fd, b"\x15")
                            helpers.until(fd, b"\x1b[?25h")
                            helpers.drain(fd)
                            os.write(fd, b"\x1b[200~" + line + b"\x1b[201~")
                            helpers.until(fd, basename)
                            helpers.drain(fd)
                            # A comment marker forces analysis without a path ghost.
                            # Keep the trial below half the 2s listing TTL; reject
                            # slow trials rather than mix expired cache into warm data.
                            tick = time.monotonic()
                            os.write(fd, b"#")
                            frame, shown = helpers.until(fd, b"\x1b[?25h")
                            while line + b"#" not in ANSI.sub(b"", frame):
                                more, shown = helpers.until(fd, b"\x1b[?25h")
                                frame += more
                            if shown - warmed_at >= 1:
                                raise RuntimeError("warm trial exceeded 1s; reduce --repeats")
                            samples.append((shown-tick)*1000)
                    finally:
                        helpers.stop(pid, fd)
            for binary, samples in samples_by_binary.items():
                results.append({"files": size, "binary": binary, "samples_ms": samples,
                                "summary": helpers.summary(samples)})
    output = {"binaries": binaries, "trials": args.trials, "repeats": args.repeats,
              "case": "append-comment-to-echo-last-path-semicolon", "results": results}
    args.output.write_text(json.dumps(output, indent=2))
    print(json.dumps({**output, "results": [{k:v for k,v in row.items() if k != "samples_ms"}
                                          for row in results]}, indent=2))


if __name__ == "__main__":
    main()
