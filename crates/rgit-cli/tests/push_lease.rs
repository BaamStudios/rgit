// SPDX-License-Identifier: MIT
//! Differential tests of `push --force-with-lease` and
//! `--force-if-includes` / `push.useForceIfIncludes` (#165): stale leases,
//! explicit and tracking expectations, `--force` over a lease, `--atomic`
//! and `--porcelain` output, the reflog check after a background fetch.

use rgit_difftest::{Check, Fixture};

mod common;
use common::servers::server_path;

const CHECKS: [Check; 4] = [Check::ExitCode, Check::Stdout, Check::Stderr, Check::Refs];

/// `r.git` has `main` at "four" (pushed from the clone `o`) and `bar` at
/// "two"; `w` tracks `main` at "two" and has rewritten it to "three" on
/// top of "one".
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "--bare", "r.git"])
        .git(["init", "-q", "-b", "main", "w"])
        .git(["-C", "w", "commit", "-q", "--allow-empty", "-m", "one"])
        .tick()
        .git(["-C", "w", "commit", "-q", "--allow-empty", "-m", "two"])
        .git(["-C", "w", "remote", "add", "origin", "../r.git"])
        .git([
            "-C",
            "w",
            "push",
            "-q",
            "origin",
            "main",
            "main:refs/heads/bar",
        ])
        .git(["clone", "-q", "r.git", "o"])
        .tick()
        .git(["-C", "o", "commit", "-q", "--allow-empty", "-m", "four"])
        .git(["-C", "o", "push", "-q", "origin", "main"])
        .git(["-C", "w", "reset", "-q", "--hard", "HEAD~1"])
        .tick()
        .git(["-C", "w", "commit", "-q", "--allow-empty", "-m", "three"])
}

fn check(fx: &mut Fixture, argv: &[&str]) {
    let path = server_path(fx);
    fx.compare(argv)
        .in_dir("w")
        .repo("r.git")
        .env("PATH", path)
        .checks(CHECKS)
        .run();
}

#[test]
fn force_with_lease_same_as_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["push", "--force-with-lease", "origin", "main"],
        &["push", "--force-with-lease=main:main", "origin", "main"],
        &[
            "push",
            "--porcelain",
            "--force-with-lease=main",
            "origin",
            "main",
        ],
        &["push", "--force-with-lease=main:", "origin", "main"],
        &["push", "--force-with-lease=main:nosuch", "origin", "main"],
        &["push", "--force-with-lease=xyz", "origin", "main"],
        &[
            "push",
            "--force-with-lease=main:main",
            "--no-force-with-lease",
            "origin",
            "main",
        ],
        &[
            "push",
            "--atomic",
            "--force-with-lease=main:main",
            "origin",
            "main",
            "main:refs/heads/new",
        ],
        &["push", "--force-with-lease=bar:main", "origin", "main:bar"],
        &["push", "--force-with-lease=new", "-n", "origin", "main:new"],
        &["push", "--force-with-lease=new:main", "origin", "main:new"],
        &["push", "--force-with-lease=bar", "origin", "HEAD~1:bar"],
        &[
            "push",
            "--force-with-lease",
            "--force-if-includes",
            "origin",
            "main",
        ],
        &[
            "push",
            "--force",
            "--force-with-lease=main:main",
            "origin",
            "main",
        ],
    ];
    for argv in cases {
        check(&mut fx, argv);
    }
}

#[test]
fn force_if_includes_same_as_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    // A background fetch: the tracking ref now has "four", which "three"
    // never included.
    fx = fx.git(["-C", "w", "fetch", "-q"]);
    let cases: &[&[&str]] = &[
        &["push", "-n", "--force-with-lease", "origin", "main"],
        &[
            "push",
            "--force-with-lease",
            "--force-if-includes",
            "origin",
            "main",
        ],
        &[
            "push",
            "--porcelain",
            "--force-with-lease=main",
            "--force-if-includes",
            "origin",
            "main",
        ],
        &[
            "push",
            "--atomic",
            "--force-with-lease",
            "--force-if-includes",
            "origin",
            "main",
            "main:refs/heads/o2",
        ],
        &[
            "-c",
            "push.useForceIfIncludes=true",
            "push",
            "--force-with-lease=main",
            "origin",
            "main",
        ],
        &[
            "-c",
            "push.useForceIfIncludes=true",
            "push",
            "--no-force-if-includes",
            "-n",
            "--force-with-lease=main",
            "origin",
            "main",
        ],
        &[
            "push",
            "--force-if-includes",
            "-n",
            "--force-with-lease=main:origin/main",
            "origin",
            "main",
        ],
    ];
    for argv in cases {
        check(&mut fx, argv);
    }
    // After a rebase onto the fetched tip the update is allowed.
    fx = fx.git(["-C", "w", "rebase", "-q", "origin/main"]);
    check(
        &mut fx,
        &[
            "push",
            "--force-with-lease",
            "--force-if-includes",
            "origin",
            "main",
        ],
    );
}
