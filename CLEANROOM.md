# Clean-room policy

rgit is licensed under MIT. Upstream git is licensed under GPL-2.0. To keep rgit
free of GPL-derived code, every contributor (human or AI agent) follows these rules.

## Allowed sources

| Source | Use |
|---|---|
| `upstream/git/Documentation/**` (man pages, `gitformat-*`, `gitprotocol-*`, `technical/*`) | Read freely. These describe formats, protocols and behaviour. |
| The `git` binary | Run it, compare output, use it as the reference implementation in tests. |
| `upstream/git/t/**` (the git test suite) | Port tests into the `rgit-conformance` repository (GPL-2.0, separate work, never linked into rgit). Run upstream scripts as a black box against `rgit`. Read tests to understand **expected behaviour**. |
| Public specs and papers (RFCs, zlib/DEFLATE, SHA-1/SHA-256, diff algorithm papers) | Read freely. |
| Permissively licensed crates (MIT/Apache/BSD/Zlib) | Use as dependencies. |
| **gitoxide** (`gix-*`, MIT OR Apache-2.0, <https://github.com/GitoxideLabs/gitoxide>) | Read, port and adapt freely. Ported code keeps a header naming the gitoxide source file, and gitoxide's copyright notice is listed in `THIRD-PARTY-NOTICES.md`. |

## Forbidden

- Reading or translating git's implementation files: `*.c`, `*.h`, `src/*.rs`,
  `builtin/`, `compat/`, `xdiff/`, `reftable/`, `refs/` etc.
- Copying text from GPL sources into rgit's own code, comments, docs or crate tests.
  This repository contains no GPL-derived material at all; it lives only in the
  separate `rgit-conformance` repository (https://github.com/BaamStudios/rgit-conformance).
- Depending on GPL-licensed crates or linking libgit2 statically in a way that
  changes the license of rgit.

## Two kinds of work

| Work | Reads | Writes to | License |
|---|---|---|---|
| **Implementation** (rgit itself) | Documentation, git binary behaviour, ported conformance tests | `crates/`, `docs/` | MIT |
| **Test porting** | `upstream/git/t/*.sh`, `t/lib-*.sh`, `t/test-lib*.sh`, `t/unit-tests/*` | the `rgit-conformance` repository only | GPL-2.0 |

Implementation agents may run and read the conformance tests (they describe
behaviour), but must never read git's C/Rust implementation.

`t/unit-tests/*.c` are C unit tests. Only **test porting** may read them, to port
their cases into `rgit-conformance/unit/` (Rust, GPL-2.0). Implementation work never
reads them directly, only the ported tests.

## How agents work

- Every issue names the relevant documentation files under "Spec".
- Acceptance criteria are behavioural: "output equals `git ...`" or
  "upstream test `tNNNN-*.sh` passes against rgit".
- If the documentation is insufficient, write a differential test against the
  `git` binary and infer behaviour from it. Never fall back to reading C source.
- Record spec gaps in the issue so humans can decide.

## Decisions on borderline material (2026-11-12)

| Material | Rule |
|---|---|
| User-visible messages (`fatal: ...`, `error: ...`, usage lines, advice) | Reproduce **verbatim** where scripts or tests depend on them. They are short functional strings required for compatibility. Collect them in one place per crate so they are reviewable. |
| Sample hooks and `init` templates | Write our own. Do not copy git's `templates/`. |
| Man pages / `rgit help` | Write our own short help. Full documentation: point to the official git documentation (link), do not bundle git's man pages. |
| Translations (`po/`) | Not used. English only until rgit has its own translations. |
| Built-in funcname regexes, mergetool definitions | Write our own, derived from language grammars / tool docs, not from git's source. |

## Version string

`rgit version` and `rgit --version` print
`git version 2.56.1.rgit.<rgit-version>` so that tools parsing `git version`
(Gitea, scripts) keep working, followed by nothing else. The README and the
second line of `rgit version` (when not called as `git`) state that this is not
the official git.
