// SPDX-License-Identifier: MIT
//! `reset --soft` against the reference git: refs, `ORIG_HEAD`, reflogs,
//! errors and exit codes.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("f", "1\n")
        .git(["add", "f"])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .write("f", "2\n")
        .git(["add", "f"])
        .git(["commit", "-q", "-m", "two"])
        .tick()
}

#[test]
fn soft() {
    let cases: &[&[&str]] = &[
        &["reset", "--soft", "HEAD~1"],
        &["reset", "--soft"],
        &["reset", "-q", "--soft", "HEAD"],
        &["reset", "--soft", "HEAD", "--"],
        &["reset", "--soft", "HEAD", "--", "f"],
        &["reset", "--soft", "nope"],
        &["reset", "--soft", "HEAD^{tree}"],
    ];
    for args in cases {
        let mut fx = fixture();
        if fx.is_skipped() {
            return;
        }
        fx.compare(*args)
            .checks([
                Check::ExitCode,
                Check::Stdout,
                Check::Stderr,
                Check::Refs,
                Check::Reflogs,
                Check::Index,
            ])
            .run();
    }
}
