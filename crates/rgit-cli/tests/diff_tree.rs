// SPDX-License-Identifier: MIT
//! Differential tests of `rgit diff-tree <tree-ish> <tree-ish>` against
//! the reference git.

use rgit_difftest::{Check, Fixture};

/// Two commits with modifications, additions, deletions, a mode change,
/// a file/symlink type change, a file that becomes a directory, odd names
/// and a binary file.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a/b/f", "one\ntwo\nthree\n")
        .write("a/g", "y\n")
        .write("c/\u{e4}\tq", "z\n")
        .write("gone", "bye\n")
        .write("mode", "#!/bin/sh\n")
        .write("tl", "target\n")
        .write("x", "file\n")
        .write("bin", "a\0b")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "first"])
        .git(["tag", "v1"])
        .tick()
        .write("a/b/f", "one\n2\nthree\nfour\n")
        .write("a/new file", "n\n")
        .write("c/\u{e4}\tq", "zz\n")
        .write_executable("mode", "#!/bin/sh\n")
        .write("bin", "a\0c")
        .git(["rm", "-q", "gone", "tl", "x"])
        .symlink("a/g", "tl")
        .write("x/inner", "dir now\n")
        .git(["add", "-A"])
        .git(["commit", "-q", "-m", "second"])
}

#[test]
fn diff_tree_matches_git() {
    let mut fx = fixture();
    for opts in [
        &[][..],
        &["-r"],
        &["-t"],
        &["-r", "--name-only"],
        &["-r", "--name-status"],
        &["--name-status"],
        &["-r", "-z"],
        &["-r", "--name-status", "-z"],
        &["-p"],
    ] {
        for (a, b) in [("v1", "HEAD"), ("HEAD", "v1"), ("HEAD^{tree}", "v1:")] {
            let mut argv = vec!["diff-tree"];
            argv.extend_from_slice(opts);
            argv.extend_from_slice(&[a, b]);
            fx.compare(&argv).checks(Check::OUTPUT).run();
        }
    }
}
