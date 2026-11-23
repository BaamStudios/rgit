// SPDX-License-Identifier: MIT
//! Differential tests of `fetch --negotiate-only` (#164) and the
//! `push.negotiate` push that uses it (#165).

use rgit_difftest::{Check, Fixture};

mod common;
use common::servers::server_path;

const CHECKS: [Check; 4] = [Check::ExitCode, Check::Stdout, Check::Stderr, Check::Refs];

/// `server` has one, two (tags) and four; `client` cloned it at two and
/// added three. `hidden.git` hides the only ref the client shares with it.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "server"])
        .git(["-C", "server", "commit", "-q", "--allow-empty", "-m", "one"])
        .git(["-C", "server", "tag", "one"])
        .tick()
        .git(["-C", "server", "commit", "-q", "--allow-empty", "-m", "two"])
        .git(["-C", "server", "tag", "two"])
        .git(["clone", "-q", "server", "client"])
        .tick()
        .git([
            "-C",
            "client",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "three",
        ])
        .tick()
        .git([
            "-C",
            "server",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "four",
        ])
        .git(["init", "-q", "--bare", "hidden.git"])
        .git([
            "-C",
            "client",
            "push",
            "-q",
            "../hidden.git",
            "HEAD~1:refs/remotes/origin/first",
        ])
        .git([
            "-C",
            "hidden.git",
            "config",
            "receive.hideRefs",
            "refs/remotes/origin/first",
        ])
}

#[test]
fn negotiate_only_same_as_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &[
            "-c",
            "protocol.version=2",
            "fetch",
            "--negotiate-only",
            "--negotiation-tip=HEAD",
            "origin",
        ],
        &[
            "-c",
            "protocol.version=2",
            "fetch",
            "--negotiate-only",
            "--negotiation-tip=HEAD",
        ],
        &[
            "-c",
            "protocol.version=2",
            "fetch",
            "--negotiate-only",
            "origin",
        ],
        &[
            "-c",
            "protocol.version=0",
            "fetch",
            "--negotiate-only",
            "--negotiation-tip=HEAD",
            "origin",
        ],
        &[
            "fetch",
            "--negotiate-only",
            "--recurse-submodules",
            "origin",
        ],
        &[
            "-c",
            "remote.origin.negotiationRestrict=refs/heads/main",
            "fetch",
            "--negotiate-only",
            "origin",
        ],
        &[
            "-c",
            "push.negotiate=1",
            "push",
            "../hidden.git",
            "main:refs/heads/x",
        ],
        &[
            "-c",
            "protocol.version=0",
            "-c",
            "push.negotiate=1",
            "push",
            "../hidden.git",
            "main:y",
        ],
        &[
            "-c",
            "push.negotiate=1",
            "push",
            "../hidden.git",
            ":refs/heads/x",
        ],
        &[
            "-c",
            "push.negotiate=bogus",
            "push",
            "../hidden.git",
            "main:z",
        ],
    ];
    for argv in cases {
        let path = server_path(&fx);
        fx.compare(*argv)
            .in_dir("client")
            .repo("hidden.git")
            .env("PATH", path)
            .checks(CHECKS)
            .run();
    }
}
