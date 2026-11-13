// SPDX-License-Identifier: MIT
//! Differential tests of `rgit branch` against the reference git: output,
//! exit code and the resulting refs and reflogs.

use rgit_difftest::{Check, Fixture};

const CHECKS: [Check; 4] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::GitFiles,
];

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["commit", "-q", "--allow-empty", "-m", "one"])
        .git(["update-ref", "refs/remotes/origin/main", "HEAD"])
        .git([
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ])
        .git(["branch", "feature"])
}

#[track_caller]
fn all_same(fx: &mut Fixture, cases: &[&[&str]]) {
    let mut failures = Vec::new();
    for args in cases {
        if let Err(m) = fx.compare(*args).checks(CHECKS).try_run() {
            failures.push(m.to_string());
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn list() {
    let mut fx = fixture();
    all_same(
        &mut fx,
        &[
            &["branch"],
            &["branch", "-a"],
            &["branch", "-r"],
            &["branch", "--list", "f*"],
            &["branch", "-l", "nomatch*"],
            &["branch", "--show-current"],
        ],
    );
}

#[test]
fn create() {
    let mut fx = fixture();
    all_same(
        &mut fx,
        &[
            &["branch", "topic"],
            &["branch", "topic", "main"],
            &["branch", "feature"],
            &["branch", "a..b"],
            &["branch", "x", "nosuch"],
        ],
    );
}

#[test]
fn force_delete() {
    let mut fx = fixture();
    all_same(
        &mut fx,
        &[
            &["branch", "-D", "feature"],
            &["branch", "-D", "nosuch"],
            &["branch", "-D", "main"],
        ],
    );
}

#[test]
fn detached() {
    let mut fx = fixture().git(["checkout", "-q", "--detach"]);
    all_same(&mut fx, &[&["branch"], &["branch", "--show-current"]]);
}
