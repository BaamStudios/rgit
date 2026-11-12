# ADR 0001: Conformance strategy — run git's test suite against rgit

Status: accepted (2026-11-12) · Issue #2

Amended 2026-11-12: the ported tests live in the separate repository
[BaamStudios/rgit-conformance](https://github.com/BaamStudios/rgit-conformance)
(GPL-2.0). rgit pins the expected commit in `conformance.lock`; rgit's CI checks
out exactly that commit. Paths below are relative to that repository.

## Decision

git's shell test suite is the acceptance test for rgit.

- Selected scripts from `upstream/git/t/` are copied **unchanged** into
  `rgit-conformance/t/`, together with `test-lib.sh`, `test-lib-functions.sh` and the
  `lib-*.sh` files they need. Changes are made only where unavoidable and are
  marked with a `# rgit:` comment.
- Every script is run twice:
  1. against the reference git (pinned version, currently v2.56.1). A test
     that fails here is excluded, it is not a valid check.
  2. against rgit, installed as `git` in a directory passed via
     `GIT_TEST_INSTALLED`.
- `test-tool` (git's C test helper) comes from the reference build at first.
  Where a script uses `test-tool` to inspect repository internals, rgit must
  produce data the reference tool can read, which is a compatibility check in
  itself. Subcommands that exercise git internals directly are replaced over
  time by small Rust helpers (#6).
- The harness (#5) records pass/fail per test case and generates
  `rgit-conformance/STATUS.md`. CI fails when a previously passing case fails.
- C unit tests in `t/unit-tests/` are ported to Rust under `rgit-conformance/unit/`
  (#8).

## Why

The upstream suite is the most complete description of git's behaviour.
Running it unchanged avoids translation errors and lets us follow new git
versions by copying newer scripts.

## Licensing

The rgit-conformance repository is GPL-2.0 (derived from git). It only runs
the rgit binary and is never linked into rgit. The rgit repository itself stays
100 % MIT. See CLEANROOM.md.
