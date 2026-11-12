# conformance — tests ported from upstream git

This directory contains tests **ported from git's own test suite** (`upstream/git/t/`)
to verify that bamm behaves exactly like git.

**License: GPL-2.0** (see `conformance/LICENSE`), because the tests are derived
from git's GPL-2.0 test scripts. This directory is a separate work: it is never
compiled into, linked with or shipped as part of the `bamm` binary, which stays MIT.

## Structure

| Path | Contents |
|---|---|
| `harness/` | Test runner: creates temp repos, runs a command with `git` and with `bamm`, compares results |
| `tNNNN-*/` | One directory per ported upstream script, same number as in `upstream/git/t/` |
| `STATUS.md` | Which upstream scripts are ported and how many cases pass against bamm |

## Rules

- Keep the upstream number and name (`t1006-cat-file` → `t1006-cat-file/`).
- Every ported test case must pass against the reference `git` first; only then
  is it a valid check for bamm.
- Keep a header with the upstream file and git version it was ported from.
