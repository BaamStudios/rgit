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

/// A random history (`seed`): 60 commits with merges, 25 branches and 10
/// annotated tags with dates older and newer than the branches.
fn random_history(seed: u64) -> Fixture {
    let mut state = seed;
    let mut next = move |n: usize| -> usize {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((state >> 33) as usize) % n
    };
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    if fx.is_skipped() {
        return fx;
    }
    let tree = String::from_utf8(fx.git_stdout(["write-tree"])).unwrap();
    let tree = tree.trim().to_owned();
    let mut commits: Vec<String> = Vec::new();
    for i in 0..60 {
        let mut args = vec!["commit-tree".to_owned(), tree.clone()];
        args.extend(["-m".to_owned(), format!("c{i}")]);
        if !commits.is_empty() {
            let p = commits[next(commits.len())].clone();
            args.extend(["-p".to_owned(), p.clone()]);
            if commits.len() > 2 && next(10) < 3 {
                let q = commits[next(commits.len())].clone();
                if q != p {
                    args.extend(["-p".to_owned(), q]);
                }
            }
        }
        fx = fx.tick();
        let id = String::from_utf8(fx.git_stdout(&args)).unwrap();
        commits.push(id.trim().to_owned());
    }
    for b in 0..25 {
        let c = commits[next(commits.len())].clone();
        fx = fx.git(["update-ref", &format!("refs/heads/b{b}"), &c]);
    }
    let base = fx.time();
    for t in 0..10 {
        let c = commits[next(commits.len())].clone();
        let date = format!("{} +0000", base - 4000 + 100 * next(60) as i64);
        fx =
            fx.env("GIT_COMMITTER_DATE", &date)
                .git(["tag", "-a", "-m", "t", &format!("v{t}"), &c]);
    }
    fx
}

/// `name-rev --all` lists the commits in the order of git's object table,
/// which depends on when each object was looked up (#409).
#[test]
fn all_order_on_random_histories() {
    for seed in 1..=30 {
        let mut fx = random_history(seed);
        if fx.is_skipped() {
            return;
        }
        fx.compare(["name-rev", "--all"])
            .checks(Check::OUTPUT)
            .run();
    }
}
