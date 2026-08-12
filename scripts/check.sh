#!/usr/bin/env bash

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
cd "$repo_root"

cargo fmt --all -- --check
scripts/check-ai-guidance.sh
scripts/check-shell-proxy-capabilities.py
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git diff --check
