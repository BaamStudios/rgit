// SPDX-License-Identifier: MIT
//! Differential tests of `rgit describe` and `rgit name-rev` against the
//! reference git: every commit of a branchy, tagged history is described
//! with several option sets (candidate choice, depth, `--debug` trace).

use rgit_difftest::{Check, Fixture, Normalizer};

fn commit(fx: Fixture, msg: &str) -> Fixture {
    fx.tick().git(["commit", "-q", "--allow-empty", "-m", msg])
}

fn tag(fx: Fixture, args: &[&str]) -> Fixture {
    let mut argv = vec!["tag"];
    argv.extend_from_slice(args);
    fx.tick().git(argv)
}

/// Three lines of history with merges between them, annotated and
/// lightweight tags, a renamed annotated tag, a remote-tracking ref and
/// commits sharing one date (ties in the walk).
fn history() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    if fx.is_skipped() {
        return fx;
    }
    fx = commit(fx, "root");
    fx = tag(fx, &["-a", "-m", "v0", "v0.0"]);
    for i in 1..=4 {
        fx = commit(fx, &format!("m{i}"));
    }
    fx = tag(fx, &["-a", "-m", "v1", "v1.0"]);
    fx = fx.git(["checkout", "-q", "-b", "topic", "main~2"]);
    for i in 1..=3 {
        fx = commit(fx, &format!("t{i}"));
    }
    fx = tag(fx, &["light-topic"]);
    fx = fx.git(["checkout", "-q", "-b", "other", "main~3"]);
    // Equal dates on this line (no tick).
    for i in 1..=3 {
        fx = fx.git(["commit", "-q", "--allow-empty", "-m", &format!("o{i}")]);
    }
    fx = fx.git(["tag", "-a", "-m", "o", "v0.5-other"]);
    fx = fx.git(["checkout", "-q", "main"]);
    for i in 5..=6 {
        fx = commit(fx, &format!("m{i}"));
    }
    fx = fx
        .tick()
        .git(["merge", "-q", "--no-ff", "-m", "merge topic", "topic"]);
    fx = tag(fx, &["-a", "-m", "v2", "v2.0-rc1"]);
    fx = commit(fx, "m7");
    fx = fx
        .tick()
        .git(["merge", "-q", "--no-ff", "-m", "merge other", "other"]);
    for i in 8..=9 {
        fx = commit(fx, &format!("m{i}"));
    }
    fx = tag(fx, &["light-main"]);
    fx = commit(fx, "m10");
    fx = fx
        .git(["update-ref", "refs/remotes/origin/main", "main~2"])
        .git(["update-ref", "refs/notes/x", "main~4"]);
    fx
}

fn sorted_lines() -> Normalizer {
    Normalizer::new("sort lines", |s| {
        let mut lines: Vec<&str> = s.lines().collect();
        lines.sort_unstable();
        lines.join("\n")
    })
}

fn commits(fx: &Fixture) -> Vec<String> {
    String::from_utf8(fx.git_stdout(["rev-list", "--all"]))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn describe_every_commit() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    let all = commits(&fx);
    assert!(all.len() > 15);
    let option_sets: &[&[&str]] = &[
        &[],
        &["--tags"],
        &["--all"],
        &["--long"],
        &["--first-parent"],
        &["--tags", "--first-parent"],
        &["--candidates=1"],
        &["--tags", "--candidates=2"],
        &["--exact-match"],
        &["--always", "--abbrev=0"],
        &["--tags", "--abbrev=12"],
        &["--all", "--match=v*", "--exclude=*rc*"],
        &["--all", "--match=origin/*"],
        &["--debug"],
        &["--tags", "--debug"],
        &["--all", "--debug", "--candidates=3"],
        &["--contains"],
        &["--contains", "--all"],
        &["--contains", "--match=v1*"],
    ];
    for opts in option_sets {
        for c in &all {
            let mut argv = vec!["describe"];
            argv.extend_from_slice(opts);
            argv.push(c);
            fx.compare(&argv).checks(Check::OUTPUT).run();
        }
    }
}

