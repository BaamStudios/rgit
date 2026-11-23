// SPDX-License-Identifier: MIT
//! Differential tests of `rgit show` and of `rgit log` with diffs.

use rgit_difftest::{Check, Fixture};

/// Two commits with changes, a side branch merged in, an annotated and a
/// lightweight tag.
fn history() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("f", "a\n")
        .git(["add", "f"])
        .git(["commit", "-q", "-m", "first"])
        .tick()
        .write("f", "a\nb\n")
        .write("dir/g", "x\n")
        .git(["add", "f", "dir/g"])
        .git(["commit", "-q", "-m", "second\n\nbody line"])
        .tick()
        .git(["checkout", "-q", "-b", "side", "HEAD~1"])
        .write("s", "s\n")
        .git(["add", "s"])
        .git(["commit", "-q", "-m", "side"])
        .tick()
        .git(["checkout", "-q", "main"])
        .git(["merge", "-q", "--no-edit", "side"])
        .tick()
        .git(["tag", "-a", "-m", "tag message", "v1", "HEAD~1"])
        .git(["tag", "light", "HEAD"])
}

fn same(fx: &mut Fixture, args: &[&str]) {
    fx.compare(args).checks(Check::OUTPUT).run();
}

#[test]
fn show_objects() {
    let mut fx = history();
    for args in [
        &["show"][..],
        &["show", "HEAD~1"],
        &["show", "v1", "HEAD:f", "HEAD^{tree}", "light"],
        &["show", "HEAD:f", "HEAD~1", "-s"],
        &["show", "HEAD^{tree}", "HEAD:f", "HEAD~1:dir"],
        &["show", "-s", "--format=%s", "HEAD", "HEAD~1", "v1"],
        &["show", "--oneline", "HEAD"],
        &["show", "--oneline", "-s", "v1"],
        &["show", "--pretty=fuller", "-s", "v1"],
        &["show", "--stat", "HEAD~1"],
        &["show", "--name-status", "HEAD~2"],
        &["show", "HEAD~2..HEAD", "--oneline"],
        &["show", "-2", "--format=%s", "HEAD~1"],
        &["show", "--graph", "HEAD"],
        &["show", "no-such-rev"],
    ] {
        same(&mut fx, args);
    }
}

#[test]
fn log_with_diffs() {
    let mut fx = history();
    for args in [
        &["log", "-p"][..],
        &["log", "--stat"],
        &["log", "-p", "--stat"],
        &["log", "--oneline", "-p", "--stat"],
        &["log", "--format=%s", "--raw"],
        &["log", "--name-only"],
        &["log", "--numstat", "--shortstat", "--summary"],
        &["log", "-p", "--first-parent"],
        &["log", "--cc", "--format=%s"],
        &["log", "-p", "--color=always"],
        &["log", "--graph", "-p", "--format=%s"],
        &["log", "--graph", "--stat"],
        &["log", "--diff-merges=bogus"],
    ] {
        same(&mut fx, args);
    }
}

/// A file named `HEAD` does not make the implicit `HEAD` of `show` and
/// `log` ambiguous; an explicit `HEAD` still is (t3404 "rebase with a file
/// named HEAD in worktree").
#[test]
fn file_named_head() {
    let mut fx = history().write("HEAD", "").git(["add", "HEAD"]);
    for args in [
        &["show", "-s", "--pretty=format:%an"][..],
        &["log", "--oneline"],
        &["show", "-s", "HEAD"],
        &["log", "--oneline", "HEAD"],
        &["log", "--oneline", "HEAD", "--"],
    ] {
        same(&mut fx, args);
    }
}

/// Ref-set options name single objects: each ref's object is shown once
/// (annotated tags with their header), without walking (#545); with
/// paths, commits that do not change them are left out.
#[test]
fn show_ref_sets() {
    let mut fx = history()
        .git(["branch", "old", "HEAD~2"])
        .git(["tag", "-a", "-m", "second tag", "v0", "HEAD~2"])
        .git(["update-ref", "refs/remotes/o/main", "HEAD~1"]);
    for args in [
        &["show", "--all"][..],
        &["show", "--all", "--oneline", "--stat"],
        &["show", "--branches", "-s"],
        &["show", "--tags", "-s"],
        &["show", "--remotes", "-s"],
        &["show", "--glob=refs/tags/v*", "-s"],
        &["show", "--exclude=refs/tags/*", "--all", "-s", "--oneline"],
        &["show", "main", "--tags", "-s", "--oneline"],
        &["show", "--all", "--not", "main", "-s", "--oneline"],
        &["show", "--all", "-s", "--oneline", "--", "s"],
        &["show", "side", "main", "-s", "--", "f"],
        &["show", "HEAD^@", "-s", "--oneline"],
    ] {
        same(&mut fx, args);
    }
}

/// Statistics of merges and the formats that may print nothing
/// (`--dirstat`, `--check`) are separated like git does.
#[test]
fn merge_statistics_and_quiet_formats() {
    let mut fx = history();
    for args in [
        &["show", "--oneline", "--stat"][..],
        &["log", "--oneline", "--numstat", "--cc"],
        &["log", "--oneline", "--summary", "-c"],
        &["log", "--format=oneline", "--shortstat", "--cc"],
        &["show", "--dirstat"],
        &["show", "--oneline", "--dirstat"],
        &["show", "--dirstat", "-p", "--oneline"],
        &["log", "--dirstat"],
        &["log", "--check"],
        &["show", "--check", "--oneline"],
    ] {
        same(&mut fx, args);
    }
}
