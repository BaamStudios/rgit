// SPDX-License-Identifier: MIT
//! `stash list` with diff options, `status --show-stash`
//! (`status.showStash`) and `stash -p <pathspec>` without `push`, against
//! the reference git.

use rgit_difftest::Fixture;

/// Two stash entries; the newest has a staged and an unstaged change.
fn stashed() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("file", "foo\n")
        .write("other", "o\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "init"])
        .tick()
        .write("other", "changed\n")
        .git(["stash", "-q"])
        .tick()
        .write("file", "index\n")
        .git(["add", "file"])
        .write("file", "working\n")
        .git(["stash", "-q"])
        .tick()
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
fn list_with_diffs() {
    cases(
        stashed,
        &[
            &["stash", "list", "--stat"],
            &["stash", "list", "--name-only"],
            &["stash", "list", "--name-status", "-1"],
            &["stash", "list", "--numstat"],
            &["stash", "list", "--format=%gd%n", "--stat"],
            &["stash", "list", "-p"],
        ],
    );
}

#[test]
fn status_show_stash() {
    cases(
        stashed,
        &[
            &["status", "--show-stash"],
            &["-c", "status.showStash=true", "status"],
            &["-c", "status.showStash=true", "status", "--no-show-stash"],
            &["status", "--show-stash", "--short"],
        ],
    );
    cases(
        || stashed().git(["stash", "drop", "-q"]),
        &[&["status", "--show-stash"]],
    );
}

#[test]
fn patch_with_pathspec_without_push() {
    cases(
        || {
            stashed()
                .write("file", "b\nc\n")
                .git(["commit", "-q", "-a", "-m", "lines"])
                .write("file", "a\nb\nc\nd\n")
                .write("other", "changed-other\n")
        },
        &[
            &["stash", "-m", "msg", "--patch", "file"],
            &["stash", "-m", "msg", "file"],
            &["stash", "file", "-p"],
        ],
    );
}
