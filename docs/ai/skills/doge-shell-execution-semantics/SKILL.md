---
name: doge-shell-execution-semantics
description: Use for doge-shell execution semantics involving pipelines, background jobs, wait/fg/bg, $!, process substitution, re-exec/subshell, ECHILD, lifecycle state, exit status, or FD/PID/PGID ownership.
---

# Doge Shell Execution Semantics

- Start with `rg -n "wait|ECHILD|final_exit_status|wait_jobs|process substitution|reexec|JobLaunchOutcome" dsh/src/process dsh/src/proxy/builtin/jobs dsh/src/shell`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for the execution entry.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Execution details live in `dsh/src/process/` (`wait.rs`, `job.rs`, `job_wait.rs`, `pipeline_status.rs`, `launch_outcome.rs`), `dsh/src/proxy/builtin/jobs/` (`wait/`, `fg.rs`, `bg.rs`), `dsh/src/shell/job_exit.rs`, `dsh/src/shell/job.rs`, and `dsh/src/shell/process_substitution/`.
- Required references: [execution.md](../doge-shell-repo/references/invariants/execution.md) for lifecycle/ownership/harness; [execution-background.md](../doge-shell-repo/references/invariants/execution-background.md) for `$!`, `wait`, `wait -n`, `wait -p`, `fg`/`bg`, and background ownership.
- Compact invariants: the canonical `JobProcess` tree is the lifecycle authority; `Job.state` is a derived summary, never truth. Never invent a final status for an incomplete tree: `Job::final_exit_status()` is the single resolver. ECHILD is a wait observation, never a synthetic completion. Helper/resource ownership is exactly one. Never consume an unrelated child with `waitpid(-1)`. Never record a stopped/incomplete job into completed `OutputHistory`. Never drop/rewrite a no-command pipeline stage.
- Harness mapping: shell semantics go through `dsh/tests/spec/*.toml` + `cargo test -p doge-shell --test shell_contract`; lifecycle through `dsh/src/process/job/lifecycle_property_tests.rs`; FD/PID/PGID/resource ownership through `dsh/tests/resource_contract.rs` plus focused integration tests. Known bugs become XFAIL contracts first (XPASS before removing the marker).
- PTY/raw-terminal/rendering-only work belongs to [doge-shell-process-pty](../doge-shell-process-pty/SKILL.md). Foreground terminal ownership for stopped jobs lives there; lifecycle/status/ownership semantics live here.
- Validate with `cargo test -p doge-shell`; run `scripts/check-execution-authority.py` when `waitpid` sites or `wait_jobs` mutation move.
