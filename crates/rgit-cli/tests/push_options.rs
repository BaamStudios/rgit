// SPDX-License-Identifier: MIT
//! Differential tests of `push --prune`, `--follow-tags` /
//! `push.followTags` and `--delete` forms (#165, #318).

use rgit_difftest::{Check, Fixture};

mod common;
use common::servers::server_path;

const CHECKS: [Check; 4] = [Check::ExitCode, Check::Stdout, Check::Stderr, Check::Refs];

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "--bare", "r.git"])
        .git(["init", "-q", "-b", "main", "w"])
        .git(["-C", "w", "commit", "-q", "--allow-empty", "-m", "one"])
        .git(["-C", "w", "tag", "-a", "-m", "t", "v1"])
        .git(["-C", "w", "tag", "light"])
        .git(["-C", "w", "remote", "add", "origin", "../r.git"])
        .git([
            "-C",
            "w",
            "push",
            "-q",
            "origin",
            "main",
            "main:refs/heads/bar",
            "light",
        ])
        .tick()
        .git(["-C", "w", "commit", "-q", "--allow-empty", "-m", "two"])
        .git(["-C", "w", "tag", "-a", "-m", "t2", "v2"])
        .git(["-C", "w", "tag", "-a", "-m", "t3", "v3", "HEAD~1"])
        .git(["-C", "w", "branch", "zz"])
}

#[test]
fn push_prune_follow_tags_delete_same_as_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &[
            "push",
            "--dry-run",
            "--prune",
            "origin",
            "refs/heads/*:refs/heads/*",
        ],
        &["push", "--prune", "origin", "refs/heads/*:refs/heads/*"],
        &["push", "--follow-tags", "origin", "main"],
        &["-c", "push.followTags=true", "push", "origin", "main"],
        &[
            "-c",
            "push.followTags=true",
            "push",
            "--no-follow-tags",
            "origin",
            "zz",
        ],
        &["push", "origin", "--delete", "tag", "light"],
        &["push", "origin", "--delete", ""],
        &["push", "origin", "--delete", "main:foo"],
        &["push", "--mirror", "origin", "main"],
        &["push", "--mirror", "--all", "origin"],
        &["push", "--mirror", "--tags", "origin"],
        &["push", "--mirror", "origin"],
        &["-c", "remote.origin.mirror=true", "push", "origin"],
        &["-c", "remote.origin.mirror=true", "push", "origin", "main"],
    ];
    for argv in cases {
        let path = server_path(&fx);
        fx.compare(*argv)
            .in_dir("w")
            .repo("r.git")
            .env("PATH", path)
            .checks(CHECKS)
            .run();
    }
}
