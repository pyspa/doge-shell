#!/usr/bin/env bash

set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/install-runtime-skills.sh [--target codex|dsh|claude|claude-project|both] [--profile name] [skill-name ...]
       scripts/install-runtime-skills.sh [codex|dsh|claude|claude-project|both]
       scripts/install-runtime-skills.sh --list [--profile name] [skill-name ...]
       scripts/install-runtime-skills.sh --status [--target codex|dsh|claude|claude-project|both] [--profile name]
       scripts/install-runtime-skills.sh --check-installed [--target codex|dsh|claude|claude-project|both] [--profile name]

Installs sample runtime skills from docs/ai/skills/ into:
  codex  -> ~/.codex/skills
  dsh    -> ~/.config/dsh/skills
  claude -> ~/.claude/skills   (Claude Code user-level skills; CLAUDE_CONFIG_DIR overrides ~/.claude)
  claude-project -> <repo>/.claude/skills   (project-level fallback; only needed if the
            .claude/skills -> ../docs/ai/skills symlink is not followed)
  both   -> codex and dsh destinations

Profiles:
  codex-core     doge-shell-repo
  codex-common   doge-shell-repo, doge-shell-validation, doge-shell-investigation, doge-shell-chat-tools
  dsh-common     doge-shell-repo, doge-shell-validation, doge-shell-investigation, doge-shell-chat-tools
  claude-common  doge-shell-repo, doge-shell-validation, doge-shell-investigation, doge-shell-chat-tools

Examples:
  scripts/install-runtime-skills.sh --list
  scripts/install-runtime-skills.sh --list --profile codex-core
  scripts/install-runtime-skills.sh --dry-run --target codex --profile codex-core
  scripts/install-runtime-skills.sh --status --target codex --profile codex-core
  scripts/install-runtime-skills.sh --check-installed --target codex --profile codex-core
  scripts/install-runtime-skills.sh --target claude --profile claude-common
  scripts/install-runtime-skills.sh
  scripts/install-runtime-skills.sh --target codex doge-shell-repo
  scripts/install-runtime-skills.sh dsh
EOF
}

mode="both"
dry_run=0
list_only=0
status_only=0
strict_status=0
profile=""
requested_skills=()

while [ "$#" -gt 0 ]; do
    case "$1" in
        --target)
            if [ "$#" -lt 2 ]; then
                usage >&2
                exit 1
            fi
            mode="$2"
            shift 2
            continue
            ;;
        --dry-run)
            dry_run=1
            ;;
        --list)
            list_only=1
            ;;
        --status)
            status_only=1
            ;;
        --check-installed)
            status_only=1
            strict_status=1
            ;;
        --profile)
            if [ "$#" -lt 2 ]; then
                usage >&2
                exit 1
            fi
            profile="$2"
            shift 2
            continue
            ;;
        codex|dsh|claude|claude-project|both)
            if [ "$mode" = "both" ] && [ "${#requested_skills[@]}" -eq 0 ]; then
                mode="$1"
            else
                requested_skills+=("$1")
            fi
            ;;
        -h|--help|help)
            usage
            exit 0
            ;;
        *)
            requested_skills+=("$1")
            ;;
    esac
    shift
done

if [ -n "$profile" ] && [ "${#requested_skills[@]}" -gt 0 ]; then
    echo "cannot combine --profile with explicit skill names" >&2
    exit 1
fi

case "$mode" in
    codex|dsh|claude|claude-project|both)
        ;;
    *)
        usage >&2
        exit 1
        ;;
esac

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
source_root="$repo_root/docs/ai/skills"

if [ ! -d "$source_root" ]; then
    echo "skill source not found: $source_root" >&2
    exit 1
fi

profile_skills() {
    case "$1" in
        codex-core)
            printf '%s\n' doge-shell-repo
            ;;
        codex-common|dsh-common|claude-common)
            printf '%s\n' \
                doge-shell-repo \
                doge-shell-validation \
                doge-shell-investigation \
                doge-shell-chat-tools
            ;;
        *)
            echo "unknown profile: $1" >&2
            exit 1
            ;;
    esac
}

