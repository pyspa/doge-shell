# Project instructions in `!` chat

`chatgpt/instructions.rs` loads `AGENTS.md` as repository data, below user and
operator instructions. It does not change tools, grants, approvals, or safety
policy. `AI_CHAT_PROJECT_INSTRUCTIONS=off` disables it.

- Discovery walks ancestors only, from the existing tool workspace boundary
  (never above it) or the nearest `.git` directory/pointer file to the target.
  No recursive scan, home-wide configuration, or alternate filenames.
- Documents apply to their directory and descendants, excluding nested Git
  checkouts. Parent comes before child; deeper guidance takes precedence within
  its scope. Sibling guidance never applies to another sibling.
- Each regular UTF-8 file is limited to 8 KiB; an ancestor chain and the combined
  live guidance each have a 32 KiB byte limit. Files exceeding limits are skipped
  whole, with diagnostics. No partial rules. Symlinks, directories, special files,
  invalid UTF-8, and read failures are diagnosed and skipped.
- Reading guidance requires session trust, keyed by source path and content.
  Changed content asks again. The `always` answer also lasts only this session;
  there is no new persistent trust file. Unattended legacy task paths never ask
  or load untrusted guidance. Guidance trust does not authorize writes or commands.
- The model receives source paths, scope, full accepted text, and diagnostics in
  the volatile request context. It is measured by existing request footprint
  accounting, outside the stable system prompt and conversation identity.
- Every request rereads active scopes, catching same-size edits, deletion, and
  creation without relying on mtime. Live guidance is not stored as history.
  Restored conversations and new turns rebuild it. At most 16 scopes per turn.
- `read_file`, `edit`, and `str_replace` check the resolved target directory;
  `execute` checks its resolved cwd. When new or changed guidance has not been
  shown yet, the operation returns a deferral and performs no action. Every call
  in the same batch also defers until the next model request includes the update.
  Existing loop/turn budgets and cancellation remain the bounds on retries.
- Shell command strings and opaque MCP arguments cannot be safely interpreted
  as arbitrary file targets. The context explicitly tells the model to inspect
  instructions in affected directories before those operations. This is guidance,
  not an OS sandbox or a guarantee that the model follows every document.

`doctor ai` lists candidate source paths, byte sizes, and discovery failures
without prompting or sending requests. Chat emits a refresh notice; exact active
sources and trust/limit failures are in the request context. Diagnostics never
print document bodies. Normal tests use stubbed responses, not paid model calls.
