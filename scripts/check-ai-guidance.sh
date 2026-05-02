#!/usr/bin/env bash

set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
source_root="$repo_root/docs/ai/skills"
failures=0

fail() {
    echo "error: $*" >&2
    failures=$((failures + 1))
}

if [ ! -d "$source_root" ]; then
    fail "skill source not found: $source_root"
fi

check_skill_frontmatter() {
    skill_dir="$1"
    skill_name=$(basename "$skill_dir")
    skill_file="$skill_dir/SKILL.md"

    if [ ! -f "$skill_file" ]; then
        fail "$skill_name missing SKILL.md"
        return
    fi

    first_line=$(sed -n '1p' "$skill_file")
    if [ "$first_line" != "---" ]; then
        fail "$skill_file missing frontmatter opening"
    fi

    if ! grep -q "^name: $skill_name$" "$skill_file"; then
        fail "$skill_file name must match directory"
    fi

    if ! grep -q "^description: .\\+" "$skill_file"; then
        fail "$skill_file missing description"
    fi
}

check_skill_agent_config() {
    skill_dir="$1"
    skill_name=$(basename "$skill_dir")
    agent_file="$skill_dir/agents/openai.yaml"

    if [ ! -f "$agent_file" ]; then
        fail "$skill_name missing agents/openai.yaml"
        return
    fi

    if ! grep -q "^interface:" "$agent_file"; then
        fail "$agent_file missing interface section"
    fi

    if ! grep -q "^[[:space:]]\\+display_name: .\\+" "$agent_file"; then
        fail "$agent_file missing display_name"
    fi

    if ! grep -q "^[[:space:]]\\+short_description: .\\+" "$agent_file"; then
        fail "$agent_file missing short_description"
    fi

    if ! grep -q "^[[:space:]]\\+default_prompt: .\\+" "$agent_file"; then
        fail "$agent_file missing default_prompt"
    fi
}

check_skill_references() {
    refs=$(grep -Rho '\$[[:alnum:]_-][[:alnum:]_-]*' "$source_root" 2>/dev/null | sed 's/^\$//' | sort -u || true)
    if [ -z "$refs" ]; then
        return
    fi

    while IFS= read -r skill_name; do
        [ -n "$skill_name" ] || continue
        if [ ! -f "$source_root/$skill_name/SKILL.md" ]; then
            fail "unknown skill reference: \$$skill_name"
        fi
    done <<EOF
$refs
EOF
}

check_markdown_links() {
    while IFS= read -r file; do
        links=$(sed -n 's/.*](\([^)]*\.md[^)]*\)).*/\1/p' "$file" || true)
        [ -n "$links" ] || continue

        while IFS= read -r link; do
            [ -n "$link" ] || continue
            case "$link" in
                http://*|https://*|mailto:*)
                    continue
                    ;;
            esac

            target=${link%%#*}
            if [ -z "$target" ]; then
                continue
            fi

            if [ ! -e "$(dirname "$file")/$target" ]; then
                fail "$file references missing markdown target: $link"
            fi
        done <<EOF
$links
EOF
    done < <(find "$repo_root/docs/ai" -name '*.md' -type f | sort)
}

check_bad_guidance() {
    bad_cargo=$(grep -RInE 'cargo test -p dsh([[:space:]`;,.:]|$)' "$repo_root/AGENTS.md" "$repo_root/docs/ai" 2>/dev/null | grep -vE 'Never (use|run)' || true)
    if [ -n "$bad_cargo" ]; then
        echo "$bad_cargo" >&2
        fail "use cargo test -p doge-shell, not cargo test -p dsh"
    fi

    bad_readme=$(grep -RInE '(Start with|start with|最初に).*(README\.md)|README\.md.*( first|から読む|を読む)' "$repo_root/AGENTS.md" "$repo_root/docs/ai" 2>/dev/null | grep -vE 'do not|読まない|only when|only for|Open.*only|読む条件' || true)
    if [ -n "$bad_readme" ]; then
        echo "$bad_readme" >&2
        fail "README.md must not be the first exploration target"
    fi
}

check_readme_skill_names() {
    readme="$repo_root/docs/ai/README.md"

    if [ ! -f "$readme" ]; then
        fail "missing docs/ai/README.md"
        return
    fi

    refs=$(grep -o '`\(doge-shell\|dsh\)-[[:alnum:]_-]*`' "$readme" 2>/dev/null | tr -d '`' | sort -u || true)
    if [ -z "$refs" ]; then
        return 0
    fi

    while IFS= read -r skill_name; do
        [ -n "$skill_name" ] || continue
        if [ ! -f "$source_root/$skill_name/SKILL.md" ]; then
            fail "README references unknown skill: $skill_name"
        fi
    done <<EOF
$refs
EOF
}

check_repo_skill_paths() {
    paths=$(grep -Rho 'docs/ai/skills/[[:alnum:]_-]*/SKILL\.md' "$repo_root/AGENTS.md" "$repo_root/docs/ai" 2>/dev/null | sort -u || true)
    if [ -z "$paths" ]; then
        return 0
    fi

    while IFS= read -r rel_path; do
        [ -n "$rel_path" ] || continue
        if [ ! -f "$repo_root/$rel_path" ]; then
            fail "missing repo-local skill path: $rel_path"
        fi
    done <<EOF
$paths
EOF
}

expect_installer_list() {
    profile="$1"
    expected="$2"
    installer="$repo_root/scripts/install-runtime-skills.sh"
    actual=$(bash "$installer" --list --profile "$profile")

    if [ "$actual" != "$expected" ]; then
        echo "expected profile $profile:" >&2
        echo "$expected" >&2
        echo "actual profile $profile:" >&2
        echo "$actual" >&2
        fail "runtime skill profile mismatch: $profile"
    fi
}

check_installer_profiles() {
    installer="$repo_root/scripts/install-runtime-skills.sh"

    if [ ! -f "$installer" ]; then
        fail "missing runtime skill installer: $installer"
        return
    fi

    expect_installer_list "codex-core" "doge-shell-repo"
    expect_installer_list "codex-common" "doge-shell-repo
doge-shell-validation
doge-shell-investigation
doge-shell-chat-tools"
    expect_installer_list "dsh-common" "doge-shell-repo
doge-shell-validation
doge-shell-investigation
doge-shell-chat-tools"

    for profile in codex-core codex-common dsh-common; do
        if ! grep -q -- "--profile $profile" "$repo_root/docs/ai/README.md"; then
            fail "docs/ai/README.md does not mention installer profile: $profile"
        fi
    done

    if ! bash "$installer" --dry-run --target codex --profile codex-core >/dev/null; then
        fail "runtime skill installer dry run failed"
    fi

    if ! bash "$installer" --status --target codex --profile codex-core >/dev/null; then
        fail "runtime skill installer status check failed"
    fi
}

if [ -d "$source_root" ]; then
    while IFS= read -r skill_dir; do
        check_skill_frontmatter "$skill_dir"
        check_skill_agent_config "$skill_dir"
    done < <(find "$source_root" -mindepth 1 -maxdepth 1 -type d | sort)

    check_skill_references
    check_markdown_links
    check_bad_guidance
    check_readme_skill_names
    check_repo_skill_paths
    check_installer_profiles
fi

if [ "$failures" -gt 0 ]; then
    echo "ai guidance lint failed: $failures issue(s)" >&2
    exit 1
fi

echo "ok ai guidance lint"
