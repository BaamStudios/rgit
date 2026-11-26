#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# Platform tests (issue #183): runs `cargo test --all` on a platform other
# than Linux and compares the test targets that fail with the recorded
# baseline of that platform, `ci/platform/<platform>.txt`.
#
#   - a target fails that the baseline does not list  -> error (regression)
#   - a listed target passes                          -> notice (remove the line)
#   - the workspace does not build                    -> error
#
# Baseline format: one target per line, exactly as cargo names it in its
# summary (`-p rgit-cli --test add`, `-p rgit-fs --lib`), or `-p <crate>` for
# every target of a crate; then `#<issue>` and a reason. Every line needs an
# issue reference, so nothing is skipped silently.
#
# Usage: scripts/platform-tests.sh <platform> [cargo test arguments]
set -uo pipefail

platform=${1:?usage: scripts/platform-tests.sh <platform> [cargo test arguments]}
shift
cd "$(dirname "$0")/.."
baseline="ci/platform/$platform.txt"
test -f "$baseline" || { echo "error: no baseline $baseline" >&2; exit 2; }

fail=0
known=$(tr -d '\r' <"$baseline" | sed -e 's/[[:space:]]*#.*$//' -e '/^[[:space:]]*$/d')
while IFS= read -r line; do
    case $line in
    '' | '#'*) ;;
    *'#'[0-9]*) ;;
    *)
        echo "error: $baseline: no issue reference (#<number>) in: $line" >&2
        fail=1
        ;;
    esac
done < <(tr -d '\r' <"$baseline")

log=$(mktemp)
trap 'rm -f "$log"' EXIT
cargo test --all --no-fail-fast "$@" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}

# cargo ends with "error: N targets failed:" and one "`-p <crate> ...`" line each.
failed=$(tr -d '\r' <"$log" | sed -n 's/^[[:space:]]*`\(-p [^`]*\)`[[:space:]]*$/\1/p' | sort -u)

if test "$status" -ne 0 && test -z "$failed"; then
    echo "error: cargo test failed without naming a failed test target (build error?)" >&2
    exit 1
fi

is_known() {
    local target=$1 crate
    crate=$(printf '%s\n' "$target" | cut -d' ' -f1-2)
    printf '%s\n' "$known" | grep -qxF -e "$target" -e "$crate"
}

echo
echo "== $platform: failed test targets compared with $baseline =="
while IFS= read -r target; do
    test -n "$target" || continue
    if is_known "$target"; then
        echo "known failure: $target"
    else
        echo "::error::$platform: test target fails and is not in $baseline: $target"
        fail=1
    fi
done <<<"$failed"

while IFS= read -r entry; do
    test -n "$entry" || continue
    case $entry in
    *' --'*)
        if ! printf '%s\n' "$failed" | grep -qxF -e "$entry"; then
            echo "::notice::$platform: $entry passes now, remove it from $baseline"
        fi
        ;;
    *)
        if ! printf '%s\n' "$failed" | grep -q -e "^$entry\( \|\$\)"; then
            echo "::notice::$platform: every target of $entry passes now, remove it from $baseline"
        fi
        ;;
    esac
done <<<"$known"

if test "$fail" -ne 0; then
    echo "error: new failures on $platform. Fix them, or record them in $baseline with an issue reference." >&2
    exit 1
fi
echo "ok: no failures on $platform beyond the recorded baseline"