#[test]
fn describe_messages_and_forms() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    // A tag whose own name differs from its reference.
    let a = String::from_utf8(fx.git_stdout(["rev-parse", "refs/tags/v1.0"])).unwrap();
    fx = fx
        .git(["update-ref", "refs/tags/renamed", a.trim()])
        .git(["tag", "-a", "-m", "blob", "blob-tag", "HEAD:"]);
    let cases: &[&[&str]] = &[
        &[],
        &["HEAD", "main~3", "v1.0"],
        &["renamed"],
        &["--abbrev=0", "renamed^0"],
        &["--all", "renamed^0"],
        &["main~5^{tree}"],
        &["nosuch"],
        &["--long", "--abbrev=0"],
        &["--dirty", "HEAD"],
        &["--broken", "HEAD"],
        &["--dirty"],
        &["--dirty=.mod"],
        &["--match=nothing"],
        &["--match=nothing", "--always"],
        &["--exact-match", "--always", "main~1"],
        &["--candidates=-1", "main~1"],
        &["--no-exact-match", "--candidates=0", "main~1"],
        &["--abbrev=2", "--tags", "main~1"],
        &["--contains", "--always", "main"],
        &["blob-tag"],
        &["-h"],
    ];
    for args in cases {
        let mut argv = vec!["describe"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
}

#[test]
fn name_rev_forms() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    let all = commits(&fx);
    let mut text = String::from("start ");
    for c in &all {
        text.push_str(c);
        text.push_str(" and ");
        text.push_str(&c[..10]);
        text.push('\n');
    }
    let tree = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD^{tree}"])).unwrap();
    text.push_str(tree.trim());
    text.push('\n');
    for opts in [
        &[][..],
        &["--tags"],
        &["--name-only"],
        &["--tags", "--name-only"],
        &["--refs=v1*"],
        &["--refs=refs/tags/v1*"],
        &["--refs=origin/*"],
        &["--exclude=refs/tags/*"],
    ] {
        for c in &all {
            let mut argv = vec!["name-rev"];
            argv.extend_from_slice(opts);
            argv.push(c);
            fx.compare(&argv).checks(Check::OUTPUT).run();
        }
        let mut argv = vec!["name-rev", "--annotate-stdin"];
        argv.extend_from_slice(opts);
        fx.compare(&argv)
            .stdin(text.clone())
            .checks(Check::OUTPUT)
            .run();
        // `--all` lists commits in the order of git's object hash table,
        // which rgit only approximates.
        let mut argv = vec!["name-rev", "--all"];
        argv.extend_from_slice(opts);
        fx.compare(&argv)
            .checks(Check::OUTPUT)
            .normalize_stdout(sorted_lines())
            .run();
    }
    let cases: &[&[&str]] = &[
        &["v1.0", "light-main", "HEAD^{tree}", "nosuch", "HEAD"],
        &["--peel-tag", "v1.0"],
        &["--refs=nothing", "HEAD"],
        &["--refs=nothing", "--no-undefined", "--name-only", "HEAD"],
        &[
            "--refs=nothing",
            "--no-undefined",
            "--always",
            "--name-only",
            "HEAD",
        ],
        &["--all", "HEAD"],
        &["-h"],
    ];
    for args in cases {
        let mut argv = vec!["name-rev"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
}

#[test]
fn describe_blobs() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    if fx.is_skipped() {
        return;
    }
    fx = fx
        .write("a", "a\n")
        .write("dir/b", "unique\n")
        .git(["add", "."]);
    fx = commit(fx, "first");
    fx = tag(fx, &["-a", "-m", "t", "v1"]);
    fx = commit(fx, "empty");
    fx = fx.git(["rm", "-q", "dir/b"]);
    fx = commit(fx, "delete");
    fx = fx.write("c/b", "unique\n").git(["add", "."]);
    fx = commit(fx, "again");
    fx = fx
        .git(["tag", "blob-light", "HEAD:c/b"])
        .git(["tag", "-a", "-m", "x", "blob-ann", "HEAD:c/b"]);
    let blob = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD:c/b"])).unwrap();
    let cases: &[&[&str]] = &[
        &["HEAD:c/b"],
        &["HEAD:a"],
        &["--tags", "HEAD~3:dir/b"],
        &["blob-light"],
        &["blob-ann"],
        &[blob.trim()],
    ];
    for args in cases {
        let mut argv = vec!["describe"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
    fx = fx.write("x", "unreachable\n");
    let other = String::from_utf8(fx.git_stdout(["hash-object", "-w", "x"])).unwrap();
    fx.compare(["describe", other.trim()])
        .checks(Check::OUTPUT)
        .run();
    fx = fx.git(["symbolic-ref", "HEAD", "refs/heads/unborn"]);
    fx.compare(["describe", "blob-light"])
        .checks(Check::OUTPUT)
        .run();
}

/// A pseudo-random history of about 130 commits on four branches with
/// merges between them, annotated and lightweight tags and runs of equal
/// commit dates; every commit is described.
#[test]
fn describe_random_history() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "b0"]);
    if fx.is_skipped() {
        return;
    }
    fx = commit(fx, "root");
    for b in 1..4 {
        fx = fx.git(["branch", &format!("b{b}")]);
    }
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    for i in 0..130 {
        let branch = format!("b{}", next(4));
        fx = fx.git(["checkout", "-q", &branch]);
        if next(3) > 0 {
            fx = fx.tick();
        }
        if next(6) == 0 {
            let other = format!("b{}", next(4));
            fx = fx.git([
                "merge",
                "-q",
                "--no-ff",
                "-m",
                &format!("merge {i}"),
                &other,
            ]);
        } else {
            fx = fx.git(["commit", "-q", "--allow-empty", "-m", &format!("c{i}")]);
        }
        match next(9) {
            0 => fx = tag(fx, &["-a", "-m", "a", &format!("v{i}")]),
            1 => fx = fx.git(["tag", &format!("l{i}")]),
            _ => {}
        }
    }
    let all = commits(&fx);
    assert!(all.len() > 100);
    for opts in [
        &[][..],
        &["--tags"],
        &["--all", "--first-parent"],
        &["--tags", "--candidates=3", "--debug"],
    ] {
        for c in &all {
            let mut argv = vec!["describe"];
            argv.extend_from_slice(opts);
            argv.push(c);
            fx.compare(&argv).checks(Check::OUTPUT).run();
        }
    }
}
