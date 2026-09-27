#!/usr/bin/env python3
"""Compare baseline and candidate runs by case; missing usage is unknown."""

import argparse
import json
import statistics
from collections import defaultdict
from pathlib import Path


def read_results(root):
    results = defaultdict(list)
    for path in Path(root).rglob("result.json"):
        result = json.loads(path.read_text(encoding="utf-8"))
        normalized = result["normalized"]
        key = (normalized["case_id"], normalized["runner"], normalized.get("model"), normalized.get("agent"), normalized.get("variant"))
        results[key].append(result)
    return results


def median(results, key):
    values = [result["normalized"][key] for result in results if result["normalized"].get(key) is not None]
    return statistics.median(values) if values else None


def summarize(results):
    runs = [run for group in results.values() for run in group]
    return {
        "cases": len(results),
        "runs": len(runs),
        "success": sum(bool(run["grade"]["outcome"]["pass"]) for run in runs),
        "median_prompt_tokens": median(runs, "prompt_tokens"),
        "median_tool_calls": median(runs, "tool_calls"),
        "median_changed_files": statistics.median([len(run["normalized"]["files_changed"]) for run in runs]) if runs else None,
        "median_duration_ms": median(runs, "duration_ms"),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    baseline = read_results(args.baseline)
    candidate = read_results(args.candidate)
    common = set(baseline) & set(candidate)
    before = summarize({key: baseline[key] for key in common})
    after = summarize({key: candidate[key] for key in common})
    groups = {
        " / ".join(str(part) for part in key): {
            "baseline": summarize({key: baseline[key]}),
            "candidate": summarize({key: candidate[key]}),
        }
        for key in sorted(common, key=str)
    }
    output = {"common_case_runner_models_agents_variants": [list(key) for key in sorted(common, key=str)], "baseline": before, "candidate": after, "groups": groups, "warnings": []}
    if before["median_prompt_tokens"] and after["median_prompt_tokens"] and after["median_prompt_tokens"] > before["median_prompt_tokens"] * 1.3:
        output["warnings"].append("candidate prompt-token median exceeds baseline by 30%")
    if args.json:
        print(json.dumps(output, indent=2))
    else:
        for key in ("cases", "success", "median_prompt_tokens", "median_tool_calls", "median_changed_files", "median_duration_ms"):
            print(f"{key}: {before[key]} -> {after[key]}")
        for warning in output["warnings"]:
            print(f"warning: {warning}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
