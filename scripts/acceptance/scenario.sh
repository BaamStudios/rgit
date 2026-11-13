#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# M2 acceptance scenario (issue #163), client side.
#
# Runs under `env -i` with PATH set to the sandbox directory built by
# m2-gitea.sh: `git` there is rgit (or, with --reference-client, the git under
# comparison) and the only other programs are a handful of shell utilities and
# ssh. There is no other git on PATH, so nothing can fall back to it.
#
# Usage: scenario.sh <phase>      phase: clone | fetch
#
# Environment (set by m2-gitea.sh):
#   ACCEPT_URL    remote URL of the scenario's repository
#   ACCEPT_STATE  directory for the work tree and the recorded commit ids
#   GIT_*         identity, dates, TLS roots, askpass or ssh command
set -eu

url=${ACCEPT_URL:?}
state=${ACCEPT_STATE:?}
work=$state/work

step() { printf '\n--- %s\n' "$*"; }
fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

# expect <description> <expected> <actual>
expect() {
    if [ "$2" != "$3" ]; then
        printf 'FAIL: %s\n--- expected\n%s\n--- actual\n%s\n---\n' "$1" "$2" "$3" >&2
        exit 1
    fi
}

# expect_line <description> <file> <fixed line>: the file holds the line.
expect_line() {
    grep -qxF -- "$3" "$2" || {
        printf 'FAIL: %s: missing line\n  %s\n--- %s\n' "$1" "$3" "$2" >&2
        cat "$2" >&2
        exit 1
    }
}

# commit_at <n> <message>: commit with a fixed timestamp, so that the commit
# ids are the same for every client and every transport.
commit_at() {
    GIT_AUTHOR_DATE="170000${1}000 +0000" GIT_COMMITTER_DATE="170000${1}000 +0000" \
        git commit -q -m "$2"
    git rev-parse HEAD >"$state/$3"
}

# server_ref <name>: what the remote advertises for refs/heads/<name>.
server_ref() {
    git ls-remote "$url" "refs/heads/$1" | sed 's/\t.*//'
}

