#!/usr/bin/env bash
#
# Claude Code PostToolUse hook for Edit/Write.
#
# Reads the hook payload on stdin and gives immediate, build-free feedback on
# the file that was just written:
#   *.rs               -> format it (only when it is actually unformatted)
#   completions/*.json -> validate it, without modifying it
#
# Exit 2 makes the message on stderr blocking feedback for the agent.

set -uo pipefail

repo_root=${CLAUDE_PROJECT_DIR:-$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)}

payload=$(cat)
file=$(printf '%s' "$payload" | jq -r '.tool_input.file_path // empty' 2>/dev/null)
[ -n "$file" ] || exit 0
[ -f "$file" ] || exit 0

case "$file" in
    *.rs)
        command -v rustfmt >/dev/null 2>&1 || exit 0
        # Only rewrite when needed, so the file mtime usually stays untouched.
        if ! rustfmt --edition 2024 --check "$file" >/dev/null 2>&1; then
            rustfmt --edition 2024 "$file" >/dev/null 2>&1
        fi
        ;;
    "$repo_root"/completions/*.json | completions/*.json)
        command -v jq >/dev/null 2>&1 || exit 0
        schema="$repo_root/command-completion-schema.json"
        [ -f "$schema" ] || exit 0

        allowed=$(jq -c '[.definitions.ArgumentType.oneOf[]
                          | select(.title == "Dynamic Type")
                          | .properties.data.properties.provider.enum[]]' "$schema" 2>/dev/null)
        [ -n "$allowed" ] || exit 0

        stem=$(basename "$file" .json)
        if ! errors=$(jq -r --argjson allowed "$allowed" --arg stem "$stem" '
              [
                (if .command == $stem then empty
                 else "`command` is \"" + (.command | tostring) + "\" but the file is named \"" + $stem + ".json\"" end),

                ([.. | objects | select(.type? == "Choice") | select((.data | type) != "array")]
                 | if length == 0 then empty
                   else "Choice data must be an array of strings, not \(.[0].data | type)" end),

                ([.. | objects | select(.type? == "Script")]
                 | if length == 0 then empty
                   else "built-in completion definitions must not use the Script type" end),

                ([.. | objects | select(.type? == "Dynamic") | .data.provider // "<missing>"]
                 | map(select(. as $p | ($allowed | index($p)) | not))
                 | unique
                 | if length == 0 then empty
                   else "unknown dynamic provider(s): " + join(", ") end)
              ] | .[]
            ' "$file" 2>&1) || [ -n "$errors" ]; then
            echo "completion definition rejected: $file" >&2
            printf '%s\n' "$errors" | sed 's/^jq: \(parse \)\?error[^:]*: //' >&2
            echo "see docs/ai/skills/doge-shell-completion-spec/references/schema.md" >&2
            exit 2
        fi
        ;;
esac

exit 0
