// SPDX-License-Identifier: MIT
//! Differential tests of `rgit am` against the reference git: output,
//! exit code, commits, index and work tree, through failures and
//! `--continue`, `--skip` and `--abort`.

use rgit_difftest::{Check, Fixture};

const STATE: [Check; 6] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Refs,
    Check::Index,
    Check::Worktree,
];

/// Three commits on `f`; `series.mbox` holds the last two as patches, and
/// HEAD is back at the first commit.
fn base() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("f", "a\nb\nc\n")
        .git(["add", "f"])
        .git(["commit", "-q", "-m", "first"])
        .git(["tag", "first"])
        .tick()
        .write("f", "a\nB\nc\n")
        .write("g", "new\n")
        .git(["add", "f", "g"])
        .git(["commit", "-q", "-m", "second\n\nbody of the second commit"])
        .tick()
        .write("f", "a\nB\nC\n")
        .git(["commit", "-q", "-a", "-m", "third"])
        .tick();
    let mbox = fx.git_stdout(["format-patch", "--stdout", "first"]);
    fx.write("series.mbox", mbox)
        .git(["reset", "-q", "--hard", "first"])
}

fn am(fx: &mut Fixture, args: &[&str]) {
    let mut argv = vec!["am"];
    argv.extend_from_slice(args);
    fx.compare(&argv).checks(STATE).run();
}

#[test]
fn applies_a_series() {
    let mut fx = base();
    am(&mut fx, &["series.mbox"]);
}

#[test]
fn options() {
    for opts in [
        &["-s"][..],
        &["-q", "--signoff"],
        &["-k"],
        &["--keep-non-patch"],
        &["--committer-date-is-author-date"],
        &["-3"],
    ] {
        let mut fx = base();
        let mut args = opts.to_vec();
        args.push("series.mbox");
        am(&mut fx, &args);
    }
}

/// HEAD changes the first line of `f`, so the patches no longer apply.
fn conflicting() -> Fixture {
    base()
        .write("f", "x\nb\nc\n")
        .git(["commit", "-q", "-a", "-m", "conflicting"])
        .tick()
}

#[test]
fn failure_skip_and_abort() {
    let mut fx = conflicting();
    am(&mut fx, &["series.mbox"]);
    am(&mut fx, &["--show-current-patch=diff"]);
    am(&mut fx, &["--continue"]);
    am(&mut fx, &["--skip"]);
    am(&mut fx, &["--abort"]);
    am(&mut fx, &["--abort"]);
}

#[test]
fn three_way_conflict_and_continue() {
    let mut fx = conflicting();
    am(&mut fx, &["-3", "series.mbox"]);
    am(&mut fx, &["--continue"]);
    let mut fx = fx.write("f", "resolved\n").git(["add", "f"]);
    am(&mut fx, &["--continue"]);
    am(&mut fx, &["--quit"]);
}

/// HEAD already has the change of the first patch, made in a commit of
/// its own: the three-way merge changes nothing and the patch is skipped.
#[test]
fn three_way_already_applied() {
    let mut fx = base()
        .write("f", "a\nB\nc\n")
        .write("g", "new\n")
        .git(["add", "f", "g"])
        .git(["commit", "-q", "-m", "the same change"])
        .tick();
    am(&mut fx, &["-3", "series.mbox"]);
}

/// The base repository with `empty.mbox`, a mail without a patch.
fn with_empty_mail() -> Fixture {
    let fx = base()
        .git(["commit", "-q", "--allow-empty", "-m", "nothing"])
        .tick();
    let patch = fx.git_stdout(["format-patch", "--always", "--stdout", "-1"]);
    fx.write("empty.mbox", patch)
        .git(["reset", "-q", "--hard", "first"])
}

#[test]
fn empty_patches() {
    for empty in ["--empty=drop", "--empty=keep"] {
        let mut fx = with_empty_mail();
        am(&mut fx, &[empty, "empty.mbox"]);
    }
    let mut fx = with_empty_mail();
    am(&mut fx, &["empty.mbox"]);
    am(&mut fx, &["--allow-empty"]);
}

#[test]
fn errors() {
    let mut fx = base().write("empty", "").write("dirty", "d\n");
    am(&mut fx, &["--continue"]);
    am(&mut fx, &["--abort"]);
    am(&mut fx, &["empty"]);
    am(&mut fx, &["--empty=bogus", "series.mbox"]);
    am(
        &mut fx,
        &["--show-current-patch=raw", "--show-current-patch=diff"],
    );
    let mut fx = fx.git(["add", "dirty"]);
    am(&mut fx, &["series.mbox"]);
    am(&mut fx, &["--abort"]);
}

#[test]
fn bare_mail_and_hooks() {
    let fx = base();
    let mbox = std::fs::read(fx.work_dir().join("series.mbox")).unwrap();
    // The first mail without its "From " line.
    let text = String::from_utf8(mbox).unwrap();
    let first = text.split("\nFrom ").next().unwrap();
    let bare = first.split_once('\n').unwrap().1.to_owned();
    let mut fx = fx.write("bare.eml", bare).write_executable(
        ".git/hooks/applypatch-msg",
        "#!/bin/sh\necho hooked >\"$1\"\n",
    );
    am(&mut fx, &["bare.eml"]);
}
