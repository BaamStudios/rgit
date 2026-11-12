## What

<!-- What this pull request changes. -->

## How it was tested

<!-- Unit, differential and/or conformance tests; commands you ran. -->

## Open

<!-- Anything left for a follow-up, open questions. Use "Closes #n" only if the issue's "Done when" is fully met, otherwise "Part of #n". -->

## Checklist

- [ ] **Clean room:** I did not read or translate git's implementation sources (`*.c`, `*.h`, `builtin/`, `xdiff/`, `refs/`, `reftable/`, `compat/`, `t/helper/`, `src/*.rs`) and copied no GPL text into this repository ([CLEANROOM.md](https://github.com/BaamStudios/rgit/blob/main/CLEANROOM.md)).
- [ ] Code ported from gitoxide names its source file in a header and is covered by `THIRD-PARTY-NOTICES.md`.
- [ ] New files start with `SPDX-License-Identifier: MIT`; new dependencies are permissively licensed.
- [ ] Every commit is signed off (`git commit -s`, DCO 1.1).
- [ ] `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings` and `cargo test --all` pass.
