// SPDX-License-Identifier: MIT
//! Differential tests of `switch` and `checkout <branch>` (#42): the work
//! tree and index update (local changes carried over or refused), `HEAD`,
//! branches, reflogs, upstream configuration and every message, against
//! the reference git.

use rgit_difftest::Fixture;

/// `main` (a2, b, c, file d) on top of `v1` (a, b, d/x); `feat` at `v1`,
/// `side` adds s/s. `main` is checked out.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .write("b", "b\n")
        .write("d/x", "x\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one\nsecond line"])
        .git(["tag", "v1"])
        .git(["branch", "feat"])
        .tick()
        .write("a", "a2\n")
        .write("c", "c\n")
        .remove("d")
        .write("d", "dfile\n")
        .git(["add", "-A"])
        .git(["commit", "-q", "-m", "two"])
        .git(["branch", "side"])
        .tick()
}

/// Runs each case in a fresh fixture prepared by `prep`.
fn cases(prep: fn(Fixture) -> Fixture, cases: &[&[&str]]) {
    for args in cases {
        let fx = prep(fixture());
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        fx.compare(*args).run();
    }
}

fn clean(fx: Fixture) -> Fixture {
    fx
}

#[test]
fn switch_clean() {
    cases(
        clean,
        &[
            &["switch", "feat"],
            &["switch", "main"],
            &["switch", "-q", "feat"],
            &["switch", "--detach", "v1"],
            &["switch", "--detach"],
            &["switch", "-d", "HEAD~1"],
            &["switch", "v1"],
            &["switch", "HEAD~1"],
            &["switch", "nope"],
            &["switch"],
            &["switch", "main", "feat"],
            &["switch", "-c", "new"],
            &["switch", "-c", "new", "v1"],
            &["switch", "-c", "new", "nope"],
            &["switch", "-c", "feat"],
            &["switch", "-c", ""],
            &["switch", "-C", "feat"],
            &["switch", "-C", "feat", "main"],
            &["switch", "-C", "main", "v1"],
            &["switch", "-c", "x", "-d"],
            &["switch", "--orphan", "o"],
            &["checkout", "feat"],
            &["checkout", "v1"],
            &["checkout", "--detach"],
            &["checkout", "--detach", "feat"],
            &["checkout", "--detach", "nope"],
            &["checkout", "HEAD~1"],
            &["checkout", "-q", "v1"],
            &["checkout", "-c", "advice.detachedHead=false", "v1"],
            &["checkout", "-b", "new"],
            &["checkout", "-b", "new", "v1"],
            &["checkout", "-b", "new", "nope"],
            &["checkout", "-B", "feat", "main"],
            &["checkout", "-B", "main", "v1"],
            &["checkout", "--orphan", "o"],
            &["checkout"],
            &["checkout", "HEAD"],
            &["checkout", "nope"],
        ],
    );
}

/// Detached at `v1` first.
#[test]
fn switch_from_detached() {
    cases(
        |fx| fx.git(["checkout", "-q", "v1"]),
        &[
            &["switch", "main"],
            &["switch", "feat"],
            &["switch", "--detach", "main"],
            &["checkout", "main"],
            &["checkout", "main~1"],
            &["checkout", "-"],
            &["switch", "-"],
            &["checkout", "v1"],
            &["switch", "-c", "new"],
        ],
    );
}

/// Local changes that are carried over or refuse the switch.
#[test]
fn switch_with_local_changes() {
    // b is the same in both trees: the change is carried over.
    cases(
        |fx| fx.write("b", "b local\n").write("u", "untracked\n"),
        &[
            &["switch", "feat"],
            &["checkout", "feat"],
            &["checkout", "-q", "feat"],
            &["switch", "--detach", "v1"],
            &["switch", "-f", "feat"],
            &["checkout"],
            &["checkout", "-f"],
            &["switch", "-c", "x", "main"],
            &["checkout", "-b", "x", "HEAD"],
            &["switch", "-c", "x"],
        ],
    );
    // a differs between the trees: refused unless forced.
    cases(
        |fx| fx.write("a", "a local\n"),
        &[
            &["switch", "feat"],
            &["checkout", "v1"],
            &["switch", "--discard-changes", "feat"],
            &["checkout", "-f", "feat"],
            &["switch", "-c", "x", "feat"],
            &["switch", "-c", "x"],
        ],
    );
    // A staged change matching neither tree.
    cases(
        |fx| fx.write("a", "staged\n").git(["add", "a"]),
        &[&["switch", "feat"], &["switch", "side"]],
    );
    // A staged change matching the target.
    cases(
        |fx| fx.write("a", "a\n").git(["add", "a"]),
        &[&["switch", "feat"]],
    );
    // Untracked files in the way of the target.
    cases(
        |fx| {
            fx.git(["checkout", "-q", "feat"])
                .write("c", "untracked\n")
                .write("d/y", "untracked\n")
        },
        &[&["switch", "main"], &["switch", "-f", "main"]],
    );
    // A touched but unchanged file does not count as a change.
    cases(|fx| fx.write("a", "a2\n"), &[&["switch", "feat"]]);
}

