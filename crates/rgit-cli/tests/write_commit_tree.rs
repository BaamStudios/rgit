// SPDX-License-Identifier: MIT
//! Differential tests of `rgit write-tree` and `rgit commit-tree` against
//! the reference git: output, exit code and the object database (fixed
//! `GIT_*_DATE` from the fixture, so commit ids must match byte for byte).

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("a.c", "x\n")
        .write("d/c", "c\n")
        .write("d/e/f", "f\n")
        .write_executable("x", "#!/bin/sh\n")
        .symlink("a", "l")
        .git([
            "update-index",
            "--add",
            "a",
            "a.c",
            "d/c",
            "d/e/f",
            "x",
            "l",
        ])
        .git(["commit", "-q", "-m", "init"])
        .tick()
        .write("msg", "from file\n")
}

const STATE: [Check; 4] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Objects,
];

fn each(cases: &[&[&str]]) {
    for args in cases {
        let mut fx = fixture();
        fx.compare(*args).checks(STATE).run();
    }
}

#[test]
fn write_tree() {
    each(&[
        &["write-tree"],
        &["write-tree", "--prefix=d/"],
        &["write-tree", "--prefix=d/e"],
        &["write-tree", "--prefix=nosuch/"],
        &["write-tree", "--missing-ok"],
    ]);
}

#[test]
fn write_tree_new_entries() {
    let mut fx = fixture()
        .write("n/m", "m\n")
        .git(["update-index", "--add", "n/m"])
        .git([
            "update-index",
            "--add",
            "--cacheinfo",
            "160000,1234567890123456789012345678901234567890,sub",
        ]);
    fx.compare(["write-tree"]).checks(STATE).run();
}

#[test]
fn commit_tree() {
    each(&[
        &["commit-tree", "HEAD^{tree}", "-m", "one"],
        &["commit-tree", "HEAD^{tree}", "-m", "one", "-m", "two"],
        &[
            "commit-tree",
            "-p",
            "HEAD",
            "-p",
            "HEAD",
            "-m",
            "dup",
            "HEAD^{tree}",
        ],
        &[
            "commit-tree",
            "HEAD^{tree}",
            "-p",
            "HEAD",
            "-F",
            "msg",
            "-m",
            "after",
        ],
        &["commit-tree", "nosuch", "-m", "x"],
        &["commit-tree", "HEAD^{tree}", "-p", "nosuch", "-m", "x"],
    ]);
}

#[test]
fn commit_tree_stdin() {
    let mut fx = fixture();
    fx.compare(["commit-tree", "HEAD^{tree}", "-p", "HEAD"])
        .stdin("from stdin\n")
        .checks(STATE)
        .run();
}
