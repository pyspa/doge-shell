"""Deterministic checks over a completed isolated run."""

from __future__ import annotations

import shlex
import re
from pathlib import Path

from .case import Case
from .workspace import bounded_command, git


def run_validation(case: Case, worktree: Path, log_dir: Path, timeout: int) -> dict:
    results = {}
    for index, command in enumerate(case.required_tests + case.required_checkers):
        code, log, timed_out = bounded_command(shlex.split(command), worktree, timeout)
        result = {"exit_code": code, "timed_out": timed_out}
        if command.startswith("cargo test "):
            counts = [int(count) for count in re.findall(r"\brunning (\d+) tests?\b", log)]
            result["tests_executed"] = sum(counts)
            if code == 0 and not any(counts):
                result["exit_code"] = 1
                result["error"] = "cargo test matched no tests"
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
        "forbidden_commands_absent": not any(bad in command for bad in case.forbidden_commands for command in commands),
        "mutation_index_intact": git("ls-files", "-s", cwd=worktree).stdout == index_baseline,
    }
    trace_text = "\n".join(commands)
    checks["expected_route_accessed"] = any(route in trace_text for route in case.expected_routes)
    checks["workspace_test_before_focus"] = not any("cargo test --workspace" in command for command in commands)
    required = case.deterministic_checks
    outcome = all(checks.get(name, False) for name in required)
    warnings = []
    if not checks["expected_route_accessed"]:
        warnings.append("expected route access was not visible in the command trace")
    if not checks["workspace_test_before_focus"]:
        warnings.append("workspace-wide test was used")
    if normalized["command_calls"] > 40:
        warnings.append("command calls exceeded 40")
    return {
        "outcome": {"pass": outcome},
        "correctness": {"score": float(checks["mutation_reverted"] and checks["required_tests_pass"])},
        "process": {"checks": checks, "score": sum(checks.values()) / len(checks)},
        "efficiency": {
            key: normalized[key]
            for key in ("duration_ms", "prompt_tokens", "cached_prompt_tokens", "completion_tokens", "tool_calls", "command_calls", "diff_insertions", "diff_deletions")
        },
        "warnings": warnings,
    }