/// Upstream configuration and `--guess` in a clone.
#[test]
fn switch_tracking() {
    let prep = |fx: Fixture| {
        fx.git(["clone", "-q", ".", "clone"])
            .git(["-C", "clone", "config", "user.name", "C"])
    };
    let clone_cases: &[&[&str]] = &[
        &["switch", "feat"],
        &["checkout", "feat"],
        &["switch", "--no-guess", "feat"],
        &["checkout", "side"],
        &["switch", "main"],
        &["checkout", "main"],
        &["checkout", "-b", "x", "origin/feat"],
        &["checkout", "-b", "x", "--no-track", "origin/feat"],
        &["checkout", "-b", "x", "--track", "main"],
        &["switch", "-c", "x", "origin/feat"],
        &["switch", "-C", "main", "origin/feat"],
        &["checkout", "-B", "main", "HEAD~1"],
        &["checkout"],
        &["switch", "-q", "feat"],
    ];
    for args in clone_cases {
        let fx = prep(fixture());
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        fx.compare(*args).in_dir("clone").repo("clone").run();
    }
    // Ahead of and diverged from the upstream.
    for args in [&["switch", "main"][..], &["checkout", "main"][..]] {
        let fx = prep(fixture())
            .git(["-C", "clone", "switch", "-q", "feat"])
            .git(["-C", "clone", "branch", "-f", "main", "origin/main~1"])
            .git(["-C", "clone", "commit", "-q", "--allow-empty", "-m", "x"])
            .git(["-C", "clone", "branch", "-f", "main"]);
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        fx.compare(args).in_dir("clone").repo("clone").run();
    }
}

/// From an unborn branch.
#[test]
fn switch_from_unborn() {
    let make = || {
        fixture().git(["init", "-q", "-b", "main", "u"]).git([
            "-C",
            "u",
            "fetch",
            "-q",
            "..",
            "main:other",
        ])
    };
    let unborn_cases: &[&[&str]] = &[
        &["switch", "other"],
        &["checkout", "other"],
        &["switch", "-c", "x"],
        &["checkout", "-b", "x"],
    ];
    for args in unborn_cases {
        let fx = make();
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        fx.compare(*args).in_dir("u").repo("u").run();
    }
    let fx = make().write("u/a", "untracked\n");
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx;
    fx.compare(["switch", "other"]).in_dir("u").repo("u").run();
}

/// Runs each case in the clone of a fresh fixture prepared by `prep`.
fn clone_cases(prep: fn(Fixture) -> Fixture, cases: &[&[&str]]) {
    for args in cases {
        let fx = prep(fixture().git(["clone", "-q", ".", "clone"]).git([
            "-C",
            "clone",
            "config",
            "user.name",
            "C",
        ]));
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        fx.compare(*args).in_dir("clone").repo("clone").run();
    }
}

/// `-t`/`--no-track` without `-c`/`-b`, `--track=inherit`,
/// `branch.autoSetupMerge`/`autoSetupRebase` and `checkout.guess`
/// through the tracking code shared with `branch`.
#[test]
fn switch_track_options() {
    clone_cases(
        |fx| fx.git(["-C", "clone", "branch", "-q", "--no-track", "loc", "main"]),
        &[
            &["switch", "-t", "origin/feat"],
            &["switch", "--track", "refs/remotes/origin/feat"],
            &["switch", "--no-track", "origin/feat"],
            &["checkout", "-t", "origin/feat"],
            &["checkout", "--no-track", "origin/feat"],
            &["checkout", "-t", "origin/feat", "--"],
            &["checkout", "-t", "origin/feat", "a"],
            &["switch", "-t", "main"],
            &["checkout", "-t", "main"],
            &["switch", "-t"],
            &["checkout", "--no-track"],
            &["switch", "-t", "-d", "origin/feat"],
            &["switch", "-t", "--orphan", "o"],
            &["switch", "-t", "origin/feat", "origin/side"],
            &["switch", "-c", "x", "--track=inherit", "main"],
            &["checkout", "--track=inherit", "-b", "x", "main"],
            &["checkout", "--track=inherit", "-b", "x", "loc"],
            &["checkout", "--track=inherit", "-b", "x", "origin/feat"],
            &["switch", "-c", "x", "--track=direct", "origin/feat"],
            &["switch", "-c", "x", "--track=bogus", "origin/feat"],
            &["switch", "-c", "x", "-t"],
            &["switch", "-c", "x", "-t", "main"],
            &["switch", "-c", "x", "--track", "HEAD"],
            &["switch", "-c", "x", "-t", "nope"],
            &["switch", "-C", "loc", "-t", "origin/feat"],
            &["switch", "-c", "x", "origin/feat~0"],
            &["checkout", "-b", "x", "origin"],
            &["-c", "branch.autoSetupMerge=always", "switch", "-c", "x"],
            &[
                "-c",
                "branch.autoSetupMerge=always",
                "checkout",
                "-b",
                "x",
                "loc",
            ],
            &["-c", "branch.autoSetupMerge=false", "switch", "feat"],
            &[
                "-c",
                "branch.autoSetupMerge=simple",
                "switch",
                "-c",
                "x",
                "origin/feat",
            ],
            &[
                "-c",
                "branch.autoSetupMerge=simple",
                "switch",
                "-c",
                "feat",
                "origin/feat",
            ],
            &[
                "-c",
                "branch.autoSetupRebase=always",
                "switch",
                "-c",
                "x",
                "origin/feat",
            ],
            &["-c", "branch.autoSetupMerge=bogus", "switch", "main"],
            &["-c", "branch.autoSetupRebase=bogus", "checkout", "main"],
            &["-c", "checkout.guess=false", "checkout", "feat"],
            &["-c", "checkout.guess=false", "switch", "feat"],
            &["-c", "checkout.guess=false", "switch", "--guess", "feat"],
            &["checkout", "feat", "--"],
            &["checkout", "feat", "--", "a"],
            &["checkout", "nope", "--"],
        ],
    );
}

