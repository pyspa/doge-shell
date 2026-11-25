"""Deterministic checks over a completed isolated run."""

from __future__ import annotations

import shlex
import re
from pathlib import Path

from .case import Case
from .workspace import bounded_command, git


def validate_command(command: str, worktree: Path, timeout: int) -> tuple[dict, str]:
    args = shlex.split(command)
    try:
        code, log, timed_out = bounded_command(args, worktree, timeout)
    except OSError as exc:
        return {"exit_code": None, "timed_out": False, "error": str(exc)}, str(exc)
    result = {"exit_code": code, "timed_out": timed_out}
    if args[:2] == ["cargo", "test"]:
        # libtest's "running N tests" includes ignored tests. Only completed
        # suites tell us how many actually passed or failed.
        plain = re.sub(r"\x1b\[[0-9;]*m", "", log)
        counts = re.findall(r"^test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed;", plain, re.M)
        failed = sum(int(fail) for _, fail in counts)
        result["tests_executed"] = sum(int(passed) for passed, _ in counts) + failed
        if code != 0 and not failed:
            result["error"] = "cargo test exited without a reported failing test"
        elif not result["tests_executed"]:
            result.update(exit_code=1, error="cargo test completed no passing or failing tests")
    if timed_out:
        result.update(exit_code=None, error=f"timed out after {timeout}s")
    elif code is not None and code < 0:
        result["error"] = f"terminated by signal {-code}"
    return result, log


def run_validation(case: Case, worktree: Path, log_dir: Path, timeout: int) -> dict:
    results = {}
    for index, command in enumerate(case.required_tests + case.required_checkers):
        result, log = validate_command(command, worktree, timeout)
        (log_dir / f"validation-{index}.log").write_text(log, encoding="utf-8")
        results[command] = result
    return results


def grade(case: Case, worktree: Path, normalized: dict, validation: dict, index_baseline: str) -> dict:
    paths = normalized["files_changed"]
    commands = normalized["commands"]
    mutation_paths = git("apply", "--numstat", str(case.mutation), cwd=worktree, check=False)
    # git apply --numstat does not depend on whether the patch still applies.
    changed_by_mutation = [line.split("\t")[-1] for line in mutation_paths.stdout.splitlines()]
    restored = bool(changed_by_mutation) and all(
        git("diff", "--quiet", "HEAD", "--", path, cwd=worktree, check=False).returncode == 0
        for path in changed_by_mutation
    )
    checks = {
        "agent_exit_success": normalized["success"],
        "structured_trace_available": normalized["trace_available"],
        "mutation_reverted": restored,
        "required_tests_pass": all(validation.get(cmd, {}).get("exit_code") == 0 for cmd in case.required_tests),
        "required_checkers_pass": all(validation.get(cmd, {}).get("exit_code") == 0 for cmd in case.required_checkers),
        "diff_scope_pass": all(any(path.startswith(prefix) for prefix in case.allowed_change_prefixes) for path in paths),
        "file_count_pass": len(paths) <= case.max_changed_files,
        "working_tree_valid": git("diff", "--check", cwd=worktree, check=False).returncode == 0,
        "forbidden_commands_absent": not any(bad in command for bad in case.forbidden_commands for command in commands) if normalized["trace_available"] else None,
        "mutation_index_intact": git("ls-files", "-s", cwd=worktree).stdout == index_baseline,
    }
    trace_text = "\n".join(commands)
    checks["expected_route_accessed"] = any(route in trace_text for route in case.expected_routes) if normalized["trace_available"] else None
    checks["workspace_test_before_focus"] = not any("cargo test --workspace" in command for command in commands) if normalized["trace_available"] else None
    required = case.deterministic_checks
    trace_only = {"structured_trace_available", "forbidden_commands_absent", "expected_route_accessed", "workspace_test_before_focus"}
    outcome = all(checks.get(name, False) for name in required if name not in trace_only)
    warnings = []
    if checks["expected_route_accessed"] is False:
        warnings.append("expected route access was not visible in the command trace")
    if checks["workspace_test_before_focus"] is False:
        warnings.append("workspace-wide test was used")
    if checks["forbidden_commands_absent"] is False:
        warnings.append("forbidden command was visible in the trace")
    if normalized["command_calls"] is not None and normalized["command_calls"] > 40:
        warnings.append("command calls exceeded 40")
    return {
        "outcome": {"pass": outcome},
        "correctness": {key: checks[key] for key in ("mutation_reverted", "required_tests_pass", "required_checkers_pass", "diff_scope_pass", "file_count_pass", "working_tree_valid", "mutation_index_intact")},
        "process": {"checks": checks},
        "efficiency": {
            key: normalized[key]
            for key in ("duration_ms", "prompt_tokens", "cached_prompt_tokens", "completion_tokens", "tool_calls", "command_calls", "diff_insertions", "diff_deletions")
        },
        "warnings": warnings,
    }
