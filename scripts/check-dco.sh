#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# DCO guard (issue #86): every non-merge commit in a range carries a
# `Signed-off-by:` trailer (Developer Certificate of Origin 1.1,
# https://developercertificate.org/) whose e-mail matches the commit author.
# Create such commits with `git commit -s`.
#
# Usage: scripts/check-dco.sh <base>..<head>
#        scripts/check-dco.sh origin/main..HEAD
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <base>..<head>" >&2
    exit 2
fi
range=$1
fail=0
count=0

while IFS= read -r sha; do
    [[ -n "$sha" ]] || continue
    count=$((count + 1))
    author_email=$(git log -1 --format='%ae' "$sha")
    subject=$(git log -1 --format='%s' "$sha")
    signoffs=$(git log -1 --format='%(trailers:key=Signed-off-by,valueonly)' "$sha")
    if [[ -z "${signoffs//[[:space:]]/}" ]]; then
        echo "error: ${sha:0:12} '$subject': missing Signed-off-by" >&2
        fail=1
        continue
    fi
    if ! grep -qiF "<$author_email>" <<<"$signoffs"; then
        echo "error: ${sha:0:12} '$subject': no Signed-off-by for author <$author_email>" >&2
        fail=1
    fi
done < <(git rev-list --no-merges "$range")

if [[ $fail -ne 0 ]]; then
    cat >&2 <<'EOF'

DCO check failed. Sign off your commits (see CONTRIBUTING.md), e.g.:
    git commit --amend -s --no-edit                 # last commit
    git rebase --signoff <base>                     # all commits of the branch
EOF
    exit 1
fi
echo "DCO check passed ($count commit(s))."
