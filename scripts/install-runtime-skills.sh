#!/usr/bin/env bash

set -eu

usage() {
    cat <<'EOF'
Usage: scripts/install-runtime-skills.sh [codex|dsh|both]

Installs sample runtime skills from docs/ai/skills/ into:
  codex -> ~/.codex/skills
  dsh   -> ~/.config/dsh/skills
  both  -> both destinations
EOF
}

mode="${1:-both}"

case "$mode" in
    codex|dsh|both)
        ;;
    -h|--help|help)
        usage
        exit 0
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

install_all() {
    dest_root="$1"
    for skill_dir in "$source_root"/*; do
        if [ -d "$skill_dir" ]; then
            install_skill_dir "$(basename "$skill_dir")" "$dest_root"
        fi
    done
}

if [ "$mode" = "codex" ] || [ "$mode" = "both" ]; then
    install_all "${CODEX_HOME:-$HOME/.codex}/skills"
fi

if [ "$mode" = "dsh" ] || [ "$mode" = "both" ]; then
    install_all "${XDG_CONFIG_HOME:-$HOME/.config}/dsh/skills"
fi
