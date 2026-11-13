#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# Nightly check for issue #163: anonymous HTTPS `ls-remote` and `clone` of
# public repositories on github.com with rgit alone on PATH.
#
# Usage: scripts/acceptance/github-public.sh [--rgit <path>] <url>...
#
# For each URL, rgit runs under `env -i` with only itself (as `git`) on PATH.
# Afterwards the host's reference git checks the result: rgit's ls-remote
# output equals git's, the clone's HEAD is the advertised HEAD, the work tree
# is clean and `git fsck` passes (not --strict: old history of
# public repositories has known fsck warnings such as bad file modes).
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
client=$here/../../target/release/rgit
if [ "${1:-}" = --rgit ]; then
    client=$2
    shift 2
fi
[ $# -gt 0 ] || { echo "usage: $0 [--rgit <path>] <url>..." >&2; exit 2; }
client=$(cd "$(dirname "$client")" && pwd)/$(basename "$client")
[ -x "$client" ] || { echo "client not found: $client" >&2; exit 2; }
reference=$(command -v git) || { echo "reference git not found" >&2; exit 2; }

work=$(mktemp -d "${TMPDIR:-/tmp}/rgit-public.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/home"
ln -s "$client" "$work/bin/git"

# strict <command...>: run with rgit alone on PATH and an empty home.
strict() {
    env -i PATH="$work/bin" HOME="$work/home" XDG_CONFIG_HOME="$work/home/.config" \
        GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0 LC_ALL=C "$@"
}
# reference_git <args...>: the host's git, isolated from the user's config.
reference_git() {
    env HOME="$work/home" XDG_CONFIG_HOME="$work/home/.config" GIT_CONFIG_NOSYSTEM=1 \
        GIT_TERMINAL_PROMPT=0 "$reference" "$@"
}

failed=""
for url in "$@"; do
    name=$(basename "$url" .git)
    echo "=== $url"
    start=$(date +%s)
    if ! strict git ls-remote "$url" >"$work/$name.rgit-refs"; then
        echo "FAIL: ls-remote $url"
        failed="$failed $url"
        continue
    fi
    reference_git ls-remote "$url" >"$work/$name.git-refs"
    if ! cmp -s "$work/$name.rgit-refs" "$work/$name.git-refs"; then
        echo "FAIL: ls-remote output differs from git's"
        diff "$work/$name.git-refs" "$work/$name.rgit-refs" | head -n 20 || true
        failed="$failed $url"
        continue
    fi
    echo "ls-remote: $(wc -l <"$work/$name.rgit-refs") refs, equal to git's"

    if ! strict git clone -q "$url" "$work/$name"; then
        echo "FAIL: clone $url"
        failed="$failed $url"
        rm -rf "${work:?}/$name"
        continue
    fi
    echo "clone: $(($(date +%s) - start)) s"
    head=$(sed -n 's/\tHEAD$//p' "$work/$name.git-refs")
    if [ "$(reference_git -C "$work/$name" rev-parse HEAD)" != "$head" ]; then
        echo "FAIL: clone HEAD is not the advertised HEAD $head"
        failed="$failed $url"
    elif [ -n "$(reference_git -C "$work/$name" status --porcelain)" ]; then
        echo "FAIL: clone's work tree is not clean"
        failed="$failed $url"
    elif ! reference_git -C "$work/$name" fsck --no-dangling --no-progress; then
        echo "FAIL: git fsck"
        failed="$failed $url"
    else
        echo "PASS"
    fi
    rm -rf "${work:?}/$name"
done

[ -z "$failed" ] || { echo "failed:$failed" >&2; exit 1; }
