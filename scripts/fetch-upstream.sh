#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Fetch the reference version of upstream git into ./upstream/git (not committed).
# Used for: reading Documentation/, building the reference binary, running t/ as a black box.
set -euo pipefail
GIT_REF="${GIT_REF:-v2.56.1}"
cd "$(dirname "$0")/.."
if [ -d upstream/git/.git ]; then
  echo "upstream/git already present ($(git -C upstream/git describe --tags 2>/dev/null || echo unknown))"
  exit 0
fi
mkdir -p upstream
git clone --depth 1 --branch "$GIT_REF" https://github.com/git/git.git upstream/git
