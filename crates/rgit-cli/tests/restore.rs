// SPDX-License-Identifier: MIT
//! Differential tests of `restore` and of the path modes of `checkout`
//! (`--no-overlay`, `--ours`/`--theirs`, `--pathspec-from-file`) against
//! the reference git.

use rgit_difftest::Fixture;

/// `v1` has a, b, d/x, d/y; HEAD (`main`) changes a, adds c and d/z and
/// removes d/y. Then a, b and d/x are modified in the work tree, `new`
/// and a change to c are staged; `list` is untracked.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .write("b", "b\n")
        .write("d/x", "x\n")
        .write("d/y", "y\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .git(["tag", "v1"])
        .tick()
        .write("a", "a2\n")
        .write("c", "c\n")
        .write("d/z", "z\n")
        .git(["rm", "-q", "d/y"])
        .git(["add", "."])
        .git(["commit", "-q", "-m", "two"])
        .tick()
        .write("a", "local\n")
        .write("b", "local\n")
        .write("d/x", "local\n")
        .write("new", "n\n")
        .write("c", "staged\n")
        .git(["add", "new", "c"])
        .write("list", "a\n")
}

/// An index with unmerged paths (`read-tree -m` of three trees): f has
/// all three stages, g no "theirs" (deleted there), h no "ours"; k is
/// clean. The work tree holds other content for all of them.
fn conflicted() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("f", "base\n")
        .write("g", "base\n")
        .write("h", "base\n")
        .write("k", "k\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "base"])
        .git(["tag", "base"])
        .tick()
        .git(["switch", "-q", "-c", "theirs"])
        .write("f", "theirs\n")
        .write("h", "theirs\n")
        .git(["rm", "-q", "g"])
        .git(["commit", "-q", "-a", "-m", "theirs"])
        .tick()
        .git(["switch", "-q", "main"])
        .write("f", "ours\n")
        .write("g", "ours\n")
        .git(["rm", "-q", "h"])
        .git(["commit", "-q", "-a", "-m", "ours"])
        .git(["read-tree", "-m", "base", "main", "theirs"])
        .write("f", "junk\n")
        .write("g", "junk\n")
        .write("h", "junk\n")
        .write("k", "junk\n")
}

fn compare(fx: fn() -> Fixture, cases: &[&[&str]]) {
    for args in cases {
        let mut fx = fx();
        if fx.is_skipped() {
            return;
        }
        fx.compare(*args).run();
    }
}

#[test]
fn restore_worktree_and_index() {
    compare(
        fixture,
        &[
            &["restore", "a"],
            &["restore", "--", "a", "b"],
            &["restore", "."],
            &["restore", "nope"],
            &["restore", "nope", "a"],
            &["restore"],
            &["restore", "-S"],
            &["restore", "-s", "v1", "a"],
            &["restore", "--source=v1", "d"],
            &["restore", "--source", "v1", "--overlay", "d"],
            &["restore", "-s", "v1", "-S", "d"],
            &["restore", "-s", "v1", "-SW", "."],
            &[
                "restore",
                "-s",
                "v1",
                "--staged",
                "--worktree",
                "--overlay",
                ".",
            ],
            &["restore", "--staged", "c", "new"],
            &["restore", "-S", "."],
            &["restore", "-SW", "."],
            &["restore", "-W", "-s", "HEAD", "c"],
            &["restore", "-s", "HEAD~", "nomatch", "a"],
            &["restore", "-s", "nonexist", "a"],
            &["restore", "-s", "HEAD:a", "a"],
            &["restore", "-s", "HEAD:d", "x"],
            &["restore", "-s", "v1...main", "a"],
            &["restore", "-q", "a"],
            &["restore", "*.txt"],
        ],
    );
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    fx.compare(["restore", "x"]).in_dir("d").run();
    let mut fx = fixture();
    fx.compare(["restore", "-s", "v1", "."]).in_dir("d").run();
    // An intent-to-add entry comes back from the source.
    let mut fx = fixture()
        .git(["rm", "-q", "--cached", "b"])
        .git(["add", "-N", "b"]);
    fx.compare(["restore", "--staged", "b"]).run();
    let mut fx = fixture().write("d/n", "n\n").git(["add", "-N", "d/n"]);
    fx.compare(["restore", "-s", "HEAD", "d"]).run();
}

#[test]
fn restore_option_errors() {
    compare(
        fixture,
        &[
            &["restore", "--staged", "--ours", "a"],
            &["restore", "--staged", "--theirs", "a"],
            &["restore", "-s", "HEAD", "--ours", "a"],
            &["restore", "--ours", "--merge", "a"],
            &["restore", "--staged", "--merge", "a"],
            &["restore", "--staged", "--conflict=diff3", "a"],
            &["restore", "-s", "HEAD", "--conflict=merge", "a"],
            &["restore", "--conflict=bad", "a"],
            &["restore", "-p", "--overlay", "a"],
            &["restore", "-p", "--pathspec-from-file=list"],
            &["restore", "--pathspec-from-file=list", "a"],
            &["restore", "--pathspec-file-nul", "a"],
            &["restore", "--pathspec-from-file=nofile"],
            &["restore", "-s", "nonexist", "--pathspec-from-file=nofile"],
            &["restore", "-s", "nonexist"],
            &["restore", "-s", "HEAD", "--ours"],
        ],
    );
}

