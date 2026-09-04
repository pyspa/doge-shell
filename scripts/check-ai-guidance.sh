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

# Tracked AI guidance AND per-tool guidance (.serena memories), so drift cannot
# hide in a tool config that agents load and act on.
#
# CLAUDE.md is included because it is guidance an agent loads and acts on, and
# it tells the reader to run this script after changing it - which it could not
# usefully do while its own contents went unchecked.
guidance_targets="$repo_root/AGENTS.md $repo_root/CLAUDE.md $repo_root/docs/ai"
if [ -d "$repo_root/.serena/memories" ]; then
    guidance_targets="$guidance_targets $repo_root/.serena/memories"
fi

check_bad_guidance() {
    # The dsh/ directory is the Cargo package `doge-shell`; `-p dsh` matches no package.
    bad_cargo=$(grep -RInE 'cargo (test|run|build|check|clippy) -p dsh([[:space:]`;,.:]|$)' $guidance_targets 2>/dev/null | grep -vE 'Never (use|run)' || true)
    if [ -n "$bad_cargo" ]; then
        echo "$bad_cargo" >&2
        fail "use the doge-shell package name (e.g. cargo test -p doge-shell), not -p dsh"
    fi

    bad_readme=$(grep -RInE '(Start with|start with|最初に).*(README\.md)|README\.md.*( first|から読む|を読む)' "$repo_root/AGENTS.md" "$repo_root/CLAUDE.md" "$repo_root/docs/ai" 2>/dev/null | grep -vE 'do not|読まない|only when|only for|Open.*only|読む条件' || true)
    if [ -n "$bad_readme" ]; then
        echo "$bad_readme" >&2
        fail "README.md must not be the first exploration target"
    fi
}

check_platform_guidance() {
    # doge-shell supports Linux and macOS. Guidance that says otherwise sends an
    # agent down a single-platform path, and it already happened once: a tool
    # config carried "OS: Linux 想定" long after the macOS port landed. Both the
    # positive statement and the absence of the stale one are checked, so the
    # rule cannot be quietly deleted either.
    spec="docs/ai/skills/doge-shell-repo/references/platform-support.md"

    if [ ! -f "$repo_root/$spec" ]; then
        fail "missing platform support spec: $spec"
        return
    fi

    for token in Linux macOS; do
        if ! grep -q "$token" "$repo_root/AGENTS.md"; then
            fail "AGENTS.md does not mention the supported platform: $token"
        fi
    done

    if ! grep -q "platform-support.md" "$repo_root/AGENTS.md"; then
        fail "AGENTS.md does not point at $spec"
    fi

    # Deliberately narrow: "Linux 専用" and "mold は Linux のみ" describe a
    # subsystem and stay legal. Only claims about the project's own reach match.
    bad_platform=$(grep -RInE 'Linux 想定|Linux ?前提|Linux[ -]only|Linux のみ対応' \
        $guidance_targets 2>/dev/null || true)
    if [ -n "$bad_platform" ]; then
        echo "$bad_platform" >&2
        fail "guidance claims a single supported OS; doge-shell targets Linux and macOS ($spec)"
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
    expect_installer_list "claude-common" "doge-shell-repo
doge-shell-validation
doge-shell-investigation
doge-shell-chat-tools"

    for profile in codex-core codex-common dsh-common claude-common; do
        if ! grep -q -- "--profile $profile" "$repo_root/docs/ai/README.md"; then
            fail "docs/ai/README.md does not mention installer profile: $profile"
        fi
    done

    if ! bash "$installer" --dry-run --target codex --profile codex-core >/dev/null; then
        fail "runtime skill installer dry run failed"
    fi

    if ! bash "$installer" --dry-run --target claude --profile claude-common >/dev/null; then
        fail "runtime skill installer claude dry run failed"
    fi

    runtime_tmp=$(mktemp -d)
    runtime_home="$runtime_tmp/codex"
    if CODEX_HOME="$runtime_home" bash "$installer" --check-installed --target codex definitely-not-a-skill >/dev/null 2>&1; then
        fail "runtime skill strict check accepted an unknown skill"
    fi
    if CODEX_HOME="$runtime_home" bash "$installer" --check-installed --target codex --profile definitely-not-a-profile >/dev/null 2>&1; then
        fail "runtime skill strict check accepted an unknown profile"
    fi
    if ! CODEX_HOME="$runtime_home" bash "$installer" --target codex --profile codex-core >/dev/null; then
        fail "runtime skill installer isolated install failed"
    fi
    if ! CODEX_HOME="$runtime_home" bash "$installer" --status --target codex --profile codex-core >/dev/null; then
        fail "runtime skill informational status failed for an exact copy"
    fi
    if ! CODEX_HOME="$runtime_home" bash "$installer" --check-installed --target codex --profile codex-core >/dev/null; then
        fail "runtime skill strict check rejected an exact copy"
    fi

    printf '\n# stale fixture\n' >>"$runtime_home/skills/doge-shell-repo/SKILL.md"
    if CODEX_HOME="$runtime_home" bash "$installer" --check-installed --target codex --profile codex-core >/dev/null 2>&1; then
        fail "runtime skill strict check accepted a stale copy"
    fi
    if ! CODEX_HOME="$runtime_home" bash "$installer" --status --target codex --profile codex-core >/dev/null; then
        fail "runtime skill informational status must tolerate a stale copy"
    fi

    rm -rf "$runtime_home/skills/doge-shell-repo"
    if CODEX_HOME="$runtime_home" bash "$installer" --check-installed --target codex --profile codex-core >/dev/null 2>&1; then
        fail "runtime skill strict check accepted a missing copy"
    fi
    if ! CODEX_HOME="$runtime_home" bash "$installer" --status --target codex --profile codex-core >/dev/null; then
        fail "runtime skill informational status must tolerate a missing copy"
    fi
    rm -rf "$runtime_tmp"
}

check_claude_project_skills() {
    claude_skills="$repo_root/.claude/skills"

    if [ ! -e "$claude_skills" ]; then
        fail ".claude/skills is missing; Claude Code sees no project skills"
        return
    fi

    # A symlink keeps docs/ai/skills the single source of truth. A real
    # directory is the supported fallback, but then it must not drift.
    if [ -L "$claude_skills" ]; then
        target=$(readlink "$claude_skills")
        if [ "$target" != "../docs/ai/skills" ]; then
            fail ".claude/skills must point at ../docs/ai/skills (found: $target)"
        fi
        return
    fi

    if ! diff -qr "$source_root" "$claude_skills" >/dev/null 2>&1; then
        fail ".claude/skills has drifted from docs/ai/skills; rerun scripts/install-runtime-skills.sh --target claude-project"
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
    check_platform_guidance
    check_readme_skill_names
    check_repo_skill_paths
    check_installer_profiles
    check_claude_project_skills
fi

if [ "$failures" -gt 0 ]; then
    echo "ai guidance lint failed: $failures issue(s)" >&2
    exit 1
fi

echo "ok ai guidance lint"
