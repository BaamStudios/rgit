// SPDX-License-Identifier: MIT
//! Differential tests of `rgit merge-base` and `rev-list --left-right
//! --count` against the reference git.

use rgit_difftest::{Check, Fixture};

fn commit(fx: Fixture, msg: &str) -> Fixture {
    fx.tick().git(["commit", "-q", "--allow-empty", "-m", msg])
}

/// root - m1 - mm (merges s1) on main; root - s1 - ms (merges m1) on side;
/// third branches from root; skew branches from m1 with old dates.
fn criss_cross() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    let fx = commit(fx, "root")
        .git(["branch", "side"])
        .git(["branch", "third"]);
    let fx = commit(fx, "m1").git(["checkout", "-q", "side"]);
    let fx = commit(fx, "s1").git(["checkout", "-q", "third"]);
    let fx = commit(fx, "t1").git(["checkout", "-q", "main"]);
    let fx = fx
        .tick()
        .git(["merge", "-q", "--no-edit", "-m", "mm", "side"])
        .git(["checkout", "-q", "side"])
        .tick()
        .git(["merge", "-q", "--no-edit", "-m", "ms", "main~1"])
        .git(["checkout", "-q", "-b", "skew", "main~1"])
        .env("GIT_COMMITTER_DATE", "1000000000 +0000");
    let fx = commit(fx, "old1");
    commit(fx, "old2").git(["checkout", "-q", "main"])
}

#[test]
fn merge_base_modes() {
    let mut fx = criss_cross();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["main", "side"],
        &["--all", "main", "side"],
        &["-a", "side", "main"],
        &["--all", "main", "side", "third"],
        &["main", "main~1"],
        &["main", "third"],
        &["skew", "side"],
        &["--all", "skew", "main"],
        &["--octopus", "main", "side"],
        &["--octopus", "-a", "main", "side"],
        &["--octopus", "main", "side", "third"],
        &["--octopus", "main"],
        &["--octopus"],
        &["--independent", "main", "side", "main~1", "third", "main"],
        &["--independent", "skew", "main~1"],
        &["--independent"],
        &["--is-ancestor", "main~1", "main"],
        &["--is-ancestor", "main", "main~1"],
        &["--is-ancestor", "main~1", "skew"],
        &["--is-ancestor", "main", "main"],
        &["v1", "main"],
        &["--all", "main", "HEAD^{tree}"],
    ];
    fx = fx.git(["tag", "-a", "-m", "tag", "v1", "side"]);
    for args in cases {
        let mut argv = vec!["merge-base"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
}

#[test]
fn merge_base_errors() {
    let mut fx = criss_cross();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &[],
        &["main"],
        &["-h"],
        &["--is-ancestor", "main"],
        &["--is-ancestor", "main", "side", "third"],
        &["--is-ancestor", "--all", "main", "side"],
        &["--independent", "-a", "main"],
        &["--octopus", "--independent", "main"],
        &["--fork-point"],
        &["--fork-point", "a", "b", "c"],
        &["nope", "main"],
        &["--is-ancestor", "main", "nope"],
        &["main", "HEAD^{tree}"],
    ];
    for args in cases {
        let mut argv = vec!["merge-base"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
}

#[test]
fn left_right_count() {
    let mut fx = criss_cross();
    if fx.is_skipped() {
        return;
    }
    for range in [
        "main...side",
        "side...main",
        "main...third",
        "skew...side",
        "main~1...main",
        "main...main",
        "...side",
        "v1...main",
    ] {
        fx = fx.git(["tag", "-f", "-a", "-m", "tag", "v1", "side"]);
        fx.compare(["rev-list", "--left-right", "--count", range])
            .checks(Check::OUTPUT)
            .run();
    }
    fx.compare(["rev-list", "--count", "--left-right", "nope...main"])
        .checks(Check::OUTPUT)
        .run();
}
