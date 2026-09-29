# Agent evaluation

`routing-cases.json` checks the deterministic pre-edit router across execution, PTY, completion, parser, and other domains. Add a routing case when a routing mistake is found. Post-edit validation remains owned by `doctor validate`. `tasks/*.json` reintroduce one regression each in a detached temporary worktree. The source checkout is never the agent's working directory. The runner removes each worktree in `finally` and writes artifacts outside the repository (default: `/tmp/doge-shell-agent-evals`).

```sh
python3 scripts/check-agent-eval-schemas.py
python3 scripts/check-agent-eval-fixtures.py
python3 scripts/check-agent-eval-fixtures.py --smoke  # optional: mutation must fail a required validation
python3 scripts/eval-agent-routing.py
python3 scripts/run-agent-eval.py --runner codex --case wait-n-completed-ledger --repeat 3
python3 scripts/run-agent-eval.py --runner opencode --suite smoke --model provider/model
python3 scripts/run-agent-eval.py --runner command --runner-config ~/.config/doge-eval/my-agent.json --case wait-n-completed-ledger
python3 scripts/summarize-agent-evals.py --baseline /tmp/before --candidate /tmp/after
```

The generic command runner accepts an argv array such as `{"argv":["my-agent","--prompt-file","{prompt_file}"]}`. No shell string is executed. Add `env_allowlist` only for credential environment variable names the CLI needs. To pass `--model`, include `{model}` in the argv array. Codex uses `codex exec --json`; Claude probes headless JSON support and falls back to plain print output; OpenCode uses `run --format json` in the isolated worktree, adding `--dir` when supported, and exports a sanitized session when the event stream supplies its ID. Missing usage and trace-derived counts stay `null`. The prompts prohibit commit, push, network use, and destructive Git commands. Run agent evaluations only in a trusted manual environment with suitable credentials. The ordinary PR CI runs only local fixture and schema checks.

Support boundary: `opencode` and generic `command` are local-only runners. The manual workflow `.github/workflows/agent-eval.yml` intentionally remains Codex/Claude-only with least-privilege credentials (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`), because OpenCode relies on provider model/auth configuration with dynamic CLI capability probing and the command runner accepts arbitrary argv, making workflow credentials and installs less bounded and reproducible than the pinned Codex/Claude setup. `python3 scripts/check-agent-eval-support.py` enforces this contract deterministically.

The grade keeps outcome, correctness, process, and efficiency separate, without a combined score. A mutation is restored when its changed path returns exactly to HEAD; a semantically equivalent but different fix may require manual review. Trace-based route access is advisory. Broader tests and command count above 40 are warnings when trace is available. Collect 5–10 runs per model before defining a regression threshold.

Reviewer cases are separate from implementer cases. A reviewer receives the original prompt, router packet, diff, changed paths, recommended validations, and relevant references, without the implementer's conversation. The suite includes historical bug diffs and a clean control. Its true-positive check is a path-and-term heuristic, so inspect the finding text before treating the rate as a quality result. Invalid output or a changed worktree fails the review run.

When a real agent failure appears, reproduce it, add a mutation/case and deterministic grader, change guidance or a checker, then compare before and after. For a guidance change, compare at least three representative cases such as wait-n, completion, and portability. Keep correctness as the gate when token use improves. Run primary Codex and Claude models manually; a scheduled matrix may later use about five representative cases. Do not run a paid model matrix on each PR.
