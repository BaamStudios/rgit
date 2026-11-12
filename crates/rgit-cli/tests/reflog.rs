// SPDX-License-Identifier: MIT
//! `reflog show`, `reflog list` and `reflog exists` against the reference
//! git: output, warnings, errors and exit codes.

use rgit_difftest::{Check, Fixture};

fn compare(fx: &mut Fixture, cases: &[&[&str]]) {
    for args in cases {
        fx.compare(*args)
            .checks([Check::ExitCode, Check::Stdout, Check::Stderr])
            .run();
    }
}

#[test]
fn unborn_repository() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"]);
    if fx.is_skipped() {
        return;
    }
    compare(
        &mut fx,
        &[
            &["reflog"],
            &["reflog", "show", "main"],
            &["reflog", "list"],
            &["reflog", "exists", "HEAD"],
        ],
    );
}

#[test]
fn show_list_exists() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .git(["commit", "-q", "--allow-empty", "-m", "one"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "two"])
        .tick()
        .git(["checkout", "-q", "-b", "topic"])
        .git(["commit", "-q", "--allow-empty", "-m", "three"])
        .tick()
        .git(["checkout", "-q", "main"])
        .git([
            "update-ref",
            "-m",
            "  odd\n message ",
            "refs/heads/main",
            "HEAD~1",
        ])
        .git(["tag", "t"])
        .git([
            "update-ref",
            "--create-reflog",
            "-m",
            "c",
            "refs/tests/t",
            "HEAD",
        ])
        .git(["update-ref", "-m", "f", "refs/tests/t", "HEAD^{tree}"])
        .git(["update-ref", "-m", "r", "refs/tests/t", "HEAD"])
        .git([
            "-c",
            "core.logAllRefUpdates=always",
            "update-ref",
            "BAR_HEAD",
            "HEAD",
        ])
        .write(".git/logs/refs/heads/a.lock", "")
        .write(".git/logs/lowercase", "");
    if fx.is_skipped() {
        return;
    }
    compare(
        &mut fx,
        &[
            &["reflog"],
            &["reflog", "show"],
            &["reflog", "show", "main"],
            &["reflog", "show", "refs/heads/main"],
            &["reflog", "show", "heads/main"],
            &["reflog", "show", "topic"],
            &["reflog", "show", "@"],
            &["reflog", "show", "HEAD"],
            &["reflog", "main"],
            &["reflog", "-1"],
            &["reflog", "-n", "2", "main"],
            &["reflog", "-n2"],
            &["reflog", "--max-count=1", "topic"],
            &["reflog", "--no-abbrev-commit", "-2"],
            &["reflog", "--abbrev=12", "main"],
            &["-c", "core.abbrev=10", "reflog"],
            &["-c", "core.abbrev=no", "reflog"],
            &["reflog", "show", "t"],
            &["reflog", "show", "refs/tests/t"],
            &["reflog", "show", "main@{1}"],
            &["reflog", "show", "main@{9}"],
            &["reflog", "show", "@{1}"],
            &["reflog", "show", "@{-1}"],
            &["reflog", "show", "@{-9}"],
            &["reflog", "show", "main@{-1}"],
            &["reflog", "show", "main@{garbage}"],
            &["reflog", "show", "main@{1700000000}"],
            &["reflog", "show", "main@{2023-11-14 22:15:00 +0000}"],
            &["reflog", "show", "main@{2000-01-01}"],
            &["reflog", "show", "nosuch"],
            &["reflog", "show", "--end-of-options", "main"],
            &["reflog", "show", "main", "--"],
            &["reflog", "show", "-h"],
            &["reflog", "-h"],
            &["reflog", "list"],
            &["reflog", "list", "-h"],
            &["reflog", "list", "bogus"],
            &["reflog", "list", "--bogus"],
            &["reflog", "exists", "refs/heads/main"],
            &["reflog", "exists", "refs/heads/nonexistent"],
            &["reflog", "exists", "--", "refs/heads/main"],
            &["reflog", "exists", "--end-of-options", "refs/heads/main"],
            &["reflog", "exists", "lowercase"],
            &["reflog", "exists", "../config"],
            &["reflog", "exists", "refs/heads/a.lock"],
            &["reflog", "exists"],
            &["reflog", "exists", "-h"],
            &["reflog", "exists", "--unknown-option"],
        ],
    );
}

#[test]
fn gaps_and_missing_objects() {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .git(["commit", "-q", "--allow-empty", "-m", "one"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "two"]);
    if fx.is_skipped() {
        return;
    }
    let id = |rev: &str| {
        String::from_utf8(fx.git_stdout(["rev-parse", rev]))
            .unwrap()
            .trim()
            .to_string()
    };
    let (one, two) = (id("HEAD~1"), id("HEAD"));
    let log = format!(
        "{one} {two} N <e> 1700000100 +0100\tm0\n\
         {two} {one} N <e> 1700000200 +0000\tm1\n\
         0000000000000000000000000000000000000000 1234567890123456789012345678901234567890 N <e> 1700000300 -0700\tbogus\n"
    );
    let mut fx = fx.write(".git/logs/refs/heads/main", log);
    compare(
        &mut fx,
        &[
            &["reflog", "show", "main"],
            &["reflog", "show", "main@{0}"],
            &["reflog", "show", "main@{1}"],
            &["reflog", "show", "main@{3}"],
            &["reflog", "show", "main@{1700000250}"],
            &["reflog", "show", "main@{2023-11-14 22:15:00 +0000}"],
        ],
    );
}

#[test]
fn packed_objects() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .git(["commit", "-q", "--allow-empty", "-m", "one"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "two"])
        .git([
            "update-ref",
            "--create-reflog",
            "-m",
            "c",
            "refs/tests/t",
            "HEAD",
        ])
        .git(["update-ref", "-m", "tree", "refs/tests/t", "HEAD^{tree}"])
        .git(["update-ref", "-m", "back", "refs/tests/t", "HEAD"])
        .git(["repack", "-adq"])
        .git(["prune-packed"]);
    if fx.is_skipped() {
        return;
    }
    compare(
        &mut fx,
        &[
            &["reflog"],
            &["reflog", "main"],
            &["reflog", "--abbrev=4", "main"],
            &["reflog", "refs/tests/t"],
        ],
    );
}
