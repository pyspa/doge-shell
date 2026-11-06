# Completion backslash identity design

## Goal and evidence

Distinct literal names such as `name`, `name\` and `name\\` must all survive
completion deduplication on the supported Linux/macOS platforms. Current remote
develop includes merged PR #86, with no open PR for this issue. Both
`commands::deduplicate_candidates` and `IntegratedCompletionEngine::deduplicate_and_sort`
strip trailing `/` and `\` before comparison. Providers carry raw words, so a
backslash here is filename data, not an escape or a path separator.

Compile regression tests against unchanged production source before fixing it.
Use real temporary files/directories as well as candidate tables to distinguish
provider collection from candidate loss in deduplication. No old binary evidence.

## Decision and boundaries

Only change the normalization predicate in these two deduplication functions:
trim `/`, preserving every literal backslash, including one immediately before
a trailing directory slash. Keep the selected candidate text unchanged.

Preserve existing forward-slash alias merging, basename comparison in the legacy
command/file merger, command-over-file preference, type separation, priority and
history ranking, truncation, cache identity, and shell-word formatting. No new
shared abstraction or enum refactor is needed for two predicates.

Do not modify relative-directory ghost lookup, project-name suggestions, shell
syntax, candidate escaping, dynamic-provider policy or configuration. Broader
questions about nonpath slash normalization are outside this narrow defect.

## Acceptance and self-review

- Zero, one and multiple trailing literal backslashes remain distinct, including
  spaces/Unicode and home/path prefixes, in both merge paths.
- Slash/no-slash aliases for the same directory still deduplicate, including
  directories whose real basename ends in a backslash; the highest-ranked
  integrated candidate and legacy command preference remain authoritative.
- Exact duplicates still collapse; unrelated types do not collapse together.
- Public integrated completion returns all real files and directories on cold
  and repeated requests; unquoted/double-quoted insertion preserves literal names.
- No filesystem/global environment changes beyond isolated temporary fixtures.

The tempting alternative of decoding or canonicalizing keys would conflate raw
candidate identity with insertion syntax and lose typed path spelling. Restricting
this fix to `/` matches the platform contract and avoids that extra authority.
Production reproduction and final Linux/macOS CI must confirm the decision.
