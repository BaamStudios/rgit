// SPDX-License-Identifier: MIT
//! Differential tests of `rgit read-tree` against the reference git:
//! output, exit code and the resulting index (including the cache tree).

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("b", "b\n")
        .write("d/c", "c\n")
        .write("d/ee/f", "f\n")
        .write("dd/g", "g\n")
        .symlink("a", "l")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "init"])
        .tick()
        .write("a", "a2\n")
        .write("new", "n\n")
        .git(["add", "."])
}

const STATE: [Check; 4] = [Check::ExitCode, Check::Stdout, Check::Stderr, Check::Index];

#[test]
fn read_tree_cases() {
    for args in [
        &["HEAD"][..],
        &["HEAD^{tree}"],
        &["--empty"],
        &[],
        &["nosuch"],
    ] {
        let mut fx = fixture();
        let mut argv = vec!["read-tree"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(STATE).run();
    }
}

/// `--prefix=<dir>/` (new, existing and nested directories, a file in the
/// way, overlapping entries, `-u`) and several trees without `-m` (an
/// overlay): the index with its cache tree and the work tree.
#[test]
fn prefix_and_overlay() {
    let fixture = || {
        fixture()
            .git(["commit", "-q", "-m", "two"])
            .git(["rm", "-q", "--cached", "b"])
            .write("x/y", "y\n")
            .git(["add", "x/y"])
    };
    let checks = [
        Check::ExitCode,
        Check::Stdout,
        Check::Stderr,
        Check::Index,
        Check::Worktree,
    ];
    for args in [
        &["--prefix=p/", "HEAD~1"][..],
        &["--prefix=p", "HEAD:d"],
        &["--prefix=d/", "HEAD~1:d"],
        &["--prefix=d/ee/g/", "HEAD:dd"],
        &["--prefix=x/y/", "HEAD:dd"],
        &["--prefix=q/r/s/", "HEAD:d"],
        &["--prefix=", "HEAD"],
        &["--prefix=/p/", "HEAD"],
        &["--prefix=d/", "HEAD"],
        &["--prefix=../p/", "HEAD:d"],
        &["-u", "--prefix=p/", "HEAD:d"],
        &["-n", "--prefix=p/", "HEAD:d"],
        &["--prefix=p/", "--index-output=other", "HEAD:d"],
        &["HEAD~1", "HEAD"],
        &["HEAD", "HEAD~1"],
        &["HEAD:d", "HEAD", "HEAD:dd"],
        &["HEAD:d", "HEAD:dd"],
        &["HEAD", "HEAD"],
    ] {
        let mut fx = fixture();
        let mut argv = vec!["read-tree"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(checks).run();
    }
}
