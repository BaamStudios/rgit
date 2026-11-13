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

/// Branches with upstreams: `main` ahead of `origin/main`, `side` behind
/// `origin/other`, `t1` level with `origin/main`, `loc` tracking `main`,
/// `gone` tracking a deleted remote branch, `merged` merged into HEAD,
/// `work` not merged anywhere.
fn tracking() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["commit", "-q", "--allow-empty", "-m", "one"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "two\n\nbody"])
        .tick()
        .git(["config", "remote.origin.url", "/nowhere"])
        .git([
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ])
        .git(["update-ref", "refs/remotes/origin/main", "HEAD~1"])
        .git(["update-ref", "refs/remotes/origin/other", "HEAD"])
        .git(["update-ref", "refs/remotes/origin/gone", "HEAD"])
        .git(["branch", "-q", "-u", "origin/main"])
        .git(["branch", "side", "HEAD~1"])
        .git(["branch", "-q", "-u", "origin/other", "side"])
        .git(["branch", "t1", "HEAD~1"])
        .git(["branch", "-q", "-u", "origin/main", "t1"])
        .git(["branch", "gone", "HEAD"])
        .git(["branch", "-q", "-u", "origin/gone", "gone"])
        .git(["update-ref", "-d", "refs/remotes/origin/gone"])
        .git(["branch", "merged", "HEAD~1"])
        .git(["branch", "loc", "HEAD~1"])
        .git(["branch", "-q", "-u", "main", "loc"])
        .git(["config", "branch.loc.description", "a local branch"])
        .git(["switch", "-q", "-c", "work"])
        .git(["commit", "-q", "--allow-empty", "-m", "three"])
        .tick()
        .git(["switch", "-q", "main"])
}

fn each(make: fn() -> Fixture, cases: &[&[&str]]) {
    let mut failures = Vec::new();
    for args in cases {
        let mut fx = make();
        if fx.is_skipped() {
            return;
        }
        if let Err(m) = fx.compare(*args).try_run() {
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
fn verbose_listing() {
    each(
        tracking,
        &[
            &["branch", "-v"],
            &["branch", "-vv"],
            &["branch", "-v", "--abbrev=10"],
            &["branch", "-vv", "--no-abbrev"],
            &["branch", "-vv", "-a"],
            &["branch", "-v", "-r"],
            &["branch", "-vv", "--list", "s*"],
        ],
    );
    let detached = || tracking().git(["checkout", "-q", "--detach", "HEAD~1"]);
    each(detached, &[&["branch", "-vv"]]);
}

#[test]
fn delete_merged() {
    each(
        tracking,
        &[
            &["branch", "-d", "merged"],
            &["branch", "-d", "t1"],
            &["branch", "-d", "side"],
            &["branch", "-d", "work"],
            &["branch", "-d", "loc"],
            &["branch", "-d", "gone"],
            &["branch", "-d", "main"],
            &["branch", "-d", "nosuch", "merged"],
            &["branch", "-d", "-q", "merged"],
            &["branch", "-D", "work", "loc"],
            &["branch", "-d", "-r", "origin/other"],
            &["branch", "-d", "-r", "origin/nosuch"],
            &["branch", "-d"],
            &[
                "-c",
                "advice.forceDeleteBranch=false",
                "branch",
                "-d",
                "work",
            ],
        ],
    );
}

#[test]
fn rename_and_copy() {
    each(
        tracking,
        &[
            &["branch", "-m", "loc", "renamed"],
            &["branch", "-m", "newmain"],
            &["branch", "-m", "main", "newmain"],
            &["branch", "-m", "loc", "side"],
            &["branch", "-M", "loc", "side"],
            &["branch", "-M", "loc", "main"],
            &["branch", "-m", "loc", "loc"],
            &["branch", "-m", "nosuch", "x"],
            &["branch", "-m", "loc", "a..b"],
            &["branch", "-m", "a", "b", "c"],
            &["branch", "-m"],
            &["branch", "-m", "loc", "deep/er/name"],
            &["branch", "-c", "loc", "copied"],
            &["branch", "-c", "copied"],
            &["branch", "-c", "loc", "side"],
            &["branch", "-C", "loc", "side"],
            &["branch", "-C", "loc", "loc"],
            &["branch", "-c", "a", "b", "c"],
        ],
    );
    let detached = || tracking().git(["checkout", "-q", "--detach"]);
    each(detached, &[&["branch", "-m", "x"], &["branch", "-c", "x"]]);
    let unborn = || tracking().git(["checkout", "-q", "--orphan", "unb"]);
    each(
        unborn,
        &[
            &["branch", "-m", "unb2"],
            &["branch", "-c", "unb2"],
            &["branch", "-m", "side"],
        ],
    );
}

#[test]
fn upstream() {
    each(
        tracking,
        &[
            &["branch", "-u", "origin/other"],
            &["branch", "-u", "origin/other", "merged"],
            &["branch", "--set-upstream-to=main", "merged"],
            &[
                "branch",
                "--set-upstream-to",
                "refs/remotes/origin/main",
                "merged",
            ],
            &["branch", "-q", "-u", "main", "merged"],
            &["branch", "-u", "main"],
            &["branch", "-u", "nosuch"],
            &["branch", "-u", "HEAD~1", "merged"],
            &["branch", "-u", "main", "nosuch"],
            &["branch", "-u", "main", "a", "b"],
            &[
                "-c",
                "branch.autoSetupRebase=always",
                "branch",
                "-u",
                "main",
                "merged",
            ],
            &[
                "-c",
                "branch.autoSetupRebase=remote",
                "branch",
                "-u",
                "origin/main",
                "merged",
            ],
            &["branch", "--unset-upstream"],
            &["branch", "--unset-upstream", "side"],
            &["branch", "--unset-upstream", "merged"],
            &["branch", "--unset-upstream", "loc"],
            &["branch", "--unset-upstream", "a", "b"],
        ],
    );
    let detached = || tracking().git(["checkout", "-q", "--detach"]);
    each(
        detached,
        &[&["branch", "-u", "main"], &["branch", "--unset-upstream"]],
    );
    let unborn = || tracking().git(["checkout", "-q", "--orphan", "unb"]);
    each(unborn, &[&["branch", "-u", "main"]]);
}
