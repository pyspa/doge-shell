#!/usr/bin/env bash

set -eu

usage() {
    cat <<'EOF'
Usage: scripts/install-runtime-skills.sh [--target codex|dsh|both] [skill-name ...]
       scripts/install-runtime-skills.sh [codex|dsh|both]

Installs sample runtime skills from docs/ai/skills/ into:
  codex -> ~/.codex/skills
  dsh   -> ~/.config/dsh/skills
  both  -> both destinations

Examples:
  scripts/install-runtime-skills.sh
  scripts/install-runtime-skills.sh --target codex doge-shell-repo
  scripts/install-runtime-skills.sh dsh
EOF
}

mode="both"
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
        codex|dsh|both)
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

case "$mode" in
    codex|dsh|both)
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

install_skill_dir() {
    skill_name="$1"
    dest_root="$2"
    src_dir="$source_root/$skill_name"
    dest_dir="$dest_root/$skill_name"

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

if [ "$mode" = "codex" ] || [ "$mode" = "both" ]; then
    skill_list | install_selected "${CODEX_HOME:-$HOME/.codex}/skills"
fi

if [ "$mode" = "dsh" ] || [ "$mode" = "both" ]; then
    skill_list | install_selected "${XDG_CONFIG_HOME:-$HOME/.config}/dsh/skills"
fi
