// SPDX-License-Identifier: MIT
//! The builtins that complete `--list-cmds=builtins` (#747): synonyms
//! (`fsck-objects`, `stage`, `pickaxe`, `merge-recursive-ours`/`-theirs`),
//! the deprecated `whatchanged`, the helpers rgit answers in part
//! (`backfill`, `checkout--worker`, `fsmonitor--daemon`) and the `-h`
//! output of `diff` and `diff-pairs`, against the reference git.

use rgit_difftest::{Check, Fixture};

/// `main` and `side` forked from `base`, changing the same line, and
/// merged with `-s ours`.
fn repo() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("f", "1\n2\n3\n")
        .git(["add", "f"])
        .git(["commit", "-qm", "base"])
        .git(["checkout", "-qb", "side"])
        .write("f", "1\nS\n3\n")
        .git(["commit", "-qam", "side"])
        .git(["checkout", "-q", "-"])
        .write("f", "1\nM\n3\n")
        .write("g", "new\n")
        .git(["add", "g"])
        .git(["commit", "-qam", "main"])
        .git(["merge", "-q", "--no-edit", "-s", "ours", "side"])
}

#[test]
fn whatchanged_same_as_git() {
    let mut fx = repo();
    let cases: &[&[&str]] = &[
        &[],
        &["-1"],
        &["--i-still-use-this"],
        &["--i-still-use-this", "-p"],
        &["-1", "--stat", "--i-still-use-this"],
        &["--i-still-use-this", "-s"],
        &["--i-still-use-this", "--merges"],
        &["--i-still-use-this", "--oneline", "--", "g"],
    ];
    for args in cases {
        let mut argv = vec!["whatchanged"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
}

#[test]
fn synonyms_same_as_git() {
    let mut fx = repo();
    fx.compare(["fsck-objects"]).checks(Check::OUTPUT).run();
    fx.compare(["pickaxe", "-h"]).checks(Check::OUTPUT).run();
    fx.compare(["pickaxe", "f"]).checks(Check::OUTPUT).run();
    fx.compare(["merge-recursive-ours", "-h"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["merge-recursive-theirs", "a", "b"])
        .checks(Check::OUTPUT)
        .run();
    let mut fx = fx.write("h", "x\n");
    fx.compare(["stage", "h"]).checks(Check::ALL).run();
}

#[test]
fn helpers_same_as_git() {
    let mut fx = repo();
    for args in [
        &["fsmonitor--daemon"][..],
        &["fsmonitor--daemon", "-h"],
        &["fsmonitor--daemon", "status"],
        &["fsmonitor--daemon", "stop"],
        &["fsmonitor--daemon", "bogus"],
        &["checkout--worker", "-h"],
        &["checkout--worker"],
        &["checkout--worker", "--bogus"],
        &["backfill", "-h"],
        &["backfill"],
        &["backfill", "--no-sparse", "--min-batch-size=10"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}

#[test]
fn diff_help_same_as_git() {
    let mut fx = repo();
    for args in [
        &["diff", "-h"][..],
        &["diff", "--help-all"],
        &["diff-pairs", "-h"],
        &["diff-pairs", "--bogus"],
        &["range-diff", "-h"],
        &["repack", "-mx"],
        &["write-tree", "-h"],
        &["hook", "-h"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
    // Outside a repository `diff` is `diff --no-index`.
    let mut fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    let ceiling = fx.work_dir();
    for args in [&["diff", "-h"][..], &["diff", "--help-all"]] {
        fx.compare(args)
            .env("GIT_CEILING_DIRECTORIES", &ceiling)
            .checks(Check::OUTPUT)
            .run();
    }
}
