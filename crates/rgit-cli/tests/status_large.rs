// SPDX-License-Identifier: MIT
//! Differential tests of `rgit status` on work trees large enough for the
//! parallel `lstat` and directory reads, and on indexes whose cache tree
//! (`TREE` extension) lets status skip unchanged subtrees of `HEAD`. Output
//! and the index status writes back are compared with the reference git.

use rgit_difftest::{Check, Fixture};

const CHECKS: [Check; 4] = [Check::ExitCode, Check::Stdout, Check::Stderr, Check::Index];

const ARGS: &[&[&str]] = &[
    &[],
    &["--porcelain=v2"],
    &["-s", "-uall"],
    &["--porcelain", "--ignored"],
    &["-s", "--ignored=matching"],
    &["-s", "-uno"],
    &["-s", "d03"],
];

fn check_all(fx: &mut Fixture) {
    for args in ARGS {
        let mut argv = vec!["-c", "status.renames=false", "status"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(CHECKS).run();
    }
}

/// 24 directories with 5 subdirectories of 12 files each: 1440 files in
/// 145 directories.
fn tree(mut fx: Fixture) -> Fixture {
    for d in 0..24 {
        for s in 0..5 {
            for f in 0..12 {
                fx = fx.write(format!("d{d:02}/s{s}/f{f:02}.txt"), format!("{d} {s} {f}\n"));
            }
        }
    }
    fx.write(".gitignore", "*.o\n/top-only\nbuild/\n")
        .write("d05/.gitignore", "!keep.o\nlocal\ns1/*.tmp\n")
}

#[test]
fn large_work_tree() {
    let fx = tree(
        Fixture::new()
            .rgit(env!("CARGO_BIN_EXE_rgit"))
            .git(["init", "-q", "-b", "main"]),
    )
    .git(["add", "."])
    .git(["commit", "-q", "-m", "one"]);
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx
        .tick()
        // Changed content, size and type; deleted files and directories.
        .write("d00/s0/f00.txt", "changed\n")
        .write("d07/s3/f11.txt", "7 3 x\n")
        .write("d23/s4/f05.txt", "longer content\n")
        .remove("d11/s2/f03.txt")
        .remove("d12/s1")
        .remove("d13/s0/f00.txt")
        .symlink("../s1/f00.txt", "d13/s0/f00.txt")
        // A tracked directory replaced by a symbolic link to another one.
        .remove("d14/s4")
        .symlink("../d15/s4", "d14/s4")
        // Untracked and ignored files everywhere.
        .write("d01/s1/new.txt", "n\n")
        .write("d02/new-dir/a/b", "b\n")
        .write("d03/s2/x.o", "o\n")
        .write("d05/keep.o", "k\n")
        .write("d05/local", "l\n")
        .write("d05/s1/a.tmp", "t\n")
        .write("d05/s2/a.tmp", "t\n")
        .write("d09/build/out", "o\n")
        .write("top-only", "t\n")
        .write("d10/top-only", "t\n")
        .write("d22/empty/.keep", "")
        // Staged changes deep inside an otherwise unchanged tree.
        .write("d17/s2/f04.txt", "staged\n")
        .write("d18/s0/added.txt", "a\n")
        .git(["add", "d17/s2/f04.txt", "d18/s0/added.txt"])
        .git(["rm", "-q", "--cached", "d19/s3/f07.txt"]);
    check_all(&mut fx);
}

#[test]
fn cache_tree_differs_from_head() {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a/b/c/one", "1\n")
        .write("a/b/c/two", "2\n")
        .write("a/b/three", "3\n")
        .write("a/four", "4\n")
        .write("e/five", "5\n")
        .write("f", "f\n")
        .write("g/h", "h\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .write("a/b/c/two", "two\n")
        .write("e/new", "n\n")
        .remove("f")
        .write("f/now-a-dir", "d\n")
        .remove("g")
        .write("g", "g is a file\n")
        .git(["add", "-A"])
        .git(["commit", "-q", "-m", "two"]);
    if fx.is_skipped() {
        return;
    }
    // The index (and its cache tree) is the second commit, HEAD the first.
    let mut fx = fx.git(["reset", "-q", "--soft", "HEAD~1"]);
    check_all(&mut fx);
    // A cache tree written by read-tree, valid but for another tree.
    let mut fx = fx
        .git(["reset", "-q", "--hard", "main@{1}"])
        .git(["read-tree", "HEAD~1"]);
    check_all(&mut fx);
    // Mode changes and a gitlink-free intent-to-add entry next to a valid
    // cache tree.
    let mut fx = fx
        .git(["reset", "-q", "--hard"])
        .git(["update-index", "--chmod=+x", "a/b/three"])
        .write("a/b/c/ita", "i\n")
        .git(["add", "-N", "a/b/c/ita"]);
    check_all(&mut fx);
}
