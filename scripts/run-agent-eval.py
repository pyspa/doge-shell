#!/usr/bin/env python3
"""Run a curated mutation case in a disposable detached Git worktree."""

from __future__ import annotations

import argparse
import json
import sys
import uuid
from pathlib import Path

from agent_eval.case import ROOT, load_case
from agent_eval.grading import grade, run_validation
from agent_eval.protocol import RunRequest
from agent_eval.runner import redact_artifacts
from agent_eval.runners.claude import ClaudeRunner
from agent_eval.runners.command import CommandRunner
from agent_eval.runners.codex import CodexRunner
from agent_eval.runners.opencode import OpenCodeRunner
from agent_eval.trace import NormalizedRunResult, diff_size, trace_accesses
from agent_eval.workspace import agent_diff, apply_mutation, changed_files, git, isolated_worktree


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runner", choices=("codex", "claude", "opencode", "command"), required=True)
    selector = parser.add_mutually_exclusive_group(required=True)
    selector.add_argument("--case")
    selector.add_argument("--suite", choices=("smoke",))
    parser.add_argument("--runner-config", type=Path)
    parser.add_argument("--repeat", type=int, default=1)
    parser.add_argument("--model")
    parser.add_argument("--agent")
    parser.add_argument("--variant")
    parser.add_argument("--output-dir", type=Path, default=Path("/tmp/doge-shell-agent-evals"))
    parser.add_argument("--timeout", type=int, default=1800)
    parser.add_argument("--validation-timeout", type=int, default=900)
    parser.add_argument("--max-turns", type=int, default=40)
    parser.add_argument("--budget-usd", type=float, default=10.0)
    args = parser.parse_args()
    if args.runner == "command" and not args.runner_config:
        parser.error("--runner command requires --runner-config")
    if args.runner != "opencode" and (args.agent or args.variant):
        parser.error("--agent and --variant require --runner opencode")
    if not 1 <= args.repeat <= 10 or args.timeout < 1 or args.validation_timeout < 1 or args.max_turns < 1 or args.budget_usd <= 0:
        parser.error("repeat must be 1..10; timeout, validation-timeout, max-turns and budget must be positive")
    output = args.output_dir.resolve()
    if output == ROOT or ROOT in output.parents:
        parser.error("output directory must be outside the source checkout")
    cases = [load_case(args.case)] if args.case else [load_case(path.stem) for path in sorted((ROOT / "docs/ai/evals/tasks").glob("*.json"))]
    base_sha = git("rev-parse", "HEAD").stdout.strip()
    runners = {"codex": CodexRunner, "claude": lambda: ClaudeRunner(args.max_turns, args.budget_usd),
               "opencode": OpenCodeRunner, "command": lambda: CommandRunner(args.runner_config)}
    adapter = runners[args.runner]()
    capabilities = adapter.probe()
    if args.model and not capabilities.model_override:
        parser.error(f"--runner {args.runner} does not accept --model")
    version = adapter.version()
    all_passed = True
    run_summaries = []
    for case in cases:
        for _ in range(args.repeat):
            run_id = f"{case.id}-{args.runner}-{uuid.uuid4().hex[:12]}"
            artifact = output / run_id
            artifact.mkdir(parents=True, exist_ok=False)
            request = {"case_id": case.id, "runner": args.runner, "runner_version": version, "capabilities": capabilities.__dict__, "model": args.model, "agent": args.agent, "variant": args.variant, "base_sha": base_sha, "timeout": args.timeout}
            (artifact / "request.json").write_text(json.dumps(request, indent=2) + "\n", encoding="utf-8")
            with isolated_worktree(base_sha) as worktree:
                apply_mutation(worktree, case.mutation)
                index_baseline = git("ls-files", "-s", cwd=worktree).stdout
                prompt = case.prompt + "\n\nこの作業treeだけで修正してください。評価用mutationはindexへ登録済みです。git add/commit/push、network利用、destructive git操作は禁止。最小範囲で検証してください。"
                raw = adapter.run(RunRequest(worktree, artifact, prompt, args.model, args.agent, args.variant), args.timeout)
                exit_code, duration = raw.exit_code, raw.duration_ms
                data = adapter.normalize(raw)
                if args.runner == "opencode" and data.get("session_id"):
                    adapter.export_session(data["session_id"], artifact)
                (artifact / "final.txt").write_text(data.pop("final"), encoding="utf-8")
                diff = agent_diff(worktree)
                (artifact / "diff.patch").write_text(diff, encoding="utf-8")
                insertions, deletions = diff_size(diff)
                validation = run_validation(case, worktree, artifact, args.validation_timeout)
                (artifact / "validation.json").write_text(json.dumps(validation, indent=2) + "\n", encoding="utf-8")
                normalized = NormalizedRunResult(
                    runner=args.runner, runner_version=version, model=args.model or data["model"], agent=args.agent, variant=args.variant, case_id=case.id,
                    success=exit_code == 0, exit_code=exit_code, duration_ms=duration,
                    prompt_tokens=data["prompt_tokens"], cached_prompt_tokens=data["cached_prompt_tokens"],
                    completion_tokens=data["completion_tokens"], cost_usd=data["cost_usd"], turn_count=data["turn_count"],
                    tool_calls=data["tool_calls"], command_calls=len(data["commands"]) if data["trace_available"] else None, files_changed=changed_files(worktree),
                    diff_insertions=insertions, diff_deletions=deletions, validation_commands=list(validation),
                    trace_available=data["trace_available"], commands=data["commands"], accesses=trace_accesses(data["commands"]),
                ).to_dict()
                graded = grade(case, worktree, normalized, validation, index_baseline)
                observed_checks = graded["process"]["checks"]
                normalized["checks_passed"] = [name for name, passed in observed_checks.items() if passed is True]
                normalized["checks_failed"] = [name for name, passed in observed_checks.items() if passed is False and name != "structured_trace_available"]
                result = {"normalized": normalized, "grade": graded}
                (artifact / "result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
                redact_artifacts(artifact, getattr(adapter, "env_allowlist", []))
                all_passed &= graded["outcome"]["pass"]
                run_summaries.append({"run_id": run_id, "pass": graded["outcome"]["pass"]})
                print(f"{run_id}: {'PASS' if graded['outcome']['pass'] else 'FAIL'} ({artifact})")
    (output / "summary.json").write_text(json.dumps({
        "case_id": args.case, "suite": args.suite, "runner": args.runner, "runner_version": version, "model": args.model, "agent": args.agent, "variant": args.variant,
        "base_sha": base_sha, "runs": run_summaries,
        "success": sum(run["pass"] for run in run_summaries), "total": len(run_summaries),
    }, indent=2) + "\n", encoding="utf-8")
    return 0 if all_passed else 1


if __name__ == "__main__":
    sys.exit(main())
