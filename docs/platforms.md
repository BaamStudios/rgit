# Platforms

**Owner decision (2026-11-26, issue #183):** rgit targets **Linux, Windows and
macOS** with git-level compatibility. On each of them rgit behaves like the
reference git built for that platform (Git for Windows on Windows).

Linux has been the only tested platform up to M3. This page records what the
other two need, what is done and how it is checked.

## What CI checks

| Platform | Runner | Checked | Blocking |
|---|---|---|---|
| Linux | `ubuntu-latest` | `cargo fmt`, `cargo clippy -D warnings`, `cargo test --all`, MSRV, conformance suite | everything |
| macOS | `macos-latest` (APFS: case-insensitive, normalisation-insensitive) | `cargo clippy --all-targets -D warnings`, `cargo test --all` | build, clippy, and every test target that is not in the baseline |
| Windows | `windows-latest` (NTFS, MSVC toolchain, Git for Windows as reference) | the same | the same |

The job `platform` in `.github/workflows/ci.yml` runs the complete test suite
on macOS and Windows. `scripts/platform-tests.sh` compares the test targets
that fail with the recorded baseline of the platform:

- `ci/platform/macos.txt`, `ci/platform/windows.txt`: one test target per line
  as cargo names it (`-p rgit-cli --test add`), or `-p <crate>` for a whole
  crate, each with the number of the issue that tracks the failure.
- A target that fails and is not listed fails the job. A listed target that
  passes produces a notice: remove the line.
- Nothing is skipped silently: a target is either green or listed with an
  issue.

Tests that only make sense on a POSIX system (signals, pseudo terminals, Unix
sockets, file modes) are compiled with `#[cfg(unix)]` and do not exist on
Windows.

The conformance suite (git's own test scripts, `rgit-conformance`) runs on
Linux only so far. The file-system scripts (`t0050`, `t6419`, `t7062`) need a
case-insensitive file system; on Linux they can be run on a `casefold` tmpfs
(see below). Running the suite on macOS and Windows runners with a recorded
baseline is tracked in the follow-up issues of #183.

## File-system differences and the settings that describe them

rgit reads the same `core.*` settings as git (git-config(1)). `git init` and
`git clone` probe the file system and record what they find.

| Setting | Meaning | Default / probe | State in rgit |
|---|---|---|---|
| `core.ignoreCase` | names that differ only in case are one file | probed at `init` (`true` on APFS/HFS+, NTFS, FAT) | probe; case-insensitive index lookups in `add`, `status`, `ls-files -o`, `clean`, `mv`, checkout/merge/reset (untracked-file checks), collision warning of `clone` |
| `core.precomposeUnicode` | macOS only: file names read from disk and from the command line are converted from decomposed (NFD) to precomposed (NFC) form | probed at `init` on macOS | see the follow-up issues of #183 |
| `core.protectNTFS` | refuse paths NTFS treats as `.git` (`git~1`, `.git.`, `.git ` , `.git::$INDEX_ALLOCATION`, backslash-separated) | `true` on every platform | checkout paths and tree checks (`fsck`); index writers see the follow-up issues |
| `core.protectHFS` | refuse `.git` with code points HFS+ ignores (zero-width joiners, direction marks, ...) | `true` on macOS | as `core.protectNTFS` |
| `core.symlinks` | `false`: symbolic links are checked out as small text files | probed at `init` (`false` on Windows without the privilege) | probe and checkout |
| `core.fileMode` | `false`: the executable bit of the work tree is not trusted | probed at `init` (`false` on Windows, FAT) | probe and stat comparison |
| `core.autocrlf`, `core.eol` | line ending conversion | unset; Git for Windows sets `core.autocrlf=true` in its *system* configuration, it is not a built-in default | conversion implemented (rgit-convert) |

Only ASCII letters fold for `core.ignoreCase`, as in git.

## Testing a case-insensitive file system on Linux

Linux 6.13 and later can mount a case-insensitive, normalisation-insensitive
tmpfs inside a user namespace, without root:

```sh
unshare -Urm sh -c '
  mount -t tmpfs -o casefold tmpfs /path/to/dir &&
  chattr +F /path/to/dir &&
  TMPDIR=/path/to/dir cargo test -p rgit-cli --test ignorecase'
```

The differential tests then run with the folding file system as their work
directory, and the conformance scripts see the `CASE_INSENSITIVE_FS`
prerequisite when `./run --out /path/to/dir/results` puts the trash
directories there.

## Windows: what is missing

rgit builds for Windows, and clippy is clean. The larger areas that are not
done are tracked in their own issues (see #183 for the list): path handling
(drive letters, UNC paths, backslashes on the command line, `\\?\` prefixes,
reserved device names), process handling (pager, hooks and aliases through a
POSIX shell, signals), the credential cache daemon (Unix sockets), file modes
and symbolic links without the privilege to create them, and time stamp
resolution.
