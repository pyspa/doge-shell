#!/bin/sh
# Create the PR with the very same github.com credential checked below.
set -eu

usage() {
    echo "Usage: $0 BASE HEAD TITLE BODY_FILE [--draft]" >&2
    exit 2
}

case $# in
    4) ;;
    5) [ "$5" = "--draft" ] || usage ;;
    *) usage ;;
esac

base=$1 head=$2 title=$3 body_file=$4
shift 4
set +x
GH_TOKEN=$(gh auth token --hostname github.com)
[ -n "$GH_TOKEN" ] || { echo "No active github.com credential." >&2; exit 1; }
export GH_TOKEN GH_PROMPT_DISABLED=1
login=$(gh api --hostname github.com user --jq .login)
if [ "$login" != mopemope ]; then
    echo "PR creation refused: active github.com user must be mopemope." >&2
    exit 1
fi

exec gh pr create --repo github.com/pyspa/doge-shell \
    --base="$base" --head="$head" --title="$title" --body-file="$body_file" "$@"
