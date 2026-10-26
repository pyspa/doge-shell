# Provider capture fixture execution

Choose the recurrent provider capture ETXTBSY failure over the low-impact empty
completion boundary. #88 CI failed provider_accepts_exact_limit_on_both_streams
with Text file busy and passed a same-head retry. That alone did not prove cause.
Current capture_script writes an executable then executes its inode; existing
hook/project fixture guidance identifies concurrent fork inheriting a writer as
a possible ETXTBSY mechanism. Confirm deterministically with an open writer.

Only the capture_script test helper changes: run its shell script as an argument
to /bin/sh, preserving the real command_output_with_timeout implementation, cwd,
1500ms timeout, environment, separate streams, limits and exit status. Production
execution policy stays unchanged. /bin/sh exists on Linux/macOS (existing fixture
pattern). Keep other provider admission/discovery executable fixtures unchanged.

Split a path-taking helper so a regression can keep the script's writer open while
using exactly the capture path. Before the fix it must fail ETXTBSY; after the fix
assert exact stdout/stderr and nonzero exit. Keep existing large/limit/timeout
checks. No sleeps, serialization or retry added. Full builtin tests and CI cover
platform behavior. This demonstrates the mechanism without claiming a particular
CI fork was identified. Existing production ETXTBSY handling is outside scope.

Self-review: a shell wrapper could hide executable admission bugs, but this helper
specifically tests stream capture, not interpreter discovery or executable mode.
Keep actual executable admission fixtures intact. Use script argument (no -c),
retain temporary directory until child capture completes, and quote via argv.
