// SPDX-License-Identifier: MIT
//! Differential tests of `fetch-pack` (#581): named refs, `HEAD`, `--all`,
//! `--stdin`, raw object ids, missing refs, `-v`, `-k` (the kept pack
//! with its `.keep` file), `--depth`, protocol v0 and v2, `--diag-url` and
//! the usage.

use rgit_difftest::{Check, Fixture};

mod common;
use common::servers::server_path;

const CHECKS: [Check; 4] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Objects,
];

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "srv"])
        .git(["-C", "srv", "commit", "-q", "--allow-empty", "-m", "one"])
        .git(["-C", "srv", "branch", "other"])
        .git(["-C", "srv", "commit", "-q", "--allow-empty", "-m", "two"])
        .git(["-C", "srv", "tag", "-a", "-m", "t", "v1", "HEAD"])
        .git(["-C", "srv", "commit", "-q", "--allow-empty", "-m", "three"])
        .git(["init", "-q", "cl"])
        .tick()
}

#[test]
fn fetch_pack_same_as_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let path = server_path(&fx);
    let cases: &[&[&str]] = &[
        &["fetch-pack", "--no-progress", "../srv", "refs/heads/main"],
        &[
            "fetch-pack",
            "--no-progress",
            "../srv",
            "HEAD",
            "refs/tags/v1",
        ],
        &["fetch-pack", "--no-progress", "--all", "../srv"],
        &[
            "fetch-pack",
            "--no-progress",
            "../srv",
            "main",
            "refs/heads/other",
        ],
        &[
            "fetch-pack",
            "--no-progress",
            "../srv",
            "refs/heads/nope",
            "refs/heads/zz",
        ],
        &["fetch-pack", "--no-progress", "../srv"],
        &[
            "fetch-pack",
            "--no-progress",
            "-v",
            "../srv",
            "refs/heads/main",
        ],
        &[
            "fetch-pack",
            "--no-progress",
            "--depth=1",
            "../srv",
            "refs/heads/main",
        ],
        &[
            "fetch-pack",
            "--no-progress",
            "--include-tag",
            "../srv",
            "refs/heads/main",
        ],
        &["fetch-pack", "--no-progress", "../srv", "--all"],
        &["fetch-pack", "--bogus", "../srv"],
        &["fetch-pack"],
        &["fetch-pack", "-h"],
        &["fetch-pack", "--diag-url", "user@host:~u/repo"],
        &["fetch-pack", "--diag-url", "ssh://[::1]:22/x"],
        &["fetch-pack", "--diag-url", "git://h/~u/r"],
        &["fetch-pack", "--diag-url", "foo://h/r"],
    ];
    for argv in cases {
        for version in ["0", "2"] {
            fx.compare(*argv)
                .in_dir("cl")
                .repo("cl")
                .env("PATH", &path)
                .env("GIT_TEST_PROTOCOL_VERSION", version)
                .checks(CHECKS)
                .run();
        }
    }
    // Refs from standard input, duplicates dropped.
    fx.compare(["fetch-pack", "--no-progress", "--stdin", "../srv", "HEAD"])
        .in_dir("cl")
        .repo("cl")
        .env("PATH", &path)
        .stdin("refs/heads/main\nrefs/heads/other\nrefs/heads/main\n")
        .checks(CHECKS)
        .run();
}

#[test]
fn fetch_pack_keep_same_as_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let path = server_path(&fx);
    // The kept pack is reported (`keep\t<checksum>`) and keeps its
    // `.keep` file; the checksum depends on the server only.
    for argv in [
        &[
            "fetch-pack",
            "--no-progress",
            "-k",
            "../srv",
            "refs/heads/other",
        ][..],
        &[
            "fetch-pack",
            "--no-progress",
            "-k",
            "-k",
            "../srv",
            "refs/heads/other",
        ],
        &[
            "fetch-pack",
            "--no-progress",
            "-k",
            "--check-self-contained-and-connected",
            "../srv",
            "refs/heads/other",
        ],
    ] {
        fx.compare(argv)
            .in_dir("cl")
            .repo("cl")
            .env("PATH", &path)
            .checks(CHECKS)
            .run();
    }
}