phase_clone() {
    rm -rf "$work"

    step "strict mode: only the client under test is on PATH"
    command -v git >/dev/null || fail "no git on PATH"
    git --version
    if [ "${ACCEPT_REFERENCE:-0}" != 1 ]; then
        git --version | grep -q rgit || fail "git on PATH is not rgit"
    fi

    step clone
    git clone "$url" "$work" 2>"$state/clone.err" || {
        cat "$state/clone.err" >&2
        fail "clone"
    }
    cd "$work"
    expect "files after clone" "README.md
obsolete.txt
src/lib.txt
tool.sh" "$(git ls-files)"
    expect "README after clone" "# accept

first line
second line
third line" "$(cat README.md)"
    [ -x tool.sh ] || fail "tool.sh lost its executable bit"
    expect "current branch" main "$(git branch --show-current)"
    expect "origin/main = HEAD" "$(git rev-parse HEAD)" "$(git rev-parse origin/main)"
    git rev-parse HEAD >"$state/seed"

    step "status (clean)"
    expect "status --porcelain clean" "" "$(git status --porcelain)"
    expect "status clean" "On branch main
Your branch is up to date with 'origin/main'.

nothing to commit, working tree clean" "$(git status)"

    step "edit, status, diff"
    printf 'fourth line\n' >>README.md
    sed 's/^beta$/BETA/' src/lib.txt >src/lib.tmp && mv src/lib.tmp src/lib.txt
    printf 'new file\n' >new.txt
    expect "status --porcelain after edit" " M README.md
 M src/lib.txt
?? new.txt" "$(git status --porcelain)"
    git diff >"$state/diff.out"
    expect "diff" "diff --git a/README.md b/README.md
index bb02387..6bf5c0f 100644
--- a/README.md
+++ b/README.md
@@ -3,3 +3,4 @@
 first line
 second line
 third line
+fourth line
diff --git a/src/lib.txt b/src/lib.txt
index 600d48a..24941eb 100644
--- a/src/lib.txt
+++ b/src/lib.txt
@@ -1,5 +1,5 @@
 alpha
-beta
+BETA
 gamma
 delta
 epsilon" "$(cat "$state/diff.out")"

    step "add, rm, commit, log"
    git add README.md src/lib.txt new.txt
    git rm -q obsolete.txt
    [ ! -e obsolete.txt ] || fail "rm left obsolete.txt in the work tree"
    expect "status --porcelain staged" "M  README.md
A  new.txt
D  obsolete.txt
M  src/lib.txt" "$(git status --porcelain)"
    commit_at 1 "edit files" c1
    # --format placeholders are not part of M2 (#125): default and --oneline only.
    expect "log --oneline" "$(cut -c1-7 "$state/c1") edit files
$(cut -c1-7 "$state/seed") seed" "$(git log --oneline)"
    git log -1 >"$state/log.out"
    expect_line "log header" "$state/log.out" "Author: Acceptance Tester <tester@example.com>"
    expect_line "log message" "$state/log.out" "    edit files"
    expect "status ahead" "On branch main
Your branch is ahead of 'origin/main' by 1 commit.
  (use \"git push\" to publish your local commits)

nothing to commit, working tree clean" "$(git status)"

    step "reset (mixed, hard, to a commit)"
    printf 'scratch\n' >>README.md
    git add README.md
    git reset -q
    expect "status after reset --mixed" " M README.md" "$(git status --porcelain)"
    git reset -q --hard
    expect "status after reset --hard" "" "$(git status --porcelain)"
    printf 'throwaway\n' >throwaway.txt
    git add throwaway.txt
    commit_at 2 "throwaway" throwaway
    git reset -q --hard HEAD~1
    expect "HEAD after reset --hard HEAD~1" "$(cat "$state/c1")" "$(git rev-parse HEAD)"
    [ ! -e throwaway.txt ] || fail "reset --hard left throwaway.txt"

    step "branch, switch"
    git branch feature
    expect "branch list" "  feature
* main" "$(git branch)"
    git switch -q feature
    expect "current branch after switch" feature "$(git branch --show-current)"
    printf 'feature\n' >feature.txt
    printf 'zeta\n' >>src/lib.txt
    git add feature.txt src/lib.txt
    commit_at 3 "feature work" c2
    git switch -q main
    [ ! -e feature.txt ] || fail "switch main left feature.txt"
    expect "src/lib.txt on main" "alpha
BETA
gamma
delta
epsilon" "$(cat src/lib.txt)"

    step "push: fast-forward of main"
    git push origin main 2>"$state/push-main.err" || {
        cat "$state/push-main.err" >&2
        fail "push main"
    }
    expect "server main after push" "$(cat "$state/c1")" "$(server_ref main)"

    step "push: new branch"
    git push -u origin feature 2>"$state/push-new.err" || {
        cat "$state/push-new.err" >&2
        fail "push new branch"
    }
    expect_line "push new branch output" "$state/push-new.err" " * [new branch]      feature -> feature"
    expect "upstream of feature" origin "$(git config branch.feature.remote)"
    expect "server feature after push" "$(cat "$state/c2")" "$(server_ref feature)"
}

phase_fetch() {
    cd "$work"

    step "fetch after a server-side change"
    git fetch origin 2>"$state/fetch.err" || {
        cat "$state/fetch.err" >&2
        fail fetch
    }
    git rev-parse origin/main >"$state/server1"
    expect "origin/main parent" "$(cat "$state/c1")" "$(git rev-parse origin/main~1)"
    expect "log origin/main" "$(cut -c1-7 "$state/server1") server change" "$(git log -1 --oneline origin/main)"
    expect "status behind" "On branch main
Your branch is behind 'origin/main' by 1 commit, and can be fast-forwarded.
  (use \"git pull\" to update your local branch)

nothing to commit, working tree clean" "$(git status)"
    git reset -q --hard origin/main
    expect_line "README after fetch" README.md "server line"

    step "push: fast-forward (upstream from clone)"
    printf 'after fetch\n' >>README.md
    git add README.md
    commit_at 5 "after fetch" c3
    git push 2>"$state/push-ff.err" || {
        cat "$state/push-ff.err" >&2
        fail "push fast-forward"
    }
    expect "server main after fast-forward" "$(cat "$state/c3")" "$(server_ref main)"

    step "push: rejected non-fast-forward"
    git switch -q feature
    git reset -q --hard HEAD~1
    printf 'rewritten\n' >feature.txt
    git add feature.txt
    commit_at 6 "rewrite feature" c4
    if git push origin feature 2>"$state/push-rejected.err"; then
        fail "non-fast-forward push was accepted"
    fi
    expect_line "rejected push output" "$state/push-rejected.err" \
        " ! [rejected]        feature -> feature (non-fast-forward)"
    grep -q "^error: failed to push some refs to " "$state/push-rejected.err" ||
        fail "rejected push: no 'failed to push' error"
    expect "server feature after rejected push" "$(cat "$state/c2")" "$(server_ref feature)"

    step "push: forced update"
    git push --force origin feature 2>"$state/push-force.err" || {
        cat "$state/push-force.err" >&2
        fail "forced push"
    }
    grep -q "^ + [0-9a-f]*\.\.\.[0-9a-f]* feature -> feature (forced update)\$" \
        "$state/push-force.err" || fail "forced push: no forced-update line"
    expect "server feature after forced push" "$(cat "$state/c4")" "$(server_ref feature)"

    step "push: deletion"
    git switch -q main
    git push origin --delete feature 2>"$state/push-delete.err" || {
        cat "$state/push-delete.err" >&2
        fail "push --delete"
    }
    expect_line "delete output" "$state/push-delete.err" " - [deleted]         feature"
    expect "server feature after delete" "" "$(server_ref feature)"
    if git rev-parse -q --verify refs/remotes/origin/feature >/dev/null; then
        fail "origin/feature still exists after the deletion"
    fi
}

case "${1:-}" in
clone) phase_clone ;;
fetch) phase_fetch ;;
*) fail "usage: scenario.sh clone|fetch" ;;
esac
echo "phase $1: ok"
