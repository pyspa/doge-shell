# Background chat tasks — implementation plan

Status: implemented locally on `improve/background-chat-task`, based on develop
7518986. Not published. PRs #47–#49 are separate fixes and are not duplicated
here. The ordered plan below records the design and its acceptance gates.

## User contract

`chat_task start --profile read-only -- <prompt>` returns a task ID immediately.
One shell owns at most one active task. Normal shell input continues while the
task runs or waits for approval. `status`, `output`, `cancel`, `approve <task-id> <request-id>` and
`deny <task-id> <request-id>` operate on that task. `restrict read-only` revokes pending grants.
The initial roots are the canonical cwd at submission; there is no root-widening
command. `--env NAME` explicitly adds an exported command environment name.
Only one task is retained; starting another finished task replaces its output.

The task has a branch of the conversation and frozen cwd/config/exported
environment. It neither consumes nor updates the foreground conversation's
persisted slot. There is no restart recovery, automatic retry/replay, daemon,
new model loop, or nested-agent scheduler. Foreground `!` remains independent.

## Architecture and ownership

Reuse `execute_chat_message`/`chat_with_tools` in a fresh internal helper, booted
from a filtered snapshot without running config.lisp. Reuse the re-exec FD
reservation, payload limit and version validation. A private inherited Unix
socketpair carries framed control messages; close-on-exec prevents delegation
to command children. No credential, prompt or environment value travels on argv.
The worker uses a memory-only conversation backend; ordinary debug/panic logs
must not persist its data before the internal entrypoint.

The parent supervisor owns command `AgentJobs`, including their process groups,
and handles typed file operations plus execute/job-status/output/cancel RPCs. The worker never owns
external command children. This avoids orphan groups if the worker crashes and
avoids serializing the full ShellProxy interface. Main-thread UI owns approvals;
the supervisor never reads a terminal. Output is bounded and redacted, continues
draining after the display cap, and is printed only by a user command or safe
REPL event. Exactly one owner reaps each worker and command child.

## Permission contract

Task scope is host-owned: ID, permission generation, canonical cwd/read/write
roots, selected exported environment names/values, tool network policy, and
time/token/tool/output budgets. Model credentials belong only to provider I/O;
they are not command environment grants. Parent loose/always/command allowlists,
AGENTS.md and skills cannot widen a task profile.

- `read-only`: automatic typed reads/listing within fixed roots; no automatic execute.
- `workspace-edit`: adds native edits within explicit roots. Sensitive files,
  credential/config/hooks/skill execution and unsupported tools still require
  separate handling; writing data never grants permission to execute it.
- `sandboxed-test`: arbitrary code is eligible for automatic execution only
  after a host-owned effective isolation probe succeeds. This initial version
  deliberately rejects the profile: no effective cross-platform probe is
  implemented. Package/version detection is insufficient; there is no fallback.

Admission runs before hooks, discovery or dispatch, even for tools omitted from
the advertised schema. MCP, cron, arbitrary hooks and unsupported subprocess
tools are unavailable in the initial runner. Existing mandatory hooks must not
silently disappear: reject unsupported configured hooks at task submission.
Use descriptor-relative file access with symlink and write-hardlink checks.
File policy revalidates actual targets; writes also validate their preimage.

Paths outside the fixed roots are denied, including command cwd. A supported
operation outside its automatic profile changes only the task to input_required.
Manual command approval explicitly warns that ordinary shell execution has no
OS confinement: typed-tool roots do not constrain an approved shell command.
Automatic shell execution is unavailable without verified sandboxing. Commands
and manually approved new file contents are capped at 2048 bytes so their
complete masked text fits the approval preview. Native file operations have a
1 MiB cap. Writes reject symlink ancestors and multi-link existing files.
Preimage and actual data checks detect stale/ABA changes; they are not an atomic
compare-and-swap guarantee against a concurrent malicious filesystem writer.
Approval is a one-shot capability bound to task ID, request ID, permission
generation, complete normalized operation, cwd/targets and write preimage/content.
Do not use a weak digest or a pathname-only key as operation identity. Revalidate
after approval; changed operations and stale replies fail closed. Cancel, expiry,
permission restriction and broker EOF revoke pending approvals. Restriction
stops running commands that no longer fit the new scope; no claim is made that
an already-running sandbox can have its policy narrowed in place.

## Ordered implementation and acceptance gates

1. Pure scope/profile/admission/approval state: prove unknown operations deny,
   profiles cannot widen themselves, stale/cross-task/changed/reused approvals
   fail, and restriction/cancel/expiry revoke grants. No public runner yet.
2. Internal worker protocol, memory-only session, frozen filtered snapshot,
   bounded redacted capture and single-owner cancel/reap. Scripted client tests
   prove prompt returns, cwd/env remain frozen, foreground session is unchanged,
   no terminal input is consumed, and worker loss or normal parent exit leaves no command group.
3. Native typed tools and approval broker, checked before every effect. Prove
   read/write root escape, symlink/hardlink/preimage changes, omitted tool calls,
   hooks/MCP and secret logs cannot bypass admission. Wire main UI after these
   tests, including deny/cancel while a foreground shell command is running.
4. Parent-owned execute jobs and sandboxed-test profile. Effective Linux/macOS
   confinement probes must block outside reads/writes and prohibited network;
   unavailable confinement remains an explicit unavailable profile. No install
   is part of this work. Reuse existing budgets and process supervision.

Verification uses the smallest related Rust tests, capability/authority/
portability checkers and a dedicated PTY with a local scripted provider. No paid
inference. Consolidate overlapping tests; do not add a benchmark or agent-eval
framework. Each integration stage gets independent review before the next one.

## Implemented limits and validation

Budgets: 10 minutes, 100 parent tool requests, 64k turn tokens and 8 MiB transferred
output. The display tail is 256 KiB, redacted before trimming. Token usage is
reported at worker completion; `tokens_reported: false` distinguishes an active
or crashed worker from measured zero. Provider usage may exceed the remaining
budget in one response; the existing model loop checks between responses.

Cancellation, worker EOF/crash and normal shell destruction cancel owned jobs
and reap the exact worker PID. Abrupt parent death (SIGKILL, host crash) cannot
run a parent-thread destructor and does **not** guarantee command cleanup. No
watchdog/daemon is introduced. Sandboxed automatic execution remains unavailable.
Unsupported tools (MCP, search, cron, history, skill mutation) are omitted from
the background schema and rejected even when called anyway. Personal skill
resources outside the fixed workspace cannot be read by background native tools.

Regression coverage includes the pure permission matrix, framing/timeouts,
conversation memory isolation, actual read/write byte matching, preimage/ABA
changes and split-stream secret masking. On Linux, the loopback-only
`scripts/verify-background-chat.py` exercises live REPL input while the provider
waits, frozen cwd/selected environment, native edit/deny/stale approval, cancellation, worker crash and
normal parent exit. Run it with a freshly built dogesh path argument. This is a
scripted-provider check, not real-model evaluation. macOS execution is unverified;
portable source checks alone do not establish runtime behavior there.

Local verification (Linux, 2026-10-15): `scripts/check.sh` completed successfully,
including workspace Clippy with warnings denied and workspace tests (4500 passed,
9 ignored, no failures). The saved loopback PTY script passed nine scenarios,
including selected environment values remaining frozen after foreground export
changes. Independent reviews covered lifecycle/protocol and admission/native I/O;
reported issues were fixed before this verification. No real-model inference or
macOS runner was used. No remote branch, PR or merge is part of this local step.