/// `--guess` with the branch in two remotes: refused with the advice,
/// unless `checkout.defaultRemote` picks one.
#[test]
fn switch_guess_ambiguous() {
    clone_cases(
        |fx| {
            fx.git(["-C", "clone", "remote", "add", "second", ".."])
                .git(["-C", "clone", "fetch", "-q", "second"])
        },
        &[
            &["switch", "feat"],
            &["checkout", "feat"],
            &["-c", "checkout.defaultRemote=second", "switch", "feat"],
            &["-c", "checkout.defaultRemote=origin", "checkout", "feat"],
            &["-c", "checkout.defaultRemote=nope", "checkout", "feat"],
            &[
                "-c",
                "advice.checkoutAmbiguousRemoteBranchName=false",
                "switch",
                "feat",
            ],
            &[
                "-c",
                "advice.checkoutAmbiguousRemoteBranchName=true",
                "switch",
                "feat",
            ],
            &["--no-advice", "checkout", "feat"],
            &["switch", "-t", "second/feat"],
        ],
    );
}

/// `@{upstream}` as the branch to switch to.
#[test]
fn switch_to_upstream() {
    clone_cases(
        |fx| fx.git(["-C", "clone", "checkout", "-q", "-t", "-b", "loc", "main"]),
        &[
            &["checkout", "@{u}"],
            &["switch", "@{upstream}"],
            &["switch", "loc@{u}"],
            &["checkout", "main@{u}"],
            &["switch", "main@{u}"],
            &["switch", "--detach", "main@{u}"],
            &["checkout", "-b", "x", "@{u}"],
            &["checkout", "nope@{u}"],
            &["switch", "feat@{u}"],
        ],
    );
}

/// Leaving a detached `HEAD` with commits no ref reaches.
#[test]
fn switch_leaving_commits_behind() {
    fn commits(fx: Fixture, n: usize) -> Fixture {
        let mut fx = fx.git(["checkout", "-q", "--detach", "main"]);
        for i in 0..n {
            let m = format!("lost {i}\n\nbody");
            fx = fx
                .tick()
                .git(["commit", "-q", "--allow-empty", "-m", m.as_str()]);
        }
        fx
    }
    let leaving: &[&[&str]] = &[
        &["switch", "main"],
        &["checkout", "main"],
        &["checkout", "feat"],
        &["switch", "-q", "main"],
        &["switch", "--detach", "feat"],
        &["checkout", "v1"],
        &["switch", "-c", "x", "v1"],
        &["switch", "-c", "x"],
        &["switch", "--orphan", "o"],
        &["checkout", "--orphan", "o"],
        &["checkout", "-f", "main"],
        &["checkout", "HEAD~1"],
        &["-c", "advice.detachedHead=false", "checkout", "main"],
        &["-c", "core.abbrev=12", "checkout", "main"],
    ];
    cases(|fx| commits(fx, 1), leaving);
    cases(
        |fx| commits(fx, 5),
        &[&["switch", "main"], &["checkout", "HEAD~2"]],
    );
    cases(
        |fx| commits(fx, 6),
        &[&["switch", "main"], &["checkout", "main"]],
    );
    // Commits reachable from another ref are not lost.
    cases(
        |fx| commits(fx, 2).git(["update-ref", "refs/x/keep", "HEAD~1"]),
        &[&["switch", "main"]],
    );
    cases(
        |fx| commits(fx, 2).git(["tag", "t"]),
        &[&["switch", "main"]],
    );
}
