// SPDX-License-Identifier: MIT
//! Differential tests of `rgit show-branch` and `rgit pack-redundant`
//! against the reference git.

use rgit_difftest::{Check, Fixture};

fn commit(fx: Fixture, msg: &str) -> Fixture {
    fx.tick().git(["commit", "-q", "--allow-empty", "-m", msg])
}

fn merge(fx: Fixture, msg: &str, other: &str) -> Fixture {
    fx.tick()
        .git(["merge", "-q", "--no-ff", "-s", "ours", "-m", msg, other])
}

/// main with a merged topic, side and side2 (which merges side), clock
/// skew on skew, tags, remote-tracking refs and a branch reflog.
fn history() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    let fx = commit(commit(fx, "one"), "two").git(["checkout", "-q", "-b", "topic"]);
    let fx = commit(commit(fx, "t1"), "t2").git(["checkout", "-q", "main"]);
    let fx = commit(fx, "m3");
    let fx = merge(fx, "Merge topic", "topic");
    let fx = commit(fx, "m4").git(["checkout", "-q", "-b", "side", "main~3"]);
    let fx = commit(commit(fx, "s1"), "s2").git(["checkout", "-q", "-b", "side2", "topic^"]);
    let fx = commit(fx, "x1");
    let fx = merge(fx, "Merge side", "side")
        .git(["checkout", "-q", "-b", "skew", "main~2"])
        .env("GIT_COMMITTER_DATE", "1000000000 +0000");
    let fx = commit(commit(fx, "old1"), "old2");
    let fx = fx.tick();
    commit(fx, "new3")
        .git(["checkout", "-q", "main"])
        .git(["tag", "v1", "main~2"])
        .git(["tag", "-a", "-m", "ann", "v2", "side"])
        .git(["update-ref", "refs/remotes/origin/main", "main~1"])
        .git(["update-ref", "refs/remotes/origin/feat", "side2~1"])
}

#[test]
fn show_branch_layouts() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &[],
        &["--topo-order"],
        &["--date-order"],
        &["--sparse"],
        &["--more=1"],
        &["--more=4"],
        &["--more=40"],
        &["--more"],
        &["--list"],
        &["--more=-2"],
        &["--sha1-name"],
        &["--no-name"],
        &["--no-name", "--sha1-name"],
        &["--topics", "main", "side", "side2"],
        &["--topics", "side"],
        &["--independent"],
        &["--merge-base"],
        &["--merge-base", "side", "side2"],
        &["--merge-base", "--independent", "main", "skew"],
        &["main~1", "HEAD", "refs/heads/side", "side2^"],
        &["heads/main", "refs/heads/main", "main", "side"],
        &["main", "main", "side"],
        &["si*"],
        &["heads/s*", "main"],
        &["--list", "tags/*", "v*", "origin/*", "*/*"],
        &["--list", "zz*"],
        &["v1", "v2", "tags/v1"],
        &["-r"],
        &["-a"],
        &["-a", "--list", "side"],
        &["--current", "side", "side2"],
        &["--color=always"],
        &["--color=always", "--sparse", "skew", "main", "side2"],
        &["--color=never"],
        &["skew"],
        &["skew", "--more=3"],
        &["main^{tree}", "main"],
    ];
    for args in cases {
        let mut argv = vec!["show-branch"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
}

#[test]
fn show_branch_config_and_errors() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["nope"],
        &["main~2..main"],
        &["--list", "--merge-base"],
        &["--more=2", "--independent"],
        &["--all", "--reflog"],
        &["--reflog", "--current"],
        &["--merge-base", "--reflog"],
        &["--independent", "--reflog"],
        &["--reflog", "--more=1"],
        &["--no-topo-order"],
        &["--no-reflog"],
        &["--reflog=x", "main"],
        &["--reflog=28", "main"],
        &["--reflog", "main", "side"],
        &["--reflog", "nope"],
        &["--color=bad"],
        &["--more=x"],
    ];
    for args in cases {
        let mut argv = vec!["show-branch"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
    fx = fx
        .git(["config", "showbranch.default", "--topo-order"])
        .git(["config", "--add", "showbranch.default", "s*"]);
    fx.compare(["show-branch"]).checks(Check::OUTPUT).run();
    fx.compare(["show-branch", "main"])
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn show_branch_reflog() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let fx = fx.git(["checkout", "-q", "side"]);
    let fx = commit(fx, "r1").git(["reset", "-q", "--hard", "HEAD^"]);
    let mut fx = commit(commit(fx, "r2"), "r3");
    let now = (fx.time() + 30).to_string();
    let cases: &[&[&str]] = &[
        &["--reflog"],
        &["--reflog", "side"],
        &["-g3", "side"],
        &["--reflog=3,2", "side"],
        &["--reflog=,1", "side"],
        &["--reflog=2,now", "--list", "side"],
        &["--reflog=2,1 year ago", "side"],
        &["--reflog", "--list", "heads/side"],
        &["--reflog=2", "--sha1-name", "HEAD"],
        &["--reflog=2", "--color=always", "side"],
        &["--reflog", "v1"],
        &["--reflog", "--topics", "side"],
    ];
    for args in cases {
        let mut argv = vec!["show-branch"];
        argv.extend_from_slice(args);
        fx.compare(&argv)
            .env("GIT_TEST_DATE_NOW", &now)
            .checks(Check::OUTPUT)
            .run();
    }
}

#[test]
fn show_branch_without_commits() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["show-branch"][..],
        &["show-branch", "--list"],
        &["show-branch", "-g"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}

#[test]
fn pack_redundant() {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx;
    fx.compare(["pack-redundant", "--i-still-use-this", "--all"])
        .checks(Check::OUTPUT)
        .run();
    // One pack all of whose objects are in the other one (the order of
    // several redundant packs follows their file times, which the copies
    // of the fixture do not keep).
    let fx = commit(fx, "one").git(["repack", "-q"]);
    let mut fx = commit(fx, "two").git(["repack", "-q", "-a"]);
    let cases: &[&[&str]] = &[
        &["pack-redundant"],
        &["pack-redundant", "--all"],
        &["pack-redundant", "--i-still-use-this", "--all"],
        &["pack-redundant", "--i-still-use-this", "--all", "--verbose"],
        &["pack-redundant", "--all", "--i-still-use-this", "--alt-odb"],
        &["pack-redundant", "--i-still-use-this"],
        &["pack-redundant", "--i-still-use-this", "--bogus"],
        &["pack-redundant", "--i-still-use-this", "-h"],
        &["pack-redundant", "-h"],
        &["pack-redundant", "--i-still-use-this", "foo"],
        &[
            "pack-redundant",
            "--i-still-use-this",
            "pack-0123456789012345678901234567890123456789.pack",
        ],
    ];
    for args in cases {
        fx.compare(*args).checks(Check::OUTPUT).run();
    }
    fx.compare(["pack-redundant", "--i-still-use-this", "--all"])
        .stdin("nonsense\n")
        .checks(Check::OUTPUT)
        .run();
}
