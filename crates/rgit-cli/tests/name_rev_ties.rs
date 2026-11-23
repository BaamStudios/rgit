// SPDX-License-Identifier: MIT
//! `name-rev` choices between competing names against the reference git
//! (#409): a topic merged twice, a merge parent against a longer first
//! parent line, and tags at different distances and dates.

use rgit_difftest::{Check, Fixture, Normalizer};

fn commit(fx: Fixture, msg: &str) -> Fixture {
    fx.tick().git(["commit", "-q", "--allow-empty", "-m", msg])
}

fn sorted() -> Normalizer {
    Normalizer::new("sorted lines", |s: &str| {
        let mut v: Vec<&str> = s.lines().collect();
        v.sort_unstable();
        v.join("\n")
    })
}

/// A topic merged into `main` early (at `topic~6`) and again at its tip,
/// with a tag on top; plus a second tag that reaches the topic directly.
fn merged_twice() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    if fx.is_skipped() {
        return fx;
    }
    fx = commit(fx, "root");
    fx = fx.git(["switch", "-q", "-c", "topic"]);
    for i in 0..9 {
        fx = commit(fx, &format!("t{i}"));
    }
    fx = fx.git(["switch", "-q", "main"]);
    for i in 0..3 {
        fx = commit(fx, &format!("m{i}"));
    }
    fx = fx
        .tick()
        .git(["merge", "-q", "--no-ff", "-m", "early", "topic~6"]);
    for i in 0..5 {
        fx = commit(fx, &format!("n{i}"));
    }
    fx = fx
        .tick()
        .git(["merge", "-q", "--no-ff", "-m", "late", "topic"]);
    fx = commit(fx, "p0");
    fx = fx.tick().git(["tag", "-a", "-m", "new", "new"]);
    // An older tag two steps above the topic's tip.
    fx = fx.git(["switch", "-q", "-c", "side", "topic"]);
    fx = commit(fx, "s0");
    fx = commit(fx, "s1");
    fx.tick()
        .env("GIT_COMMITTER_DATE", "1100000000 +0000")
        .git(["tag", "-a", "-m", "old", "old"])
        .git(["switch", "-q", "main"])
}

#[test]
fn competing_names() {
    let mut fx = merged_twice();
    if fx.is_skipped() {
        return;
    }
    fx.compare(["name-rev", "--all"])
        .checks(Check::OUTPUT)
        .normalize_stdout(sorted())
        .run();
    fx.compare(["name-rev", "--tags", "--all"])
        .checks(Check::OUTPUT)
        .normalize_stdout(sorted())
        .run();
    for rev in ["topic~6", "topic~7", "topic~2", "topic", "topic~8"] {
        fx.compare(["name-rev", rev]).checks(Check::OUTPUT).run();
        fx.compare(["describe", "--contains", rev])
            .checks(Check::OUTPUT)
            .run();
    }
}
