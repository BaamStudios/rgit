// SPDX-License-Identifier: MIT
//! `checkout -m <branch>` and `switch -m`: local changes that block the
//! switch are stashed, the branch is checked out and the stash applied
//! (kept on conflicts), against the reference git.

use rgit_difftest::Fixture;

/// `main`: one (1..8), two (a..e); `simple` changes two, `side` adds a
/// line to one.
fn branches() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("one", "1\n2\n3\n4\n5\n6\n7\n8\n")
        .write("two", "a\nb\nc\nd\ne\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "init"])
        .tick()
        .git(["checkout", "-q", "-b", "simple"])
        .write("two", "a\nc\ne\n")
        .git(["commit", "-q", "-a", "-m", "simple"])
        .tick()
        .git(["checkout", "-q", "-b", "side", "main"])
        .write("one", "1\n2\n3\n4\n5\n6\n7\n8\n9\n")
        .git(["commit", "-q", "-a", "-m", "side"])
        .tick()
        .git(["checkout", "-q", "main"])
}

fn cases(prep: impl Fn() -> Fixture, cases: &[&[&str]]) {
    let mut failed = Vec::new();
    for args in cases {
        let mut fx = prep();
        if fx.is_skipped() {
            return;
        }
        if let Err(m) = fx.compare(*args).try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

#[test]
fn conflicting_changes_are_stashed() {
    cases(
        || branches().write("two", "b\nd\n"),
        &[
            &["checkout", "-m", "simple"],
            &["checkout", "--merge", "--conflict=diff3", "simple"],
            &["switch", "-m", "simple"],
            &["checkout", "simple"],
        ],
    );
    cases(
        || branches().write("one", "1\n2\n3\n4\n5\n"),
        &[
            &["checkout", "-m", "side"],
            &["checkout", "-q", "-m", "side"],
        ],
    );
}

#[test]
fn clean_changes_are_carried() {
    cases(
        || branches().write("one", "0\n1\n2\n3\n4\n5\n6\n7\n8\n"),
        &[&["checkout", "-m", "side"], &["switch", "--merge", "side"]],
    );
    cases(
        || {
            branches()
                .write("two", "a\nb\nc\nd\ne\nf\n")
                .git(["add", "two"])
                .write("one", "1\n2\n3\n4\n5\n6\n7\n")
        },
        &[&["checkout", "-m", "side"]],
    );
    // Nothing blocks the switch: no stash at all.
    cases(
        || branches().write("two", "a\nb\nc\nd\ne\nx\n"),
        &[&["checkout", "-m", "side"]],
    );
}

#[test]
fn failed_switch_restores_the_changes() {
    cases(
        || {
            branches()
                .git(["checkout", "-q", "side"])
                .write("another", "x\n")
                .git(["add", "another"])
                .git(["commit", "-q", "-m", "another"])
                .tick()
                .git(["checkout", "-q", "main"])
                .write("another", "")
                .write("one", "1\n2\n3\n4\n5\n")
        },
        &[&["checkout", "-m", "side"]],
    );
}
