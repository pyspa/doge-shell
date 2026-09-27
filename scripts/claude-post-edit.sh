#!/usr/bin/env bash
# Thin Claude PostToolUse adapter to the shared path-based checks.
set -uo pipefail
repo_root=${CLAUDE_PROJECT_DIR:-$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)}
file=$(jq -r '.tool_input.file_path // empty' 2>/dev/null) || exit 0
[ -n "$file" ] || exit 0
if ! python3 "$repo_root/scripts/agent-fast-check.py" --path "$file"; then
    exit 2
fi
