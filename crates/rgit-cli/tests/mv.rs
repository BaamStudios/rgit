// SPDX-License-Identifier: MIT
//! Differential tests of `rgit mv` against the reference git: output,
//! exit code, the resulting index and the work tree.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("b", "b\n")
        .write("d/c", "c\n")
        .write("e/f/x", "x\n")
        .symlink("a", "l")
        .git(["add", "a", "b", "d", "e", "l"])
        .git(["commit", "-q", "-m", "init"])
        .tick()
        .write("untr", "u\n")
        .write("u/1", "1\n")
        .write("m", "m\n")
        .git(["add", "m"])
        .write("m", "m2\n")
        .write("n", "n\n")
        .git(["add", "-N", "n"])
        .write("z/old", "o\n")
        .write("d/untracked", "t\n")
        .symlink("d", "dl")
}

const STATE: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Index,
    Check::Worktree,
];

fn each(dir: &str, cases: &[&[&str]]) {
    for args in cases {
        let mut fx = fixture();
        let mut argv = vec!["mv"];
        argv.extend_from_slice(args);
        fx.compare(&argv).in_dir(dir).checks(STATE).run();
    }
}

#[test]
fn files() {
    each(
        "",
        &[
            &["a", "q"],
            &["-v", "a", "q"],
            &["-n", "a", "q"],
            &["-n", "-v", "a", "q"],
            &["-v", "a", "b", "z"],
            &["a", "z/"],
            &["m", "q"],
            &["n", "q"],
            &["l", "q"],
            &["a", "d/c2"],
            &["./a", "d/../q"],
            &["a/", "q"],
            &["a", "b"],
            &["-f", "a", "b"],
            &["-f", "-v", "a", "b"],
            &["-f", "-n", "-v", "a", "untr"],
            &["-f", "a", "l"],
            &["-f", "l", "a"],
            &["a", "untr"],
            &["a", "a"],
            &["a", "."],
            &["a", ""],
            &["nosuch", "q"],
            &["untr", "q"],
            &["", "q"],
            &["a", "nodir/q"],
            &["a", "nodir/"],
            &["a", "b/q"],
            &["a", "dl/q"],
            &["a", "dl"],
            &["-f", "a", "d"],
            &["a", "b", "nodir"],
            &["a", "b", "b"],
            &["a", "a", "z"],
            &["a", "../q"],
            &["-k", "nosuch", "untr", "a", "b", "z"],
            &["-k", "a", "a", "z"],
            &["-k", "a", "b"],
            &["-n", "-k", "nosuch", "a", "z"],
            &["--sparse", "a", "q"],
            &[],
            &["a"],
        ],
    );
}

#[test]
fn directories() {
    each(
        "",
        &[
            &["d", "q"],
            &["-v", "d", "q"],
            &["-n", "d", "q"],
            &["d/", "q/"],
            &["-v", "d", "e"],
            &["-v", "a", "d", "e/f"],
            &["e/f", "d"],
            &["d", "z"],
            &["d", "d/q"],
            &["d", "nodir/q"],
            &["d", "b"],
            &["-f", "d", "b"],
            &["u", "q"],
            &["-k", "u", "a", "q"],
            &["e/f/x", "e/f", "d"],
            &["e", "e/f", "d"],
            &["e/f", "e/f", "d"],
            &["d", "d", "z"],
            &["-k", "e/f", "e/f", "d"],
            &["-v", "e/f", "e/g"],
            &["e/f/x", "e"],
        ],
    );
}

#[test]
fn subdirectory() {
    each(
        "d",
        &[
            &["-v", "c", "../q"],
            &["c", "../b"],
            &["nosuch", "../q"],
            &["-v", "../a", "."],
            &["../a", "../b", "q"],
            &["../a", "../../q"],
            &["../../q", "../a"],
            &["-v", "../e", "q"],
        ],
    );
}

#[test]
fn deleted_and_locked() {
    let mut fx = fixture().remove("a");
    fx.compare(["mv", "a", "q"]).checks(STATE).run();
    let mut fx = fixture().remove("b");
    fx.compare(["mv", "-v", "a", "b"]).checks(STATE).run();
    let mut fx = fixture().remove("d/c");
    fx.compare(["mv", "d", "q"]).checks(STATE).run();
    let mut fx = fixture().write(".git/index.lock", "");
    fx.compare(["mv", "a", "q"]).checks(STATE).run();
}
