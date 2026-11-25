#!/usr/bin/env python3
"""Check each mutation against HEAD in a disposable worktree."""

import argparse
import subprocess
import sys

from agent_eval.case import ROOT, load_case
from agent_eval.grading import validate_command
from agent_eval.workspace import git, isolated_worktree


def smoke_error(case, worktree, timeout):
    commands = case.required_tests + case.required_checkers
    for command in commands:
        result, log = validate_command(command, worktree, timeout)
        if result["exit_code"] != 0:
            reason = result.get("error", f"exit {result['exit_code']}")
            return f"clean HEAD: {command}: {reason}\n{log.rstrip()[-1200:]}".rstrip()

    subprocess.run(["git", "apply", str(case.mutation)], cwd=worktree, check=True)
    detected = False
    for command in commands:
        result, log = validate_command(command, worktree, timeout)
        if "error" in result:
            return f"mutation: {command}: {result['error']}\n{log.rstrip()[-1200:]}".rstrip()
        detected |= result["exit_code"] != 0
    return None if detected else "mutation did not fail any required validation"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--smoke", action="store_true", help="check that validations pass on HEAD and detect the mutation")
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
                error = smoke_error(case, worktree, args.timeout)
                if error:
                    failures.append(f"{case_id}: {error}")
    for failure in failures:
        print(failure, file=sys.stderr)
    check = "pass smoke" if args.smoke else "apply to HEAD"
    print(f"agent eval fixtures: {len(ids) - len(failures)}/{len(ids)} {check}")
    return int(bool(failures))


if __name__ == "__main__":
    sys.exit(main())