install_skill_dir() {
    skill_name="$1"
    dest_root="$2"
    src_dir="$source_root/$skill_name"
    dest_dir="$dest_root/$skill_name"

    if [ "$dry_run" -eq 1 ]; then
        echo "would install $skill_name -> $dest_dir"
        return
    fi

    mkdir -p "$dest_root"
    rm -rf "$dest_dir"
    cp -R "$src_dir" "$dest_dir"
    echo "installed $skill_name -> $dest_dir"
}

validate_skill() {
    skill_name="$1"
    if [ ! -d "$source_root/$skill_name" ] || [ ! -f "$source_root/$skill_name/SKILL.md" ]; then
        echo "unknown skill: $skill_name" >&2
        exit 1
    fi
}

skill_list() {
    if [ "${#requested_skills[@]}" -gt 0 ]; then
        for skill_name in "${requested_skills[@]}"; do
            validate_skill "$skill_name"
            printf '%s\n' "$skill_name"
        done
        return
    fi

    if [ -n "$profile" ]; then
        profile_skills "$profile" | while IFS= read -r skill_name; do
            validate_skill "$skill_name"
            printf '%s\n' "$skill_name"
        done
        return
    fi

    for skill_dir in "$source_root"/*; do
        if [ -d "$skill_dir" ] && [ -f "$skill_dir/SKILL.md" ]; then
            basename "$skill_dir"
        fi
    done
}

install_selected() {
    dest_root="$1"
    while IFS= read -r skill_name; do
        install_skill_dir "$skill_name" "$dest_root"
    done
}

status_skill_dir() {
    skill_name="$1"
    dest_root="$2"
    target_label="$3"
    src_dir="$source_root/$skill_name"
    dest_dir="$dest_root/$skill_name"

    validate_skill "$skill_name"

    if [ ! -d "$dest_dir" ]; then
        echo "missing $target_label $skill_name -> $dest_dir"
        return 1
    fi

    if diff -qr "$src_dir" "$dest_dir" >/dev/null 2>&1; then
        echo "ok $target_label $skill_name -> $dest_dir"
    else
        echo "stale $target_label $skill_name -> $dest_dir"
        return 1
    fi
}

status_selected() {
    dest_root="$1"
    target_label="$2"
    status_failures=0
    while IFS= read -r skill_name; do
        if ! status_skill_dir "$skill_name" "$dest_root" "$target_label"; then
            status_failures=1
        fi
    done
    if [ "$strict_status" -eq 1 ]; then
        return "$status_failures"
    fi
    return 0
}

if [ "$list_only" -eq 1 ]; then
    skill_list
    exit 0
fi

if [ "$status_only" -eq 1 ]; then
    status_failures=0
    if [ "$mode" = "codex" ] || [ "$mode" = "both" ]; then
        if ! skill_list | status_selected "${CODEX_HOME:-$HOME/.codex}/skills" codex; then
            status_failures=1
        fi
    fi

    if [ "$mode" = "dsh" ] || [ "$mode" = "both" ]; then
        if ! skill_list | status_selected "${XDG_CONFIG_HOME:-$HOME/.config}/dsh/skills" dsh; then
            status_failures=1
        fi
    fi

    if [ "$mode" = "claude" ]; then
        if ! skill_list | status_selected "${CLAUDE_CONFIG_DIR:-$HOME/.claude}/skills" claude; then
            status_failures=1
        fi
    fi

    if [ "$mode" = "claude-project" ]; then
        if ! skill_list | status_selected "$repo_root/.claude/skills" claude-project; then
            status_failures=1
        fi
    fi

    if [ "$strict_status" -eq 1 ]; then
        exit "$status_failures"
    fi
    exit 0
fi

if [ "$mode" = "codex" ] || [ "$mode" = "both" ]; then
    skill_list | install_selected "${CODEX_HOME:-$HOME/.codex}/skills"
fi

if [ "$mode" = "dsh" ] || [ "$mode" = "both" ]; then
    skill_list | install_selected "${XDG_CONFIG_HOME:-$HOME/.config}/dsh/skills"
fi

if [ "$mode" = "claude" ]; then
    skill_list | install_selected "${CLAUDE_CONFIG_DIR:-$HOME/.claude}/skills"
fi

if [ "$mode" = "claude-project" ]; then
    if [ -L "$repo_root/.claude/skills" ]; then
        echo "note: .claude/skills is a symlink to the canonical source; nothing to copy" >&2
        exit 0
    fi
    skill_list | install_selected "$repo_root/.claude/skills"
fi
