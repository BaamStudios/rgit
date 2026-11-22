// SPDX-License-Identifier: MIT
//! Automatic maintenance after `commit`, `merge`, `fetch`, `pull`,
//! `rebase` and `am` (its `reflog-expire` task) and `reflog expire`,
//! against the reference git: which reflog entries go, and when.
//!
//! The fixture dates lie years back, so every entry is older than git's
//! default expiry; the auto task runs once the `HEAD` reflog has 100 such
//! entries.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use rgit_difftest::Fixture;

/// `main`: c0 c1; `side`: c0 s; a stash; `HEAD` reflog with `switches`
/// branch switches on top. Maintenance runs in the foreground.
fn fixture(switches: usize) -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["config", "maintenance.autoDetach", "false"])
        .write("a", "a\n")
        .git(["add", "a"])
        .git(["commit", "-q", "-m", "c0"])
        .tick()
        .git(["checkout", "-q", "-b", "side"])
        .write("s", "s\n")
        .git(["add", "s"])
        .git(["commit", "-q", "-m", "s"])
        .tick()
        .git(["checkout", "-q", "main"])
        .write("b", "b\n")
        .git(["add", "b"])
        .git(["commit", "-q", "-m", "c1"])
        .tick()
        .write("a", "stashed\n")
        .git(["stash", "-q"])
        .tick();
    for _ in 0..switches {
        fx = fx
            .git(["checkout", "-q", "side"])
            .git(["checkout", "-q", "main"]);
    }
    fx.tick()
}

/// `PATH` with the directory of the reference git first, so that a fetch
/// from `.` finds its `git-upload-pack`.
fn server_path(fx: &Fixture) -> OsString {
    let mut dirs: Vec<PathBuf> = fx
        .reference_git()
        .parent()
        .into_iter()
        .map(Path::to_path_buf)
        .collect();
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(dirs).unwrap()
}

