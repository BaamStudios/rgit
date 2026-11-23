// SPDX-License-Identifier: MIT
//! Differential tests of `push --signed[=<value>]`, `push.gpgSign` and
//! `send-pack --signed` (#165): the push certificate the server's hooks
//! see, the errors without server support or without a signature.

use rgit_difftest::{Check, Fixture, Normalizer};

mod common;
use common::servers::server_path;

const CHECKS: [Check; 4] = [Check::ExitCode, Check::Stdout, Check::Stderr, Check::Refs];

/// A signing program that accepts any key and prints a fixed signature;
/// `bad-gpg` fails.
const FAKE_GPG: &str = "#!/bin/sh\ncat >/dev/null\necho '[GNUPG:] SIG_CREATED ' >&2\n\
printf -- '-----BEGIN PGP SIGNATURE-----\\n\\nabc\\n-----END PGP SIGNATURE-----\\n'\n";
const BAD_GPG: &str = "#!/bin/sh\ncat >/dev/null\necho 'gpg: no key' >&2\nexit 2\n";

/// Shows the certificate without its time-dependent nonce.
const HOOK: &str = "#!/bin/sh\ncat >/dev/null\n\
if test -n \"$GIT_PUSH_CERT\"\nthen\n\
\techo \"nonce: $GIT_PUSH_CERT_NONCE_STATUS\"\n\
\tgit cat-file blob $GIT_PUSH_CERT | sed -e '/^nonce /d'\n\
else\n\techo 'no certificate'\nfi\n";

fn fixture() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .write_executable("fake-gpg", FAKE_GPG)
        .write_executable("bad-gpg", BAD_GPG)
        .git(["init", "-q", "--bare", "nosup.git"])
        .git(["init", "-q", "--bare", "r.git"])
        .git(["-C", "r.git", "config", "receive.certNonceSeed", "sekrit"])
        .git([
            "-C",
            "r.git",
            "config",
            "receive.advertisePushOptions",
            "true",
        ])
        .write_executable("r.git/hooks/post-receive", HOOK)
        .git(["init", "-q", "-b", "main", "w"])
        .git(["-C", "w", "commit", "-q", "--allow-empty", "-m", "one"])
        .git(["-C", "w", "push", "-q", "../r.git", "main", "main:gone"])
        .git(["-C", "w", "push", "-q", "../nosup.git", "main"])
        .tick()
        .git(["-C", "w", "commit", "-q", "--allow-empty", "-m", "two"]);
    let gpg = fx.work_dir().join("fake-gpg");
    fx.git([
        "-C",
        "w",
        "config",
        "gpg.program",
        gpg.to_str().expect("utf-8 path"),
    ])
}

#[test]
fn push_signed_same_as_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let bad = format!("gpg.program={}", fx.work_dir().join("bad-gpg").display());
    let cases: Vec<Vec<&str>> = vec![
        vec!["push", "--signed", "../r.git", "main", ":gone"],
        vec![
            "push", "--signed", "-o", "a", "-o", "b c", "../r.git", "main",
        ],
        vec!["push", "--signed=if-asked", "../r.git", "main:x"],
        vec!["push", "--signed", "--porcelain", "../r.git", "main:y"],
        vec!["push", "--signed", "../r.git", "main~1:refs/heads/main"],
        vec!["push", "--signed", "../nosup.git", "main"],
        vec!["push", "--signed=1", "../nosup.git", "main"],
        vec!["push", "--signed=if-asked", "../nosup.git", "main"],
        vec!["push", "--signed=no", "../nosup.git", "main"],
        vec!["push", "--signed=bogus", "../nosup.git", "main"],
        vec!["push", "--signed", "-n", "../nosup.git", "main"],
        vec!["push", "--signed", "../nosup.git", "nonexistent"],
        vec!["-c", "push.gpgSign=true", "push", "../nosup.git", "main"],
        vec![
            "-c",
            "push.gpgSign=if-asked",
            "push",
            "../nosup.git",
            "main",
        ],
        vec![
            "-c",
            "push.gpgSign=true",
            "push",
            "--no-signed",
            "../nosup.git",
            "main",
        ],
        vec!["-c", "push.gpgSign=bogus", "push", "../nosup.git", "main"],
        vec!["-c", "push.gpgSign=true", "push", "../r.git", "main:z"],
        vec!["-c", &bad, "push", "--signed", "../r.git", "main"],
        vec![
            "-c",
            &bad,
            "push",
            "--signed",
            "--dry-run",
            "../r.git",
            "main",
        ],
        vec!["send-pack", "--signed", "../r.git", "main"],
        vec!["send-pack", "--signed", "../nosup.git", "main"],
        vec![
            "-c",
            "push.gpgSign=true",
            "send-pack",
            "../nosup.git",
            "main",
        ],
        vec![
            "-c",
            "push.gpgSign=bogus",
            "send-pack",
            "../nosup.git",
            "main",
        ],
        vec!["send-pack", "--signed=bogus", "../r.git", "main"],
    ];
    for argv in &cases {
        let path = server_path(&fx);
        fx.compare(argv)
            .in_dir("w")
            .repo("r.git")
            .env("PATH", path)
            // The pusher line carries the current time, not the fixture's.
            .normalize_stderr(Normalizer::regex(
                r"(?m)^(remote: pusher .*>) \d+ [+-]\d{4}",
                "$1 <time>",
            ))
            .checks(CHECKS)
            .run();
    }
}
