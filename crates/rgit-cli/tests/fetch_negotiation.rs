// SPDX-License-Identifier: MIT
//! Differential tests of `fetch.negotiationAlgorithm` (`consecutive`,
//! `skipping`, `noop`, unknown values), `--[no-]show-forced-updates` /
//! `fetch.showForcedUpdates`, `--porcelain` lines for `FETCH_HEAD`, merge
//! candidates of `--tags` from a URL and `--set-upstream` with a tag
//! (#164, #328).

use rgit_difftest::{Check, Fixture};

mod common;
use common::servers::server_path;

const CHECKS: [Check; 6] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Refs,
    Check::Reflogs,
    Check::GitFiles,
];

fn commit(fx: Fixture, dir: &str, msg: &str) -> Fixture {
    fx.git(["-C", dir, "commit", "-q", "--allow-empty", "-m", msg])
        .tick()
}

/// `c` is a clone of `one`, whose `main` was then rewritten.
fn fixture() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "one"]);
    let fx = commit(fx, "one", "a");
    let fx = commit(fx, "one", "b")
        .git(["-C", "one", "tag", "t1"])
        .git(["clone", "-q", "one", "c"]);
    let fx = commit(fx, "c", "local");
    let fx = fx.git(["-C", "one", "reset", "-q", "--hard", "HEAD~1"]);
    commit(fx, "one", "x").git(["-C", "one", "tag", "-a", "-m", "m", "t2"])
}

fn check(fx: &mut Fixture, argv: &[&str]) {
    let path = server_path(fx);
    fx.compare(argv)
        .in_dir("c")
        .repo("c")
        .env("PATH", path)
        .checks(CHECKS)
        .run();
}

#[test]
fn fetch_forced_updates_and_porcelain_same_as_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        // (git warns once more when --dry-run looks at the tags again.)
        &[
            "-c",
            "advice.fetchShowForcedUpdates=false",
            "-c",
            "fetch.showForcedUpdates=false",
            "fetch",
            "--dry-run",
            "origin",
        ],
        &[
            "-c",
            "fetch.showForcedUpdates=false",
            "fetch",
            "--show-forced-updates",
            "--dry-run",
            "origin",
        ],
        &["fetch", "--porcelain", "--no-show-forced-updates", "origin"],
        &["fetch", "--porcelain", "origin", "HEAD"],
        &["fetch", "--porcelain", "origin", "HEAD:refs/heads/foo"],
        // Refused whatever comes of it; without the option the submodules
        // are left alone.
        &["fetch", "--porcelain", "--recurse-submodules", "origin"],
        &[
            "fetch",
            "--porcelain",
            "--recurse-submodules=on-demand",
            "origin",
        ],
        &[
            "fetch",
            "--porcelain",
            "--dry-run",
            "--recurse-submodules=no",
            "origin",
        ],
        &["fetch", "--tags", "../one"],
        &["fetch", "../one", "tag", "t2"],
        &["fetch", "--set-upstream", "origin", "tag", "t1"],
    ];
    for argv in cases {
        check(&mut fx, argv);
    }
}

#[test]
fn negotiation_algorithms_same_as_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["-c", "fetch.negotiationAlgorithm=bogus", "fetch", "origin"],
        &[
            "-c",
            "fetch.negotiationAlgorithm=noop",
            "fetch",
            "--dry-run",
            "origin",
        ],
        &[
            "-c",
            "fetch.negotiationAlgorithm=Skipping",
            "fetch",
            "--dry-run",
            "origin",
        ],
        &[
            "-c",
            "feature.experimental=true",
            "-c",
            "fetch.negotiationAlgorithm=default",
            "fetch",
            "origin",
        ],
    ];
    for argv in cases {
        check(&mut fx, argv);
    }
}
