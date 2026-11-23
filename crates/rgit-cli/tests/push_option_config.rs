// SPDX-License-Identifier: MIT
//! Differential tests of `push.pushOption` (default push options, #165).

use rgit_difftest::{Check, Fixture};

mod common;
use common::servers::server_path;

const CHECKS: [Check; 4] = [Check::ExitCode, Check::Stdout, Check::Stderr, Check::Refs];

/// Prints the push options the server received.
const HOOK: &str = "#!/bin/sh\ni=0\nwhile test $i -lt ${GIT_PUSH_OPTION_COUNT:-0}\ndo\n\
\teval echo opt=\\$GIT_PUSH_OPTION_$i\n\ti=$((i+1))\ndone\n";

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "--bare", "up.git"])
        .git([
            "-C",
            "up.git",
            "config",
            "receive.advertisePushOptions",
            "true",
        ])
        .write_executable("up.git/hooks/pre-receive", HOOK)
        .git(["init", "-q", "--bare", "plain.git"])
        .git(["init", "-q", "-b", "main", "w"])
        .git(["-C", "w", "commit", "-q", "--allow-empty", "-m", "one"])
}

#[test]
fn push_option_config_same_as_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["-c", "push.pushOption", "push", "../up.git", "main"],
        &[
            "-c",
            "push.pushOption=a",
            "-c",
            "push.pushOption=b",
            "push",
            "../up.git",
            "main:x1",
        ],
        &[
            "-c",
            "push.pushOption=a",
            "-c",
            "push.pushOption=",
            "-c",
            "push.pushOption=b",
            "push",
            "../up.git",
            "main:x2",
        ],
        &[
            "-c",
            "push.pushOption=a",
            "push",
            "-o",
            "m",
            "../up.git",
            "main:x3",
        ],
        &[
            "-c",
            "push.pushOption=a",
            "push",
            "--no-push-option",
            "../up.git",
            "main:x4",
        ],
        &["-c", "push.pushOption=a", "push", "../plain.git", "main"],
        &["push", "--atomic", "-o", "x", "../plain.git", "main"],
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
}
