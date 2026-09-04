#!/usr/bin/env bash

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
cd "$repo_root"

base_ref=${DSH_CHECK_BASE_REF:-develop}
if ! git rev-parse --verify --quiet "${base_ref}^{commit}" >/dev/null; then
    if [[ -z ${DSH_CHECK_BASE_REF:-} ]] \
        && git rev-parse --verify --quiet "origin/develop^{commit}" >/dev/null; then
        base_ref=origin/develop
    else
        echo "error: base ref '$base_ref' does not resolve to a commit" >&2
        exit 1
    fi
fi
merge_base=$(git merge-base "$base_ref" HEAD)

cargo fmt --all -- --check
scripts/check-ai-guidance.sh
scripts/check-project-consistency.py
scripts/check-shell-proxy-capabilities.py
scripts/check-portability.py
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

git diff --check "$merge_base" HEAD
git diff --cached --check
git diff --check
