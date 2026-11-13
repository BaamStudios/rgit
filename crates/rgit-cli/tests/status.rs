// SPDX-License-Identifier: MIT
//! Differential tests of `rgit status --porcelain[=v2]` against the
//! reference git.

use rgit_difftest::{Check, Fixture};

fn check(fx: &mut Fixture, args: &[&str]) {
    let mut argv = vec!["status"];
    argv.extend_from_slice(args);
    fx.compare(&argv).checks(Check::OUTPUT).run();
}

const MODES: &[&[&str]] = &[
    &["--porcelain"],
    &["--porcelain=v2"],
    &["--porcelain", "-b"],
    &["--porcelain=v2", "--branch"],
    &["-z"],
    &["--porcelain=v2", "-z", "-b"],
    &["--porcelain", "-uno"],
    &["--porcelain", "--untracked-files=normal"],
    &["--porcelain", "-uall"],
    &["--porcelain=v2", "-uall", "-z"],
    &["--porcelain", "u"],
];

#[test]
fn unborn_branch() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .write("b", "b\n")
        .git(["add", "a"]);
    if fx.is_skipped() {
        return;
    }
    for args in MODES {
        check(&mut fx, args);
    }
}

#[test]
fn changes_untracked_and_ignored() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .write("b", "b\n")
        .write("d/x", "x\n")
        .write("sp ace", "s\n")
        .write("t", "t\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .write("a", "changed\n")
        .git(["rm", "-q", "--cached", "b"])
        .write("new", "n\n")
        .git(["add", "new"])
        .write("new", "n2\n")
        .remove("d/x")
        .write_executable("sp ace", "s\n")
        .remove("t")
        .symlink("a", "t")
        .write("u/v/f", "1\n")
        .write(".gitignore", "*.log\n")
        .write("z.log", "l\n")
        .write("ig/q.log", "q\n")
        .write("ita", "i\n")
        .git(["add", "-N", "ita"])
        .write("\u{fc}mlaut", "u\n")
        .write("q\"uote", "q\n");
    if fx.is_skipped() {
        return;
    }
    for args in MODES {
        check(&mut fx, args);
    }
}

#[test]
fn conflict_and_detached() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .git(["checkout", "-q", "-b", "side"])
        .write("a", "side\n")
        .git(["commit", "-q", "-a", "-m", "side"])
        .git(["checkout", "-q", "main"])
        .write("a", "main\n")
        .git(["commit", "-q", "-a", "-m", "main"]);
    if fx.is_skipped() {
        return;
    }
    let _ = fx.git_output(["merge", "-q", "side"]);
    for args in MODES {
        check(&mut fx, args);
    }
    let mut fx = fx
        .git(["reset", "-q", "--hard"])
        .git(["checkout", "-q", "--detach"]);
    for args in MODES {
        check(&mut fx, args);
    }
}
