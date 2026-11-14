#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# Acceptance check for issue #149: for commit pairs (parent, commit) of a
# repository, `git diff parent commit | rgit apply --index` in a checkout of
# the parent must yield the commit's tree.
#
# Usage: scripts/acceptance/apply-pairs.sh [--rgit <path>] [--count <n>] [<repo>]
#
# <repo> defaults to upstream/git (scripts/fetch-upstream.sh). The repository
# is cloned into a temporary directory; the reference git produces the diffs
# (with its default rename detection) and checks the result. Pairs whose diff
# contains binary changes are skipped: `git diff` without `--binary` cannot
# be applied by git either.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
client=$here/../../target/release/rgit
count=500
while [ $# -gt 0 ]; do
    case "$1" in
    --rgit) client=$2; shift 2 ;;
    --count) count=$2; shift 2 ;;
    *) break ;;
    esac
done
src=${1:-$here/../../upstream/git}
client=$(cd "$(dirname "$client")" && pwd)/$(basename "$client")
[ -x "$client" ] || { echo "client not found: $client" >&2; exit 2; }
reference=$(command -v git) || { echo "reference git not found" >&2; exit 2; }

work=$(mktemp -d "${TMPDIR:-/tmp}/rgit-apply-pairs.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/home"
# ref <args...>: the reference git, isolated from the user's configuration.
ref() {
    env HOME="$work/home" XDG_CONFIG_HOME="$work/home/.config" GIT_CONFIG_NOSYSTEM=1 \
        "$reference" "$@"
}
rgit() {
    env HOME="$work/home" XDG_CONFIG_HOME="$work/home/.config" GIT_CONFIG_NOSYSTEM=1 \
        "$client" "$@"
}

ref clone -q --no-checkout "$src" "$work/repo"
cd "$work/repo"
tried=0 passed=0 skipped=0 failed=0
for c in $(ref rev-list --no-merges -n "$count" HEAD); do
    p=$(ref rev-parse -q --verify "$c^") || continue
    tried=$((tried + 1))
    if ref diff --numstat "$p" "$c" | grep -q '^-	-	'; then
        skipped=$((skipped + 1))
        continue
    fi
    ref diff "$p" "$c" >"$work/patch"
    ref checkout -q -f --detach "$p"
    ref clean -q -fdx
    if rgit apply --index "$work/patch" 2>"$work/err" &&
        [ "$(ref write-tree)" = "$(ref rev-parse "$c^{tree}")" ] &&
        ref diff --quiet; then
        passed=$((passed + 1))
    else
        failed=$((failed + 1))
        echo "FAIL $p..$c" >&2
        head -5 "$work/err" >&2
    fi
done
echo "pairs: $tried, passed: $passed, skipped (binary): $skipped, failed: $failed"
[ "$failed" -eq 0 ]
