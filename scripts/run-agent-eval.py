#!/usr/bin/env python3
"""Run a curated mutation case in a disposable detached Git worktree."""

from __future__ import annotations

import argparse
import json
import sys
import uuid
from pathlib import Path

from agent_eval import claude, codex
from agent_eval.case import ROOT, load_case
from agent_eval.grading import grade, run_validation
from agent_eval.runner import redact_artifacts, run_process
from agent_eval.trace import NormalizedRunResult, diff_size, trace_accesses
from agent_eval.workspace import agent_diff, apply_mutation, changed_files, git, isolated_worktree


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runner", choices=("codex", "claude"), required=True)
    parser.add_argument("--case", required=True)
    parser.add_argument("--repeat", type=int, default=1)
    parser.add_argument("--model")
    parser.add_argument("--output-dir", type=Path, default=Path("/tmp/doge-shell-agent-evals"))
    parser.add_argument("--timeout", type=int, default=1800)
    parser.add_argument("--validation-timeout", type=int, default=900)
    parser.add_argument("--max-turns", type=int, default=40)
    parser.add_argument("--budget-usd", type=float, default=10.0)
    args = parser.parse_args()
    if not 1 <= args.repeat <= 10 or args.timeout < 1 or args.validation_timeout < 1 or args.max_turns < 1 or args.budget_usd <= 0:
        parser.error("repeat must be 1..10; timeout, validation-timeout, max-turns and budget must be positive")
    output = args.output_dir.resolve()
    if output == ROOT or ROOT in output.parents:
        parser.error("output directory must be outside the source checkout")
    case = load_case(args.case)
    base_sha = git("rev-parse", "HEAD").stdout.strip()
    adapter = codex if args.runner == "codex" else claude
    if args.runner == "claude":
        supported, version, _ = claude.capability()
        if not supported:
            print("SKIP: claude structured trace unsupported by installed version")
            return 0
    else:
        version = codex.version()
    all_passed = True
    run_summaries = []
    for _ in range(args.repeat):
        run_id = f"{case.id}-{args.runner}-{uuid.uuid4().hex[:12]}"
        artifact = output / run_id
        artifact.mkdir(parents=True, exist_ok=False)
        request = {"case_id": case.id, "runner": args.runner, "runner_version": version, "model": args.model, "base_sha": base_sha, "timeout": args.timeout}
        (artifact / "request.json").write_text(json.dumps(request, indent=2) + "\n", encoding="utf-8")
        with isolated_worktree(base_sha) as worktree:
            apply_mutation(worktree, case.mutation)
            index_baseline = git("ls-files", "-s", cwd=worktree).stdout
            prompt = case.prompt + "\n\nこの作業treeだけで修正してください。評価用mutationはindexへ登録済みです。git add/commit/push、network利用、destructive git操作は禁止。最小範囲で検証してください。"
            command = codex.command(worktree, args.model) if args.runner == "codex" else claude.command(worktree, args.model, args.max_turns, args.budget_usd)
            exit_code, duration = run_process(command, prompt, worktree, artifact / "events.jsonl", artifact / "stderr.log", args.timeout)
            data = adapter.normalize(artifact / "events.jsonl")
            (artifact / "final.txt").write_text(data.pop("final"), encoding="utf-8")
            diff = agent_diff(worktree)
            (artifact / "diff.patch").write_text(diff, encoding="utf-8")
            insertions, deletions = diff_size(diff)
            validation = run_validation(case, worktree, artifact, args.validation_timeout)
            (artifact / "validation.json").write_text(json.dumps(validation, indent=2) + "\n", encoding="utf-8")
            normalized = NormalizedRunResult(
                runner=args.runner, runner_version=version, model=args.model or data["model"], case_id=case.id,
                success=exit_code == 0, exit_code=exit_code, duration_ms=duration,
                prompt_tokens=data["prompt_tokens"], cached_prompt_tokens=data["cached_prompt_tokens"],
                completion_tokens=data["completion_tokens"], cost_usd=data["cost_usd"], turn_count=data["turn_count"],
                tool_calls=data["tool_calls"], command_calls=len(data["commands"]), files_changed=changed_files(worktree),
                diff_insertions=insertions, diff_deletions=deletions, validation_commands=list(validation),
                trace_available=data["trace_available"], commands=data["commands"], accesses=trace_accesses(data["commands"]),
            ).to_dict()
            graded = grade(case, worktree, normalized, validation, index_baseline)
            result = {"normalized": normalized, "grade": graded}
            (artifact / "result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
            redact_artifacts(artifact)
            all_passed &= graded["outcome"]["pass"]
            run_summaries.append({"run_id": run_id, "pass": graded["outcome"]["pass"]})
            print(f"{run_id}: {'PASS' if graded['outcome']['pass'] else 'FAIL'} ({artifact})")
    (output / "summary.json").write_text(json.dumps({
        "case_id": case.id, "runner": args.runner, "runner_version": version, "model": args.model,
        "base_sha": base_sha, "runs": run_summaries,
        "success": sum(run["pass"] for run in run_summaries), "total": len(run_summaries),
    }, indent=2) + "\n", encoding="utf-8")
    return 0 if all_passed else 1


if __name__ == "__main__":
    sys.exit(main())
