---
name: doge-shell-review
description: Use for independent review of doge-shell changes, especially high-risk execution, safety, lifecycle, portability, or cross-crate changes. Review the diff and invariants without implementing the original change.
---

# Doge Shell Review

1. Do not implement the requested change.
2. Inspect `git diff` and `python3 scripts/agent-review-context.py --json --prompt "<original task>"`.
3. Read only the routed Skill and references needed for changed paths.
4. Look for correctness, resource, lifecycle, portability, and validation gaps.
5. Do not report stylistic preferences as defects.
6. Return concrete findings ordered by severity using `docs/ai/evals/schemas/review-result.schema.json`.
7. For a clean review, return `{"verdict":"clean","findings":[]}`.
