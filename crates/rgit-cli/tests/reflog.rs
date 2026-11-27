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

/// `reflog --all` and `log -g` with ref sets walk the reflog of every
/// reference of the set (under its full name, `HEAD` last for `--all`).
#[test]
fn all_reflogs() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["commit", "-q", "--allow-empty", "-m", "one"])
        .tick()
        .git(["branch", "b"])
        .git(["commit", "-q", "--allow-empty", "-m", "two"])
        .tick()
        .git(["tag", "t"])
        .git(["update-ref", "refs/remotes/o/x", "HEAD"])
        .git([
            "update-ref",
            "--create-reflog",
            "-m",
            "tg",
            "refs/tags/tt",
            "HEAD",
        ])
        .git(["commit", "-q", "--allow-empty", "-m", "three"]);
    if fx.is_skipped() {
        return;
    }
    compare(
        &mut fx,
        &[
            &["reflog", "--all"],
            &["reflog", "show", "--all", "main"],
            &["log", "-g", "--all", "--format=%gd %gD %gs"],
            &["log", "-g", "--branches", "--oneline"],
            &["log", "-g", "--remotes", "--oneline"],
            &["log", "-g", "--tags", "--oneline"],
            &["log", "-g", "--glob=refs/heads/m*", "--oneline"],
            &["log", "-g", "--branches=m*", "--oneline"],
            &["log", "-g", "--branches=b", "--oneline"],
            &["log", "-g", "--all", "--exclude=refs/heads/*", "--oneline"],
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

/// `reflog write`, `delete` and `drop`, and `expire --updateref`: output,
/// reflog files and references.
#[test]
fn write_delete_drop() {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .git(["commit", "-q", "--allow-empty", "-m", "c1"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "c2"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "c3"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "c4"])
        .git(["branch", "side"])
        .git(["checkout", "-q", "side"])
        .git(["checkout", "-q", "main"]);
    if fx.is_skipped() {
        return;
    }
    let head = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .to_string();
    let zero = "0".repeat(head.len());
    let short = &head[..8];
    let bogus = "deadbeef".repeat(5);
    let mut fx = fx;
    let cases: Vec<Vec<&str>> = vec![
        vec!["reflog", "write"],
        vec!["reflog", "write", "1", "2", "3", "4", "5"],
        vec!["reflog", "write", "refs/heads/ bad", &zero, &zero, "m"],
        vec!["reflog", "write", "unqualified", &zero, &zero, "m"],
        vec!["reflog", "write", "ROOT_REF", &zero, &head, "m"],
        vec!["reflog", "write", "ROOT_REF_HEAD", &zero, &head, "m"],
        vec!["reflog", "write", "refs/heads/x", short, &zero, "m"],
        vec!["reflog", "write", "refs/heads/x", &zero, "HEAD", "m"],
        vec!["reflog", "write", "refs/heads/x", &zero, &bogus, "m"],
        vec!["reflog", "write", "refs/heads/x", &zero, &head, "a\nb  c\n"],
        vec!["reflog", "write", "HEAD", &head, &head, "again"],
        vec!["reflog", "delete"],
        vec!["reflog", "delete", "-h"],
        vec!["reflog", "delete", "main"],
        vec!["reflog", "delete", "main@{9}"],
        vec!["reflog", "delete", "nope@{1}"],
        vec!["reflog", "delete", "@{1}"],
        vec!["reflog", "delete", "--verbose", "-n", "main@{1}"],
        vec!["reflog", "delete", "--verbose", "main@{1}"],
        vec!["reflog", "delete", "--rewrite", "main@{1}", "main@{2}"],
        vec!["reflog", "delete", "main@{1}", "main@{1}"],
        vec!["reflog", "delete", "x@{1}", "heads/main@{1}"],
        vec!["reflog", "delete", "--updateref", "main@{0}"],
        vec!["reflog", "delete", "--updateref", "--rewrite", "main@{0}"],
        vec!["reflog", "delete", "--updateref", "HEAD@{0}"],
        vec![
            "reflog",
            "delete",
            "--verbose",
            "main@{2023-11-14 22:13:21 +0000}",
        ],
        vec!["reflog", "delete", "--verbose", "main@{2001-01-01}"],
        vec!["reflog", "delete", "--verbose", "main@{now}"],
        vec!["reflog", "delete", "side@{0}"],
        vec!["reflog", "drop", "-h"],
        vec!["reflog", "drop"],
        vec!["reflog", "drop", "@"],
        vec!["reflog", "drop", "nope"],
        vec!["reflog", "drop", "--all", "main"],
        vec!["reflog", "drop", "--all"],
        vec!["reflog", "drop", "--all", "--single-worktree"],
        vec!["reflog", "drop", "side", "side"],
        vec!["reflog", "expire", "--updateref", "--expire=now", "main"],
        vec![
            "reflog",
            "expire",
            "--updateref",
            "--expire=1700000002",
            "--rewrite",
            "--all",
        ],
        vec!["reflog", "expire", "--stale-fix", "--all"],
        vec!["-c", "gc.reflogExpire=false", "reflog", "expire", "--all"],
        vec!["reflog", "expire", "HEAD@{0}"],
    ];
    for args in &cases {
        fx.compare(args)
            .checks([
                Check::ExitCode,
                Check::Stdout,
                Check::Stderr,
                Check::Refs,
                Check::Reflogs,
            ])
            .run();
    }
}

/// `--stale-fix` prunes entries whose commits lead to missing objects.
#[test]
fn expire_stale_fix() {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("f", "1\n")
        .git(["add", "f"])
        .git(["commit", "-q", "-m", "c1"])
        .tick()
        .write("f", "2\n")
        .git(["commit", "-q", "-am", "c2"])
        .tick()
        .write("f", "3\n")
        .git(["commit", "-q", "-am", "c3"])
        .git(["reset", "-q", "--hard", "HEAD~2"]);
    if fx.is_skipped() {
        return;
    }
    // The blob of the second commit goes missing.
    let blob = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD@{1}~1:f"]))
        .unwrap()
        .trim()
        .to_string();
    let mut fx = fx.remove(format!(".git/objects/{}/{}", &blob[..2], &blob[2..]));
    for args in [
        &[
            "reflog",
            "expire",
            "--stale-fix",
            "--verbose",
            "--expire=never",
            "--all",
        ][..],
        &[
            "reflog",
            "expire",
            "--stale-fix",
            "-n",
            "--verbose",
            "--all",
        ][..],
    ] {
        fx.compare(args)
            .checks([
                Check::ExitCode,
                Check::Stdout,
                Check::Stderr,
                Check::Reflogs,
            ])
            .run();
    }
}

/// A scripted session (commits, branch, switch, reset, rename): after
/// every step the files under `.git/logs/` written by rgit equal git's
/// (#325; push and fetch compare their reflogs in their own tests).
#[test]
fn session_reflogs() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "work"]);
    if fx.is_skipped() {
        return;
    }
    let steps: &[&[&str]] = &[
        &["commit", "-q", "--allow-empty", "-m", "one"],
        &["branch", "topic"],
        &["switch", "-q", "topic"],
        &["commit", "-q", "--allow-empty", "-m", "two"],
        &["switch", "-q", "-c", "other", "HEAD~1"],
        &["commit", "-q", "--allow-empty", "-m", "three"],
        &["reset", "-q", "--hard", "HEAD~1"],
        &["switch", "-q", "topic"],
        &["branch", "-m", "topic", "renamed"],
        &["reset", "-q", "--soft", "HEAD~1"],
        &["switch", "-q", "--detach", "other"],
        &["switch", "-q", "-"],
    ];
    for args in steps {
        fx.compare(*args)
            .in_dir("work")
            .repo("work")
            .checks([Check::ExitCode, Check::Refs, Check::Reflogs])
            .run();
    }
}
