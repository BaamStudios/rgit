#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# ADR 0002 guard (issue #83): rgit depends neither on libgit2 nor on gitoxide.
# cargo-deny's [bans] list in deny.toml names the crates one by one; this check
# also catches gix-* crates that appear after that list was written.
#
# Usage: scripts/check-banned-deps.sh     (from anywhere inside the repository)
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

if [[ ! -f Cargo.lock ]]; then
    echo "error: Cargo.lock not found" >&2
    exit 1
fi

if banned=$(grep -E '^name = "(gix|gix-[A-Za-z0-9_-]+|git2|libgit2-sys)"$' Cargo.lock); then
    echo "error: banned dependencies in Cargo.lock (ADR 0002, no gitoxide/libgit2):" >&2
    echo "$banned" >&2
    echo "Use 'cargo tree -i <crate>' to see which crate pulls them in." >&2
    exit 1
fi
echo "Banned dependency check passed."
