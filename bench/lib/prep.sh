#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# Untimed preparation steps for the benchmark scenarios (issue #77). They run
# before every timed run with the reference git ($BENCH_GIT), so git and rgit
# always start from the same state.
#
#   prep.sh rmrf PATH...            remove paths
#   prep.sh copy SRC DST            DST becomes a hard-linked copy of the bare repo SRC
#   prep.sh noco SRC DST            DST becomes a clone of SRC without a checkout
#   prep.sh reset DIR REV           detached, clean worktree at REV
#   prep.sh dirty DIR REV N         reset, then N modified tracked + N untracked files
#   prep.sh staged DIR REV N        dirty, then everything staged (input for commit)
set -euo pipefail

git=${BENCH_GIT:?BENCH_GIT must name the reference git}

reset_to() {
    "$git" -C "$1" checkout -q -f --detach "$2"
    "$git" -C "$1" clean -q -f -d -x
}

# N regular files spread evenly over the tree: each gets a line appended and an
# untracked sibling `<file>.bench-new`.
make_dirty() {
    local dir=$1 n=$2 total step
    total=$("$git" -C "$dir" ls-files -s | awk '$1 == "100644"' | wc -l)
    step=$(( total / n ))
    (( step >= 1 )) || step=1
    "$git" -C "$dir" ls-files -s -z | tr '\0' '\n' |
        awk -v s="$step" -v n="$n" '$1 == "100644" { if ((i++ % s) == 0 && k < n) { k++; sub(/^[^\t]*\t/, ""); print } }' |
        while IFS= read -r f; do
            printf 'bench change\n' >>"$dir/$f"
            printf 'bench new file\n' >"$dir/$f.bench-new"
        done
}

action=$1
shift
case $action in
rmrf)
    rm -rf -- "$@"
    ;;
copy)
    rm -rf -- "$2"
    cp -al -- "$1" "$2"
    ;;
noco)
    rm -rf -- "$2"
    "$git" clone -q --no-checkout -- "$1" "$2"
    ;;
reset)
    reset_to "$1" "$2"
    ;;
dirty)
    reset_to "$1" "$2"
    make_dirty "$1" "$3"
    ;;
staged)
    reset_to "$1" "$2"
    make_dirty "$1" "$3"
    "$git" -C "$1" add -A
    ;;
*)
    echo "prep.sh: unknown action '$action'" >&2
    exit 2
    ;;
esac
