// SPDX-License-Identifier: MIT
//! Differential tests of `merge-tree --stdin` and the deprecated
//! trivial-merge mode (#152) against the reference git.

use rgit_difftest::Fixture;

/// `a`, `b` and `c` change `f` on top of `base` (`a`/`b` conflict, `a`/`c`
/// merge cleanly); `t` adds, removes and changes files and turns a file
/// into a directory.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("f", "1\n2\n3\n")
        .write("gone", "gone\n")
        .write("same", "same\n")
        .write("turn", "file\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "base"])
        .git(["tag", "base"])
        .git(["checkout", "-q", "-b", "a"])
        .tick()
        .write("f", "a\n2\n3\n")
        .write("both", "a\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "a"])
        .git(["checkout", "-q", "-b", "b", "base"])
        .tick()
        .write("f", "b\n2\n3\n")
        .write("both", "b\n")
        .write("new", "new\n")
        .git(["rm", "-q", "gone", "turn"])
        .write("turn/x", "x\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "b"])
        .git(["checkout", "-q", "-b", "c", "base"])
        .tick()
        .write("f", "1\n2\nc\n")
        .write("same", "changed\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "c"])
}

#[test]
fn stdin() {
    let mut failed = Vec::new();
    for (input, args) in [
        ("a b\na c\nbase -- a c\n", &["merge-tree", "--stdin"][..]),
        ("a b\n", &["merge-tree", "--stdin", "--name-only"]),
        ("a b\n", &["merge-tree", "--stdin", "--no-messages"]),
        ("a b c\n", &["merge-tree", "--stdin"]),
        ("a\tb\n", &["merge-tree", "--stdin"]),
    ] {
        let mut fx = fixture();
        if fx.is_skipped() {
            return;
        }
        if let Err(m) = fx.compare(args).stdin(input.as_bytes().to_vec()).try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

#[test]
fn trivial_merge() {
    let mut failed = Vec::new();
    for args in [
        &["merge-tree", "base", "a", "c"][..],
        &["merge-tree", "base", "a", "b"],
        &["merge-tree", "base", "b", "a"],
        &["merge-tree", "base", "c", "b"],
        &["merge-tree", "--trivial-merge", "base", "base", "b"],
        &["merge-tree", "base", "a", "a"],
    ] {
        let mut fx = fixture();
        if fx.is_skipped() {
            return;
        }
        if let Err(m) = fx.compare(args).try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}