/// Runs each case in a fresh fixture prepared by `prep`; reports every
/// differing case.
fn cases(switches: usize, prep: impl Fn(Fixture) -> Fixture, cases: &[&[&str]]) {
    let mut failed = Vec::new();
    for args in cases {
        let fx = prep(fixture(switches));
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        let path = server_path(&fx);
        if let Err(m) = fx.compare(*args).env("PATH", path).try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

/// 102 old `HEAD` entries: the commands that run maintenance expire every
/// reflog but the stash; the others leave them alone.
#[test]
fn auto_reflog_expire() {
    cases(
        48,
        |fx| fx,
        &[
            &["commit", "-q", "--allow-empty", "-m", "x"],
            &["commit", "--allow-empty", "-m", "x"],
            &["commit", "-q", "--allow-empty", "--dry-run", "-m", "x"],
            &["merge", "-q", "side"],
            &["merge", "-q", "--no-commit", "side"],
            &["merge", "--squash", "side"],
            &["merge", "-q", "main~1"],
            &["fetch", "-q", ".", "side:copy"],
            &["fetch", "-q", "--no-auto-maintenance", ".", "side:copy"],
            &["pull", "-q", "--no-rebase", ".", "side"],
            &["pull", "-q", "--rebase", ".", "side"],
            &["rebase", "-q", "side"],
            &["rebase", "-q", "main~1"],
            &["cherry-pick", "side"],
            &["reset", "-q", "--hard", "HEAD~1"],
            &["checkout", "-q", "side"],
        ],
    );
    // A rebase stopped by a conflict: --abort runs maintenance, the
    // commit that resolves it as well.
    let stopped = |fx: Fixture| {
        fx.git(["checkout", "-q", "-b", "conf", "main~1"])
            .write("b", "conflict\n")
            .git(["add", "b"])
            .git(["commit", "-q", "-m", "conf"])
    };
    let stopped = |fx: Fixture| {
        let fx = stopped(fx);
        let _ = fx.git_output(["rebase", "-q", "main"]);
        fx
    };
    cases(48, stopped, &[&["rebase", "--abort"], &["status"]]);
    cases(
        48,
        |fx| fx.git(["format-patch", "-q", "-1", "side", "-o", "p"]),
        &[&["am", "-q", "p/0001-s.patch"]],
    );
    cases(
        48,
        |fx| stopped(fx).write("b", "resolved\n").git(["add", "b"]),
        &[&["rebase", "--continue"]],
    );
}

/// The settings that switch the task off or change its threshold, and the
/// count of old entries.
#[test]
fn auto_reflog_expire_settings() {
    let commit: &[&str] = &["commit", "-q", "--allow-empty", "-m", "x"];
    let with = |config: &[&'static str]| {
        let mut v: Vec<&'static str> = Vec::new();
        for c in config {
            v.push("-c");
            v.push(c);
        }
        v.extend_from_slice(&["commit", "-q", "--allow-empty", "-m", "x"]);
        v
    };
    let configs: Vec<Vec<&str>> = vec![
        with(&["maintenance.auto=false"]),
        with(&["maintenance.strategy=gc"]),
        with(&["maintenance.strategy=incremental"]),
        with(&["maintenance.strategy=geometric"]),
        with(&["maintenance.strategy=bogus"]),
        with(&["maintenance.reflog-expire.enabled=false"]),
        with(&[
            "maintenance.strategy=gc",
            "maintenance.reflog-expire.enabled=true",
        ]),
        with(&["maintenance.reflog-expire.auto=0"]),
        with(&["maintenance.reflog-expire.auto=500"]),
        with(&["gc.reflogExpire=never", "gc.reflogExpireUnreachable=never"]),
        with(&["gc.reflogExpire=never"]),
        with(&[
            "gc.HEAD.reflogExpire=never",
            "gc.HEAD.reflogExpireUnreachable=never",
        ]),
        with(&["gc.auto=0"]),
    ];
    let refs: Vec<&[&str]> = configs.iter().map(Vec::as_slice).collect();
    cases(48, |fx| fx, &refs);
    // 98 old entries plus the commit's own are not enough, 100 plus one
    // are.
    cases(46, |fx| fx, &[commit]);
    cases(47, |fx| fx, &[commit]);
    // Below the threshold nothing happens, unless it is negative.
    cases(
        40,
        |fx| fx,
        &[commit, &with(&["maintenance.reflog-expire.auto=-1"])],
    );
    // A held lock skips the maintenance.
    cases(
        48,
        |fx| fx.write(".git/objects/maintenance.lock", ""),
        &[commit],
    );
}

/// `reflog expire`: limits from the options and the configuration,
/// reachability, `--dry-run`, `--verbose`, `--rewrite`, the stash, errors.
#[test]
fn reflog_expire() {
    // Entries from 1700000000 on, one minute apart; an amended commit
    // makes an unreachable entry.
    let amended = |fx: Fixture| {
        fx.git(["commit", "-q", "--allow-empty", "-m", "a"])
            .tick()
            .git(["commit", "-q", "--amend", "--allow-empty", "-m", "b"])
            .tick()
            .write("a", "stashed again\n")
            .git(["stash", "-q"])
            .tick()
            .git(["commit", "-q", "--allow-empty", "-m", "c"])
    };
    let mid = "--expire=2023-11-14 22:15:30 +0000";
    let mid_unreachable = "--expire-unreachable=2023-11-14 22:15:30 +0000";
    cases(
        0,
        amended,
        &[
            &["reflog", "expire", "--all"],
            &["reflog", "expire", "--all", "--verbose"],
            &["reflog", "expire", "--all", "--dry-run", "--verbose"],
            &["reflog", "expire", "-n", "--verbose", "main"],
            &["reflog", "expire", "--verbose", "main", "nosuch"],
            &["reflog", "expire", "--verbose", "heads/main", "@"],
            &["reflog", "expire", "--verbose", "HEAD@{1}"],
            &["reflog", "expire", "--verbose", "--all", "main"],
            &["reflog", "expire", "--verbose", "--all", mid],
            &["reflog", "expire", "--verbose", "--all", "--rewrite", mid],
            &[
                "reflog",
                "expire",
                "--verbose",
                "--all",
                "--expire=never",
                mid_unreachable,
            ],
            &["reflog", "expire", "--verbose", "--all", "--expire=never"],
            &[
                "reflog",
                "expire",
                "--verbose",
                "--expire=now",
                "refs/stash",
            ],
            &[
                "reflog",
                "expire",
                "--verbose",
                "--expire-unreachable=now",
                "--expire=never",
                "--all",
            ],
            &["reflog", "expire", "--verbose", "--expire=all", "--all"],
            &["reflog", "expire", "--expire=garbage", "--all"],
            &["reflog", "expire"],
            &["reflog", "expire", "-h"],
            &["-c", "gc.reflogExpire=garbage", "reflog", "expire", "--all"],
            &[
                "-c",
                "gc.reflogExpire=never",
                "reflog",
                "expire",
                "--verbose",
                "--all",
            ],
            &[
                "-c",
                "gc.refs/heads/*.reflogExpire=never",
                "reflog",
                "expire",
                "--verbose",
                "--all",
            ],
            &[
                "-c",
                "gc.refs/stash.reflogExpire=now",
                "reflog",
                "expire",
                "--verbose",
                "--all",
            ],
            &[
                "-c",
                "gc.reflogExpire=now",
                "reflog",
                "expire",
                "--verbose",
                "refs/stash",
            ],
        ],
    );
}
