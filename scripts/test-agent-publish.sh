#!/bin/sh
# Exercise the commit hook with real Git and PR creation with an offline gh stub.
set -eu
root=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1

reject() {
    if "$@" >"$tmp/rejected.log" 2>&1; then
        echo "unexpected success: $*" >&2
        exit 1
    fi
}

git init -q
git config core.hooksPath "$root/.githooks"
git config user.name mopemope
git config user.email 22645+mopemope@users.noreply.github.com
git commit -qm valid --allow-empty
head=$(git rev-parse HEAD)
reject git commit --no-verify --allow-empty --author='Other <other@example.org>' -m wrong-author
reject env GIT_COMMITTER_NAME=Other git commit --allow-empty -m wrong-committer
reject git -c user.email=other@example.org commit --allow-empty -m unlinked-email
[ "$(git rev-parse HEAD)" = "$head" ]

cat > gh <<'STUB'
#!/bin/sh
set -eu
case "$1 $2" in
    'auth token') echo fixture-token ;;
    'api --hostname')
        [ "$GH_TOKEN" = fixture-token ] && [ "$3" = github.com ] && [ "$4" = user ] || exit 1
        [ "$FAKE_LOGIN" != offline ] || exit 1
        echo "$FAKE_LOGIN" ;;
    'pr create')
        [ "$GH_TOKEN" = fixture-token ] && [ "$GH_PROMPT_DISABLED" = 1 ] || exit 1
        printf '%s\n' "$@" > pr-args ;;
    *) exit 1 ;;
esac
STUB
chmod +x gh
export PATH="$tmp:$PATH"
pr="$root/scripts/agent-create-pr.sh"
reject env FAKE_LOGIN=Other "$pr" develop topic "test title" body.md
reject env FAKE_LOGIN=offline "$pr" develop topic "test title" body.md
reject env FAKE_LOGIN=mopemope "$pr" develop topic "test title" body.md --web
[ ! -e pr-args ]
FAKE_LOGIN=mopemope "$pr" develop topic "test title" body.md --draft
printf '%s\n' pr create --repo github.com/pyspa/doge-shell --base=develop --head=topic \
    '--title=test title' --body-file=body.md --draft > expected-args
diff -u expected-args pr-args
echo "agent commit and PR identity checks passed"
