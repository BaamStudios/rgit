// SPDX-License-Identifier: MIT
//! Differential tests: run the same command line with the reference `git` and
//! with `rgit`, then compare output and repository state.
//!
//! This crate is a dev-dependency only (`publish = false`). It contains no
//! material from git's sources; git is only *run* as a black box.
//!
//! # How it works
//!
//! A [`Fixture`] is a temporary directory with an isolated `$HOME` and a
//! deterministic environment: everything inherited is cleared except `PATH`,
//! `GIT_CONFIG_NOSYSTEM=1`, fixed author/committer identity and date
//! (advanced with [`Fixture::tick`]), `TZ=UTC`, `LC_ALL=C`, no pager or
//! editor, `init.defaultBranch=main` in `$HOME/.gitconfig`, and
//! `GIT_CEILING_DIRECTORIES` so repository discovery never leaves the fixture.
//!
//! Setup steps ([`Fixture::git`], [`Fixture::write`], ...) run with the
//! reference git. A comparison ([`Fixture::same`], [`Fixture::compare`])
//! copies the current state twice, runs the command once with each binary
//! *at the same absolute path* (so paths in messages match), and compares:
//!
//! | [`Check`] | What |
//! |---|---|
//! | `ExitCode`, `Stdout`, `Stderr` | byte for byte, after normalisers |
//! | `Refs` | `HEAD` and `for-each-ref` |
//! | `Reflogs` | raw files under `logs/` |
//! | `Objects` | `cat-file --batch-all-objects --batch-check` |
//! | `Fsck` | `fsck --full --strict` result |
//! | `Index` | index version, entry count, `ls-files --stage --debug` (times, device, inode masked) |
//! | `Worktree` | every file below the work directory outside `.git`, with mode and content |
//! | `GitFiles` | `HEAD`, `config`, `FETCH_HEAD`, `ORIG_HEAD`, `MERGE_HEAD`, ... |
//!
//! State is always inspected with the reference git. On a mismatch the test
//! panics with one unified diff per differing aspect. Afterwards the fixture
//! continues from the reference git's result.
//!
//! Differences that are accepted must be normalised explicitly per test with
//! a [`Normalizer`] (for example [`Normalizer::version_line`]).
//!
//! # Binaries
//!
//! - Reference git: `$RGIT_REFERENCE_GIT`, else `upstream/git/git` in the
//!   workspace, else `git` from `PATH`. If none is found the fixture is
//!   *skipped* (all steps are no-ops), unless `RGIT_REQUIRE_REFERENCE=1`.
//! - rgit: [`Fixture::rgit`] (in `rgit-cli` tests: `env!("CARGO_BIN_EXE_rgit")`),
//!   else `$RGIT_BIN`.
//! - Each side finds itself as `git` first in `PATH`, for commands that run `git` again.
//!
//! Set `RGIT_DIFFTEST_KEEP=1` to keep the temporary directory of a failing comparison.
//!
//! # Example
//!
//! ```no_run
//! use rgit_difftest::{Check, Fixture, Normalizer};
//!
//! let mut fx = Fixture::new()
//!     // in an rgit-cli integration test: .rgit(env!("CARGO_BIN_EXE_rgit"))
//!     .rgit("target/debug/rgit")
//!     .git(["init", "-q"])
//!     .write("hello.txt", "hello\n")
//!     .git(["add", "hello.txt"])
//!     .git(["commit", "-q", "-m", "first"])
//!     .tick()
//!     // all checks, byte-identical output
//!     .same(["status", "--porcelain=v2"]);
//!
//! // finer control
//! fx.compare(["--version"])
//!     .output_only()
//!     .normalize_stdout(Normalizer::version_line())
//!     .run();
//! fx.compare(["commit", "-m", "x"]).skip(Check::Reflogs).run();
//! ```
//!
//! Library crates without a binary use the fixture to set up repositories and
//! compare their own results with the reference git's output:
//!
//! ```no_run
//! use rgit_difftest::{Fixture, assert_same_text};
//!
//! let fx = Fixture::new().git(["init", "-q"]).write("a", "x\n").git(["add", "a"]);
//! if !fx.is_skipped() {
//!     let expected = fx.git_stdout(["ls-files", "--stage"]);
//!     let ours = "100644 587be6b4c3f93f93c489c0111bba5596147a26cb 0\ta\n"; // computed by the crate under test
//!     assert_same_text("ls-files", &String::from_utf8_lossy(&expected), ours);
//! }
//! ```

mod diff;
pub mod env;
mod exec;
mod fixture;
mod messages;
mod normalize;
mod state;

pub use diff::unified_diff;
pub use exec::write_executable;
pub use fixture::{Comparison, Fixture, Mismatch, Outcome, RunResult};
pub use normalize::Normalizer;
pub use state::{Check, DEFAULT_GIT_FILES};

/// Asserts that `rgit` equals `git`, panicking with a unified diff otherwise.
///
/// # Panics
/// If the texts differ.
#[track_caller]
pub fn assert_same_text(label: &str, git: &str, rgit: &str) {
    let d = unified_diff(
        git,
        rgit,
        &format!("git ({label})"),
        &format!("rgit ({label})"),
    );
    assert!(d.is_empty(), "{label} differs between git and rgit\n{d}");
}
