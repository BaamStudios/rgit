// SPDX-License-Identifier: MIT
//! Differential tests of `send-pack` (#165): named refs, matching
//! branches, `--all`, `--mirror`, `--force`, `+<ref>`, deletions,
//! `--dry-run`, and refs that do not match. (Several updates of one ref are
//! checked by the client in git 2.56.0 and by the server in 2.56.1, t5408.)

use rgit_difftest::{Check, Fixture};

mod common;
use common::servers::server_path;

const CHECKS: [Check; 4] = [Check::ExitCode, Check::Stdout, Check::Stderr, Check::Refs];

#[test]
fn send_pack_same_as_git() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "--bare", "up.git"])
        .git(["init", "-q", "-b", "main", "w"])
        .git(["-C", "w", "commit", "-q", "--allow-empty", "-m", "one"])
        .git(["-C", "w", "branch", "side"])
        .git(["-C", "w", "tag", "v1"])
        .tick();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["send-pack", "../up.git", "main"],
        &["send-pack", "../up.git"],
        &["send-pack", "--dry-run", "../up.git", "--all"],
        &["send-pack", "-v", "../up.git", "--all"],
        &["send-pack", "../up.git", "refs/tags/v1"],
        &["send-pack", "../up.git", "nosuch"],
        &["send-pack", "../up.git", "main:refs/heads/x", "side:y"],
        &["send-pack", "../up.git", ":x"],
        &["send-pack", "--mirror", "../up.git"],
        &["send-pack", "--all", "../up.git", "main"],
        &["send-pack"],
    ];
    for argv in cases {
        let path = server_path(&fx);
        fx.compare(*argv)
            .in_dir("w")
            .repo("up.git")
            .env("PATH", path)
            .checks(CHECKS)
            .run();
    }
    // Not a fast-forward any more.
    fx = fx
        .git([
            "-C",
            "w",
            "commit",
            "-q",
            "--amend",
            "--allow-empty",
            "-m",
            "two",
        ])
        .tick();
    for argv in [
        &["send-pack", "../up.git", "main"][..],
        &["send-pack", "../up.git", "+main:y"],
        &["send-pack", "--force", "../up.git", "main"],
    ] {
        let path = server_path(&fx);
        fx.compare(argv)
            .in_dir("w")
            .repo("up.git")
            .env("PATH", path)
            .checks(CHECKS)
            .run();
    }
}
