#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# License guard (issue #84): every tracked source file states its license.
#
#   - every *.rs file under crates/ starts with `// SPDX-License-Identifier: MIT`
#   - every file under scripts/ carries `# SPDX-License-Identifier: MIT` on its
#     first line, or on its second line after a `#!` shebang
#   - no tracked file declares a GPL SPDX identifier (GPL-derived material lives
#     only in the separate rgit-conformance repository)
#
# Usage: scripts/check-spdx.sh            (from anywhere inside the repository)
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

rs_header='// SPDX-License-Identifier: MIT'
sh_header='# SPDX-License-Identifier: MIT'
fail=0

while IFS= read -r -d '' f; do
    first=$(head -n 1 -- "$f" | tr -d '\r')
    if [[ "$first" != "$rs_header" ]]; then
        echo "error: $f: first line must be '$rs_header'" >&2
        fail=1
    fi
done < <(git ls-files -z -- 'crates/*.rs' 'crates/**/*.rs')

while IFS= read -r -d '' f; do
    first=$(sed -n 1p -- "$f" | tr -d '\r')
    second=$(sed -n 2p -- "$f" | tr -d '\r')
    if [[ "$first" == "$sh_header" ]]; then
        continue
    fi
    if [[ "$first" == '#!'* && "$second" == "$sh_header" ]]; then
        continue
    fi
    echo "error: $f: needs '$sh_header' on line 1 (or line 2 after a shebang)" >&2
    fail=1
done < <(git ls-files -z -- 'scripts/*')

# The pattern is split so that this script does not match itself.
gpl_pattern='SPDX-License-Identifier:[[:space:]]*(L|A)?'"GPL"
if matches=$(git grep -l -I -E "$gpl_pattern" -- . ':!scripts/check-spdx.sh'); then
    while IFS= read -r f; do
        echo "error: $f: GPL SPDX identifier; GPL material belongs in rgit-conformance" >&2
    done <<<"$matches"
    fail=1
fi

if [[ $fail -ne 0 ]]; then
    echo "SPDX check failed. See CONTRIBUTING.md (License headers)." >&2
    exit 1
fi
echo "SPDX check passed."
