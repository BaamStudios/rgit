# Clean-room policy

baam is licensed under MIT. Upstream git is licensed under GPL-2.0. To keep baam
free of GPL-derived code, every contributor (human or AI agent) follows these rules.

## Allowed sources

| Source | Use |
|---|---|
| `upstream/git/Documentation/**` (man pages, `gitformat-*`, `gitprotocol-*`, `technical/*`) | Read freely. These describe formats, protocols and behaviour. |
| The `git` binary | Run it, compare output, use it as the reference implementation in tests. |
| `upstream/git/t/**` (the git test suite) | Port tests into `conformance/` (GPL-2.0, separate work, never linked into baam). Run upstream scripts as a black box against `baam`. Read tests to understand **expected behaviour**. |
| Public specs and papers (RFCs, zlib/DEFLATE, SHA-1/SHA-256, diff algorithm papers) | Read freely. |
| Permissively licensed crates (MIT/Apache/BSD/Zlib) | Use as dependencies. |

## Forbidden

- Reading or translating git's implementation files: `*.c`, `*.h`, `src/*.rs`,
  `builtin/`, `compat/`, `xdiff/`, `reftable/`, `refs/`, `t/unit-tests/` etc.
- Copying text from GPL sources into baam's own code, comments, docs or crate tests.
  The only place for GPL-derived material is `conformance/`.
- Depending on GPL-licensed crates or linking libgit2 statically in a way that
  changes the license of baam.

## Two kinds of work

| Work | Reads | Writes to | License |
|---|---|---|---|
| **Implementation** (baam itself) | Documentation, git binary behaviour, ported conformance tests | `crates/`, `docs/` | MIT |
| **Test porting** | `upstream/git/t/*.sh`, `t/lib-*.sh`, `t/test-lib*.sh` | `conformance/` only | GPL-2.0 |

Implementation agents may run and read the conformance tests (they describe
behaviour), but must never read git's C/Rust implementation.

## How agents work

- Every issue names the relevant documentation files under "Spec".
- Acceptance criteria are behavioural: "output equals `git ...`" or
  "upstream test `tNNNN-*.sh` passes against baam".
- If the documentation is insufficient, write a differential test against the
  `git` binary and infer behaviour from it. Never fall back to reading C source.
- Record spec gaps in the issue so humans can decide.
