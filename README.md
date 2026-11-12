# rgit

rgit is an unofficial, native reimplementation of [git](https://git-scm.com/) in Rust: same
repository format, same wire protocol, same command line. The goal is a drop-in
`git` replacement, client and server side, that interoperates byte-for-byte with
existing repositories and hosts (GitHub, Gitea, Forgejo, GitLab).

Status: **planning / early development.** Nothing is usable yet. The roadmap
lives in the [epics](https://github.com/BaamStudios/rgit/issues?q=label%3Aepic).

rgit is developed as a **clean-room implementation** under the MIT license. It is
written from git's public documentation and from the observable behaviour of the
`git` binary, never from git's GPL source code. See [CLEANROOM.md](CLEANROOM.md).

> [!IMPORTANT]
> **rgit is not git.** rgit is an independent reimplementation of git written
> from scratch in Rust. It is **not** the official git, not a fork of it, and not
> affiliated with, endorsed by or supported by the Git project or Software Freedom
> Conservancy. "Git" is a trademark of Software Freedom Conservancy and is used
> here only to describe compatibility. Report rgit bugs here, never to the git
> mailing list. The official git lives at <https://git-scm.com/>.

## Directory layout

| Path | Contents |
|---|---|
| `crates/rgit-cli/` | The `rgit` binary (command-line front end) |
| `crates/` | Library crates, one per subsystem (objects, packs, refs, index, protocol, ...) |
| `conformance/` | Tests ported from git's `t/` suite (GPL-2.0, separate work, not part of the binary) |
| `docs/` | Design notes and architecture decision records |
| `scripts/fetch-upstream.sh` | Fetches the reference git version into `upstream/git` (not committed) |
| `upstream/git/` | Local copy of upstream git: documentation, reference binary, test suite |
| `CLEANROOM.md` | Rules for what sources may be used |
| `THIRD-PARTY-NOTICES.md` | Copyright notices for adapted code (gitoxide) |

## Building

```sh
cargo build --release
./target/release/rgit --version
```

To get the reference implementation and its documentation:

```sh
scripts/fetch-upstream.sh   # clones git v2.56.1 into upstream/git
```

## Testing strategy

1. Unit tests per crate.
2. Differential tests: run the same operation with `git` and `rgit`, compare
   repository state and output.
3. Conformance: git's own test suite (`t/`) is ported into `conformance/`. Every
   ported test passes against the reference `git` first and is then the
   acceptance check for rgit.

## License

rgit itself: MIT, see [LICENSE](LICENSE).
The ported tests in `conformance/`: GPL-2.0, see [conformance/LICENSE](conformance/LICENSE).
