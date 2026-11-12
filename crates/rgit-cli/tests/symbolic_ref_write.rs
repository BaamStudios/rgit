// SPDX-License-Identifier: MIT
//! `symbolic-ref` creating and deleting symbolic refs, against the
//! reference git (output, exit code and the refs afterwards).

use rgit_difftest::{Check, Fixture};

#[test]
fn symbolic_ref_create_and_delete() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .git(["commit", "-q", "--allow-empty", "-m", "c"])
        .write(".git/refs/heads/locked.lock", "");
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["symbolic-ref", "HEAD", "foo"],
        &["symbolic-ref", "HEAD", "refs/heads/a..b"],
        &["symbolic-ref", "refs/heads/s", "refs/heads/a..b"],
        &["symbolic-ref", "refs/heads/s", "foo"],
        &["symbolic-ref", "refs/heads/s", "refs/heads/main"],
        &[
            "symbolic-ref",
            "-m",
            "why",
            "refs/remotes/o/HEAD",
            "refs/remotes/o/main",
        ],
        &["symbolic-ref", "refs/heads/main/x", "refs/heads/main"],
        &["symbolic-ref", "refs/heads/locked", "refs/heads/main"],
        &["symbolic-ref", "HEAD", "refs/heads/other"],
        &["symbolic-ref", "HEAD", "refs/heads/main"],
        &["symbolic-ref", "-d", "refs/heads/main"],
        &["symbolic-ref", "-d", "HEAD"],
        &["symbolic-ref", "-d", "refs/heads/nope"],
        &["symbolic-ref", "-q", "-d", "refs/heads/nope"],
        &["symbolic-ref", "-d", "refs/heads/s"],
        &["symbolic-ref", "--delete", "refs/remotes/o/HEAD"],
        &["symbolic-ref", "-d"],
        &["symbolic-ref", "refs/heads/s", "refs/heads/main"],
        &["symbolic-ref", "refs/heads/s"],
    ];
    for args in cases {
        fx.compare(*args)
            .checks([Check::ExitCode, Check::Stdout, Check::Stderr, Check::Refs])
            .run();
    }
}

#[test]
fn symbolic_ref_short() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["commit", "-q", "--allow-empty", "-m", "c"])
        .git(["update-ref", "refs/heads/x", "HEAD"])
        .git(["update-ref", "refs/remotes/x", "HEAD"])
        .git(["update-ref", "refs/remotes/origin/HEAD", "HEAD"])
        .git(["symbolic-ref", "TEST_A", "refs/heads/x"])
        .git(["symbolic-ref", "TEST_B", "refs/remotes/origin/HEAD"])
        .git(["symbolic-ref", "TEST_C", "refs/tags/t"]);
    if fx.is_skipped() {
        return;
    }
    for name in ["HEAD", "TEST_A", "TEST_B", "TEST_C"] {
        fx.compare(["symbolic-ref", "--short", name])
            .checks([Check::ExitCode, Check::Stdout, Check::Stderr])
            .run();
    }
    // A branch `origin` makes `origin` ambiguous.
    let mut fx = fx.git(["update-ref", "refs/heads/origin", "HEAD"]);
    fx.compare(["symbolic-ref", "--short", "TEST_B"])
        .checks([Check::ExitCode, Check::Stdout, Check::Stderr])
        .run();
}
