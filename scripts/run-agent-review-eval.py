#!/usr/bin/env python3
"""Evaluate independent reviewers against historical mutation diffs."""

import argparse
import json
import sys
import uuid
from pathlib import Path

from agent_eval import claude, codex
from agent_eval.case import ROOT, load_case
from agent_eval.runner import redact_artifacts, run_process
from agent_eval.workspace import apply_mutation, git, isolated_worktree
from agent_routing import load_routes, route_context

REVIEW_CASES = {
    "review-echild": ("echild-synthetic-status", "dsh/src/process/process.rs", ("ECHILD", "NoChild", "synthetic")),
    "review-wait-ownership": ("wait-n-completed-ledger", "dsh/src/proxy/builtin/jobs/wait/mod.rs", ("completed", "ledger", "status")),
    "review-pipeline-partial-completion": ("pipeline-partial-completion", "dsh/src/process/pipeline_status.rs", ("incomplete", "partial", "未完了")),
    "review-runtime-authority": ("runtime-authority", "dsh/src/utils/editor.rs", ("snapshot", "runtime", "環境")),
    "review-portability": ("platform-user-completion", "dsh/src/completion/generators/user.rs", ("macOS", "cfg", "Linux")),
    "review-clean": ("echild-synthetic-status", None, ()),
}


def parse_review(text):
    try:
        result = json.loads(text)
    except json.JSONDecodeError:
        return None
    if not isinstance(result, dict) or result.get("verdict") not in ("clean", "changes-requested") or not isinstance(result.get("findings"), list):
        return None
    if set(result) != {"verdict", "findings"}:
        return None
    for finding in result["findings"]:
        if not isinstance(finding, dict) or not {"severity", "path", "summary"} <= set(finding):
            return None
        if finding["severity"] not in ("critical", "high", "medium", "low") or not isinstance(finding["path"], str) or not isinstance(finding["summary"], str):
            return None
    if (result["verdict"] == "clean") != (not result["findings"]):
        return None
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runner", choices=("codex", "claude"), required=True)
    parser.add_argument("--case", choices=sorted(REVIEW_CASES), required=True)
    parser.add_argument("--model")
    parser.add_argument("--output-dir", type=Path, default=Path("/tmp/doge-shell-agent-reviews"))
    parser.add_argument("--timeout", type=int, default=900)
    args = parser.parse_args()
    output = args.output_dir.resolve()
    if output == ROOT or ROOT in output.parents:
        parser.error("output directory must be outside the source checkout")
    case_id, expected_path, terms = REVIEW_CASES[args.case]
    case = load_case(case_id)
    adapter = codex if args.runner == "codex" else claude
    if args.runner == "claude":
        supported, version, _ = claude.capability()
        if not supported:
            print("SKIP: claude structured trace unsupported by installed version")
            return 0
    else:
        version = codex.version()
    artifact = output / f"{args.case}-{args.runner}-{uuid.uuid4().hex[:12]}"
    artifact.mkdir(parents=True, exist_ok=False)
    with isolated_worktree(git("rev-parse", "HEAD").stdout.strip()) as worktree:
        if expected_path:
            apply_mutation(worktree, case.mutation)
        index_baseline = git("ls-files", "-s", cwd=worktree).stdout
        diff = git("diff", "HEAD", cwd=worktree).stdout
        (artifact / "diff.patch").write_text(diff, encoding="utf-8")
        routes = route_context(load_routes(ROOT / "docs/ai/agent-routing.json"), case.prompt, [expected_path] if expected_path else [])
        packet = {
            "original_prompt": case.prompt,
            "router": routes,
            "changed_files": [expected_path] if expected_path else [],
            "validation": {"status": "not run", "recommended": case.required_tests + case.required_checkers},
        }
        (artifact / "request.json").write_text(json.dumps(packet, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        prompt = (
            "次の変更を独立reviewしてください。実装やfile editはしないでください。"
            "関連invariantだけを確認し、docs/ai/evals/schemas/review-result.schema.json形式のJSONのみ返してください。"
            f"\nReview packet: {json.dumps(packet, ensure_ascii=False)}\nDiff:\n{diff}"
        )
        command = codex.command(worktree, args.model) if args.runner == "codex" else claude.command(worktree, args.model, 20, 5.0)
        if args.runner == "codex":
            command[command.index("workspace-write")] = "read-only"
        else:
            command[command.index("acceptEdits")] = "plan"
        exit_code, duration_ms = run_process(command, prompt, worktree, artifact / "events.jsonl", artifact / "stderr.log", args.timeout)
        read_only = (
            git("diff", "--quiet", cwd=worktree, check=False).returncode == 0
            and git("ls-files", "-s", cwd=worktree).stdout == index_baseline
            and not git("ls-files", "--others", "--exclude-standard", cwd=worktree).stdout.strip()
        )
        normalized = adapter.normalize(artifact / "events.jsonl")
        review = parse_review(normalized["final"])
        (artifact / "final.txt").write_text(normalized["final"], encoding="utf-8")
        if review is not None:
            (artifact / "review.json").write_text(json.dumps(review, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        findings = review["findings"] if review else []
        def matches(finding):
            summary = str(finding.get("summary", "")) + " " + str(finding.get("invariant", ""))
            return finding.get("path") == expected_path and any(term.casefold() in summary.casefold() for term in terms)
        valid_run = exit_code == 0 and normalized["trace_available"] and read_only and review is not None
        true_positive = int(valid_run and any(matches(f) for f in findings))
        false_positive = sum(not matches(f) for f in findings) if valid_run else 0
        result = {
            "review_case": args.case, "runner": args.runner, "runner_version": version,
            "model": args.model or normalized["model"], "exit_code": exit_code, "duration_ms": duration_ms,
            "review": review, "valid_run": valid_run, "true_positive": true_positive, "false_negative": int(bool(expected_path) and true_positive == 0),
            "false_positive": false_positive, "trace_available": normalized["trace_available"], "read_only": read_only,
        }
        (artifact / "result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
        redact_artifacts(artifact)
        print(f"{args.case}: TP={true_positive} FN={result['false_negative']} FP={false_positive} ({artifact})")
    return int(not valid_run or result["false_negative"] or result["false_positive"])


if __name__ == "__main__":
    sys.exit(main())
