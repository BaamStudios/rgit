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

## License guard (similarity check)

AI-assisted code can reproduce memorised fragments of git. `crates/rgit-simcheck`
compares rgit's sources mechanically with git's implementation sources and flags
suspicious matches for human review.

**What it compares.** All `crates/**/*.rs` against upstream git v2.56.1
(`*.c`, `*.h`, `*.rs` outside `t/`, fetched by `scripts/fetch-upstream.sh`).
Three independent checks:

| Check | Method | Finding when |
|---|---|---|
| code | Token normalisation (identifiers, types, literals abstracted; comments, whitespace, preprocessor lines, declaration keywords and `::`/`:` dropped; C and Rust spellings unified), then k-gram fingerprints with winnowing (MOSS) and gap-tolerant chaining | ≥ 30 matched tokens against C, ≥ 60 against upstream Rust. Only k-grams found in at most 10 upstream files count, and repeated k-grams count once, so common idioms and tables do not add up |
| comments | Same method on comment words | ≥ 16 matched words |
| strings | Literals compared verbatim after unifying format placeholders (`%s`, `{}`) | literal of ≥ 32 characters that occurs in a git literal (or contains one); hex constants such as object ids are ignored |

**What it prints.** rgit file:lines, upstream file:lines and a score. It never
prints upstream source text, so reading the report does not break the rules above.
Do not open the upstream lines it names.

**Allowed without review.** Files whose header comment names a gitoxide source
(ported, MIT/Apache-2.0); string literals in a `messages.rs` file or below a
`messages/` directory, and literals that start with `fatal: `, `error: `,
`warning: `, `hint: ` or `usage: ` (user-visible messages may be reproduced
verbatim, see "Decisions on borderline material"). Also allowed: string literals
in test code (files below a `tests/` directory and inline `mod tests { ... }`
modules), because tests compare rgit's output with git's observable output and
spell the expected messages verbatim; and literals without whitespace (environment
variables, config keys, ref names, paths), which are interface names.

Messages in implementation code belong in the crate's `messages.rs` (or a
`messages/` module); a verbatim message elsewhere is a finding until it is moved.

**Running it.**

```sh
scripts/fetch-upstream.sh
cargo run --release -p rgit-simcheck                 # scan crates/, exit 1 on findings
cargo run --release -p rgit-simcheck -- --self-test  # plant transliterated upstream fragments
cargo run --release -p rgit-simcheck -- --help       # thresholds and other options
```

The self-test cuts 40-token fragments from random upstream `.c` files, rewrites
them mechanically into Rust syntax in memory (renamed identifiers, `let mut`
declarations, `.` for `->`, no casts or condition parentheses, other literal
values) and checks that the scanner reports them. Fragments that consist only of
idioms found all over git are skipped and counted, because they match independent
code just as well. At the current defaults 95 % or more of the remaining fragments
must be reported (about 97 % are).

The thresholds were calibrated on gitoxide (an independent Rust implementation,
about 2 million normalised tokens): there they yield 4 code, 1 comment and about
20 string findings, which is the expected review load for a code base of that size.

**CI.** The workflow `License guard` runs on every pull request, weekly and on
demand. It is blocking: a finding fails the job, and with the job set as a
required check the PR cannot be merged until the finding is resolved as below.
The report is in the job summary and as artifact `simcheck-report`.

**Procedure for a finding.**

1. The PR author looks at the **rgit** side only and explains in the PR where the
   code came from (written from which documentation, test or behaviour).
2. A maintainer reviews. Who reviews: any maintainer other than the author; for
   code written by an AI agent, a human maintainer.
3. Resolution, one of:
   - **Rewrite** the flagged rgit code from the documentation (a different person
     or agent than the one who wrote it, without looking at the old version);
   - **Move** a user-visible message into the crate's `messages.rs`;
   - **Accept** an unavoidable match (format constants, protocol strings, a short
     generic idiom) by adding an entry with reason, reviewer, date and PR to
     `crates/rgit-simcheck/allowlist.txt`.
4. Nobody resolves a finding by looking at the upstream lines. If a maintainer
   decides the upstream side has to be inspected, someone who does not work on
   rgit's implementation does it and reports only the verdict.

## Version string

`rgit version` and `rgit --version` print
`git version 2.56.1.rgit.<rgit-version>` so that tools parsing `git version`
(Gitea, scripts) keep working, followed by nothing else. The README and the
second line of `rgit version` (when not called as `git`) state that this is not
the official git.
