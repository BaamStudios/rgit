# SPDX-License-Identifier: MIT
#
# Benchmark scenarios (issue #77), sourced by bench/run.sh once per repository.
#
# Each `sc` line is one scenario that runs identically for git and rgit:
#
#   sc NAME [--prep "ACTION ARGS"] [--cleanup "ACTION ARGS"] [--input FILE] -- ARGS...
#
# ARGS are the arguments after the binary. ACTION is one of bench/lib/prep.sh's
# actions; it runs with the reference git before every timed run. Commands use
# `-C DIR`, never the current directory. Available paths:
#
#   $BARE   the bare source repository (--repos DIR/<repo>.git, never modified)
#   $WT     clean worktree clone (read-only scenarios)
#   $DIRTY  worktree clone with DIRTY_FILES modified and DIRTY_FILES untracked files
#   $MUT    worktree clone that scenarios reset before every run
#   $TMP    scratch directory for copies and clones
#   $BASE   the commit $WT, $DIRTY and $MUT start from
#   $PACK   the largest pack file of $BARE
#   $OBJS   `rev-list --objects --all` of $BARE (input for pack-objects)

# Per-repository parameters: a large revision range, a file with long history
# (blame, log -L) and a grep pattern.
case $REPO in
git)
    A=v2.40.0 B=v2.50.0 FILE=Makefile LINES=1,40 PATTERN=strbuf_addf
    ;;
gitea)
    A=v1.20.0 B=v1.24.0 FILE=go.mod LINES=1,20 PATTERN=context.Context
    ;;
linux)
    A=v6.1 B=v6.6 FILE=Makefile LINES=1,40 PATTERN=EXPORT_SYMBOL_GPL
    ;;
*)
    echo "scenarios.sh: no parameters for repository '$REPO'" >&2
    return 1
    ;;
esac

# Read-only, worktree.
sc status-clean -- -C "$WT" status
sc status-dirty -- -C "$DIRTY" status
sc log-oneline -- -C "$WT" log --oneline
sc log-p-200 -- -C "$WT" log -p -n 200
sc log-L -- -C "$WT" log "-L$LINES:$FILE"
sc rev-list-objects -- -C "$WT" rev-list --all --objects
sc cat-file-batch-check -- -C "$WT" cat-file --batch-all-objects --batch-check
sc diff-stat -- -C "$WT" diff --stat "$A" "$B"
sc blame -- -C "$WT" blame -- "$FILE"
sc grep -- -C "$WT" grep -e "$PATTERN"
sc fetch-noop -- -C "$WT" fetch -q origin

# Packs.
sc index-pack --prep "rmrf $TMP/ip.idx $TMP/ip.rev" --cleanup "rmrf $TMP/ip.idx $TMP/ip.rev" \
    -- -C "$WT" index-pack -o "$TMP/ip.idx" "$PACK"
sc pack-objects --input "$OBJS" -- -C "$WT" pack-objects -q --stdout
sc repack-ad --prep "copy $BARE $TMP/repack" --cleanup "rmrf $TMP/repack" \
    -- -C "$TMP/repack" repack -a -d -q

# Clone and checkout.
sc clone-local --prep "rmrf $TMP/clone" --cleanup "rmrf $TMP/clone" \
    -- clone -q "$BARE" "$TMP/clone"
sc checkout-full --prep "noco $BARE $TMP/noco" --cleanup "rmrf $TMP/noco" \
    -- -C "$TMP/noco" checkout -q -f "$B"
sc checkout-switch --prep "reset $MUT $A" -- -C "$MUT" checkout -q "$B"

# Writing.
sc add-A --prep "dirty $MUT $BASE $DIRTY_FILES" -- -C "$MUT" add -A
sc commit --prep "staged $MUT $BASE $DIRTY_FILES" -- -C "$MUT" commit -q -m bench
sc gc --prep "copy $BARE $TMP/gc" --cleanup "rmrf $TMP/gc" -- -C "$TMP/gc" gc -q