#[test]
fn restore_pathspec_from_file() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    fx.compare(["restore", "--pathspec-from-file=-", "-s", "v1"])
        .stdin("a\nd/x\r\n")
        .run();
    let mut fx = fixture();
    fx.compare(["restore", "--pathspec-from-file=-", "--pathspec-file-nul"])
        .stdin("a\0b\0")
        .run();
    let mut fx = fixture();
    fx.compare(["restore", "--pathspec-from-file=-"])
        .stdin("\"\\141\"\n")
        .run();
    let mut fx = fixture();
    fx.compare(["restore", "--pathspec-from-file=-", "--pathspec-file-nul"])
        .stdin("\"\\141\"\0")
        .run();
    let mut fx = fixture();
    fx.compare(["restore", "--pathspec-from-file=-"])
        .stdin("")
        .run();
    let mut fx = fixture().write("d/list", "x\n");
    fx.compare(["restore", "--pathspec-from-file=list"])
        .in_dir("d")
        .run();
}

#[test]
fn restore_unmerged() {
    compare(
        conflicted,
        &[
            &["restore", "."],
            &["restore", "f", "k"],
            &["restore", "--ignore-unmerged", "."],
            &["restore", "--ignore-unmerged", "-q", "."],
            &["restore", "--ours", "."],
            &["restore", "--theirs", "."],
            &["restore", "--ours", "--theirs", "f"],
            &["restore", "--ours", "--overlay", "."],
            &["restore", "--theirs", "--overlay", "f", "g"],
            &["restore", "-s", "HEAD", "f"],
            &["restore", "-s", "HEAD", "h"],
            &["restore", "-s", "HEAD", "--ignore-unmerged", "."],
            &["restore", "-s", "HEAD", "--overlay", "."],
            &["restore", "-S", "f"],
            &["restore", "-S", "."],
            &["restore", "-SW", "."],
            &["restore", "-S", "--ignore-unmerged", "."],
            &["restore", "-SW", "--ignore-unmerged", "."],
            &["restore", "-SW", "--ignore-unmerged", "--overlay", "."],
        ],
    );
}

#[test]
fn checkout_path_modes() {
    compare(
        fixture,
        &[
            &["checkout", "--no-overlay", "v1", "--", "d"],
            &["checkout", "--no-overlay", "v1", "d"],
            &["checkout", "--no-overlay", "v1", "a", "d"],
            &["checkout", "--no-overlay", "HEAD", "."],
            &["checkout", "--no-overlay", "--", "a"],
            &["checkout", "--overlay", "v1", "--", "d"],
            &["checkout", "--no-overlay", "main"],
            &["checkout", "--overlay"],
            &["checkout", "--no-overlay", "-b", "x"],
            &["checkout", "--ours"],
            &["checkout", "--ours", "--"],
            &["checkout", "--theirs", "HEAD"],
            &["checkout", "--ours", "main"],
            &["checkout", "--ours", "--detach", "main"],
            &["checkout", "--ours", "-b", "x"],
            &["checkout", "--ours", "-b", "x", "HEAD", "a"],
            &["checkout", "--ours", "HEAD", "a"],
            &["checkout", "--ours", "-f", "a"],
            &["checkout", "-f", "--theirs", "HEAD", "a"],
            &["checkout", "-m", "HEAD", "--", "a"],
            &["checkout", "--conflict=bad", "a"],
            &["checkout", "--ours", "a"],
            &["checkout", "-p", "--overlay", "HEAD"],
            &["checkout", "-p", "--pathspec-from-file=list"],
            &["checkout", "--pathspec-file-nul"],
            &["checkout", "--pathspec-from-file=list", "--", "a"],
            &["checkout", "--pathspec-from-file=list", "HEAD", "--", "a"],
            &["checkout", "--pathspec-from-file=list", "main", "HEAD"],
            &["checkout", "--pathspec-from-file=list", "nonexist"],
            &["checkout", "--pathspec-from-file=list", "--detach"],
            &["checkout", "--pathspec-from-file=nofile"],
        ],
    );
}

#[test]
fn checkout_pathspec_from_file() {
    let cases: &[&[&str]] = &[
        &["checkout", "--pathspec-from-file=-"],
        &["checkout", "--pathspec-from-file=-", "--"],
        &["checkout", "--pathspec-from-file=-", "v1"],
        &["checkout", "--pathspec-from-file=-", "v1", "--"],
        &["checkout", "--pathspec-from-file=-", "-b", "x"],
        &["checkout", "--no-overlay", "--pathspec-from-file=-", "v1"],
    ];
    for args in cases {
        let mut fx = fixture();
        if fx.is_skipped() {
            return;
        }
        fx.compare(*args).stdin("a\nd\n").run();
    }
    // An empty list: no paths, a branch switch.
    for args in [
        &["checkout", "--pathspec-from-file=-", "main"][..],
        &["checkout", "--ours", "--pathspec-from-file=-"],
    ] {
        let mut fx = fixture();
        fx.compare(args).stdin("").run();
    }
}

#[test]
fn checkout_unmerged() {
    compare(
        conflicted,
        &[
            &["checkout", "."],
            &["checkout", "-f", "."],
            &["checkout", "-f", "-q", "."],
            &["checkout", "--ours", "."],
            &["checkout", "--theirs", "--", "."],
            &["checkout", "--ours", "--", "f", "g"],
            &["checkout", "--theirs", "f"],
            &["checkout", "-2", "f"],
            &["checkout", "-3", "f"],
            &["checkout", "--ours", "--no-overlay", "."],
            &["checkout", "--theirs", "--no-overlay", "--", "."],
            &["checkout", "HEAD", "f"],
            &["checkout", "HEAD", "--", "h"],
            &["checkout", "--no-overlay", "HEAD", "--", "h"],
            &["checkout", "--no-overlay", "-f", "HEAD", "--", "."],
            &["checkout", "--no-overlay", "base", "."],
        ],
    );
}
