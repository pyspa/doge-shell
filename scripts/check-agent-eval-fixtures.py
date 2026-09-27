#!/usr/bin/env python3
"""Check each mutation against HEAD in a disposable worktree."""

import argparse
import re
import shlex
import subprocess
import sys
from pathlib import Path

from agent_eval.case import ROOT, load_case
from agent_eval.workspace import bounded_command, git, isolated_worktree


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--smoke", action="store_true", help="also confirm each required test fails on its mutation")
    parser.add_argument("--timeout", type=int, default=600, help="per-test smoke timeout in seconds")
    parser.add_argument("--case", action="append", help="check only this case (repeatable)")
    args = parser.parse_args()
    ids = sorted(args.case or [path.stem for path in (ROOT / "docs/ai/evals/tasks").glob("*.json")])
    base = git("rev-parse", "HEAD").stdout.strip()
    failures = []
    for case_id in ids:
        case = load_case(case_id)
        with isolated_worktree(base) as worktree:
            result = subprocess.run(["git", "apply", "--check", str(case.mutation)], cwd=worktree, capture_output=True, text=True)
            if result.returncode:
                failures.append(f"{case_id}: {result.stderr.strip()}")
            elif args.smoke:
                subprocess.run(["git", "apply", str(case.mutation)], cwd=worktree, check=True)
                failed_validation = False
                for command in case.required_tests + case.required_checkers:
                    code, log, timed_out = bounded_command(shlex.split(command), worktree, args.timeout)
                    if timed_out:
                        failures.append(f"{case_id}: smoke timed out after {args.timeout}s")
                        break
                    if command.startswith("cargo test ") and not any(int(n) for n in re.findall(r"\brunning (\d+) tests?\b", log)):
                        failures.append(f"{case_id}: required test matched no tests: {command}")
                    failed_validation |= code != 0
                else:
                    if not failed_validation:
                        failures.append(f"{case_id}: mutation did not fail any required validation")
    for failure in failures:
        print(failure, file=sys.stderr)
    print(f"agent eval fixtures: {len(ids) - len(failures)}/{len(ids)} apply to HEAD")
    return int(bool(failures))


if __name__ == "__main__":
    sys.exit(main())
