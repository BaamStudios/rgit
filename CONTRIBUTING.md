# Contributing to rgit

rgit is an unofficial clean-room reimplementation of git in Rust, licensed under
MIT. These rules apply to every contribution, from humans and from AI agents.
They keep the provenance of the code explicit and the repository free of
GPL-derived material.

## Clean-room rules in short

The full policy is [CLEANROOM.md](CLEANROOM.md). The short version:

- **Allowed:** git's documentation (`upstream/git/Documentation/**`), the
  behaviour of the `git` binary, git's test suite (`upstream/git/t/**`) to learn
  expected behaviour, public specs and papers, permissively licensed crates, and
  gitoxide (MIT OR Apache-2.0) as a reference.
- **Forbidden:** reading or translating git's implementation (`*.c`, `*.h`
  outside `t/`, `src/*.rs`, `builtin/`, `xdiff/`, `refs/`, `reftable/`,
  `compat/`, `t/helper/`), copying GPL text into this repository, and GPL or
  other copyleft dependencies.
- **No libgit2, no gitoxide crates** as dependencies ([ADR 0002](docs/adr/0002-own-implementation.md)).
  Pieces ported from gitoxide keep a header naming the gitoxide source file, and
  gitoxide's notice stays in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
- If the documentation does not answer a question, write a differential test
  against the `git` binary. Never fall back to reading git's source.

### Two kinds of work

| Work | Reads | Writes to | License |
|---|---|---|---|
| **Implementation** (rgit itself) | Documentation, `git` binary behaviour, the ported conformance tests | this repository (`crates/`, `docs/`, `scripts/`) | MIT |
| **Test porting** | `upstream/git/t/*.sh`, `t/lib-*.sh`, `t/test-lib*.sh`, `t/unit-tests/*` | [rgit-conformance](https://github.com/BaamStudios/rgit-conformance) only | GPL-2.0 |

A pull request does one kind of work, never both. Implementation work may run
and read the ported tests, but not git's C sources or C unit tests.

## Developer Certificate of Origin (DCO)

Every commit must be signed off under the
[Developer Certificate of Origin 1.1](https://developercertificate.org/). By
signing off you certify that you wrote the change or otherwise have the right to
submit it under the project's license, and that it follows the clean-room rules.

```sh
git commit -s                       # adds "Signed-off-by: Your Name <you@example.com>"
git commit --amend -s --no-edit     # sign off the last commit afterwards
git rebase --signoff origin/main    # sign off every commit of your branch
```

The `Signed-off-by` e-mail must match the commit author's e-mail. CI checks every
commit of a pull request (`scripts/check-dco.sh origin/main..HEAD` runs the same
check locally).

## License headers

Every source file starts with an SPDX identifier:

- Rust files under `crates/`: first line `// SPDX-License-Identifier: MIT`
- Files under `scripts/`: `# SPDX-License-Identifier: MIT` on line 1, or on
  line 2 after a `#!` shebang

No file in this repository may carry a GPL identifier. `scripts/check-spdx.sh`
checks this locally and in CI.

## Dependencies

New dependencies must be permissively licensed. [`deny.toml`](deny.toml) allows
MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, Zlib, ISC and Unicode-3.0, bans
`git2`, `libgit2-sys`, `gix` and `gix-*`, and accepts crates from crates.io only.
Extending the allowlist needs a reviewed pull request of its own.

```sh
cargo install --locked cargo-deny
cargo deny check licenses bans sources
scripts/check-banned-deps.sh
```

Put dependency versions in your crate's own `Cargo.toml`, not in
`[workspace.dependencies]`.

## Building

```sh
cargo build --release
./target/release/rgit --version
scripts/fetch-upstream.sh          # reference git into upstream/git (not committed)
```

New crates go under `crates/` (picked up by the workspace glob) and inherit
`version`, `edition`, `license`, `repository` and `rust-version` from the
workspace.

## Testing

Before pushing, all of these must pass (CI runs them too):

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all
scripts/check-spdx.sh
```

- **Unit tests** live in each crate and run with `cargo test --all`. Test data
  in this repository must be written by us, never copied from git's tests.
- **Differential tests** run the same operation with `git` and `rgit` and compare
  repository state and output. They belong to the crate (or CLI) they exercise.
- **Conformance tests** are git's own test suite, ported into the separate
  repository [rgit-conformance](https://github.com/BaamStudios/rgit-conformance)
  (GPL-2.0). This repository pins the expected revision in
  [`conformance.lock`](conformance.lock). Each script runs first against the
  reference `git`, then against `rgit` installed as `git`; see
  [ADR 0001](docs/adr/0001-conformance-strategy.md) and the README of
  rgit-conformance for how to run them.

## Spec gaps

When git's documentation does not describe a behaviour you need:

1. Determine the behaviour with a differential test against the reference `git`.
2. Comment on the issue with a short "Spec gap" note: what the documentation
   says (with file path), what is missing, what `git` actually does (command and
   output), and which behaviour you implemented.
3. If it needs a human decision, list it under "Open questions" in the pull
   request and implement the most conservative option.

## Pull requests

- One issue per branch, named `issue-<n>-<short-slug>`.
- The pull request template asks you to confirm the clean-room rules.
- Write `Closes #<n>` only when the issue's "Done when" is fully met, otherwise
  `Part of #<n>`.
