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

/// Remote-tracking refs, a tag and branches with and without upstream
/// configuration, for creating branches with tracking.
fn upstreams() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["commit", "-q", "--allow-empty", "-m", "one"])
        .git(["config", "remote.origin.url", "/nowhere"])
        .git([
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ])
        .git(["update-ref", "refs/remotes/origin/main", "HEAD"])
        .git(["update-ref", "refs/remotes/origin/x", "HEAD"])
        .git(["update-ref", "refs/remotes/origin/y", "HEAD"])
        .git([
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ])
        .git(["update-ref", "refs/remotes/foo/bar", "HEAD"])
        .git(["tag", "v1"])
        .git(["branch", "-q", "-u", "origin/main"])
        .git(["branch", "loc"])
        .git(["branch", "-q", "-u", "main", "loc"])
        .git(["branch", "plain"])
        .git(["branch", "multi"])
        .git(["config", "branch.multi.remote", "origin"])
        .git(["config", "branch.multi.merge", "refs/heads/x"])
        .git(["config", "--add", "branch.multi.merge", "refs/heads/y"])
        .git(["branch", "nomerge"])
        .git(["config", "branch.nomerge.remote", "origin"])
}

#[test]
fn create_with_tracking() {
    each(
        upstreams,
        &[
            &["branch", "a", "origin/x"],
            &["branch", "a", "main"],
            &["branch", "-t", "a", "main"],
            &["branch", "-t", "a"],
            &["branch", "-t", "a", "HEAD"],
            &["branch", "-t", "a", "HEAD^0"],
            &["branch", "-t", "a", "origin/HEAD"],
            &["branch", "-t", "a", "refs/remotes/origin/x"],
            &["branch", "-q", "a", "origin/y"],
            &["branch", "--track=direct", "a", "origin/y"],
            &["branch", "--no-track", "a", "origin/x"],
            &["branch", "-t", "--no-track", "a", "origin/x"],
            &["branch", "--no-track", "-t", "a", "origin/x"],
            &["branch", "--track=bogus", "a"],
            &["branch", "-t", "a", "v1"],
            &["branch", "-t", "a", "origin/x^0"],
            &["branch", "a", "origin/x^0"],
            &["branch", "-t", "a", "nosuch"],
            &[
                "-c",
                "advice.setUpstreamFailure=false",
                "branch",
                "-t",
                "a",
                "nosuch",
            ],
            &["branch", "-t", "a", "foo/bar"],
            &["branch", "a", "foo/bar"],
            &["branch", "-t"],
        ],
    );
}

#[test]
fn create_inherit() {
    each(
        upstreams,
        &[
            &["branch", "--track=inherit", "a", "loc"],
            &["branch", "--track=inherit", "a", "main"],
            &["branch", "--track=inherit", "a", "multi"],
            &["branch", "--track=inherit", "a", "plain"],
            &["branch", "--track=inherit", "a", "nomerge"],
            &["branch", "--track=inherit", "a", "origin/x"],
            &["branch", "--track=inherit", "a", "foo/bar"],
            &["branch", "--track=inherit", "a", "HEAD^0"],
            &["branch", "--track=inherit", "a", "v1"],
            &["branch", "--track=inherit", "a", "nosuch"],
            &[
                "-c",
                "branch.autoSetupRebase=always",
                "branch",
                "--track=inherit",
                "a",
                "multi",
            ],
            &[
                "-c",
                "branch.autoSetupRebase=remote",
                "branch",
                "--track=inherit",
                "a",
                "loc",
            ],
            &[
                "-c",
                "branch.autoSetupRebase=local",
                "branch",
                "--track=inherit",
                "a",
                "loc",
            ],
        ],
    );
}

#[test]
fn auto_setup_merge_and_rebase() {
    let mut cases: Vec<Vec<String>> = Vec::new();
    for merge in [
        "false", "true", "yes", "always", "inherit", "simple", "bogus",
    ] {
        for args in [
            &["a", "origin/x"][..],
            &["x", "origin/x"],
            &["a", "main"],
            &["a", "loc"],
            &["a", "v1"],
            &["a", "HEAD^0"],
            &["a", "nosuch"],
            &["-t", "a", "main"],
        ] {
            let mut v = vec![
                "-c".to_owned(),
                format!("branch.autoSetupMerge={merge}"),
                "branch".to_owned(),
            ];
            v.extend(args.iter().map(|s| s.to_string()));
            cases.push(v);
        }
    }
    for rebase in ["never", "local", "remote", "always"] {
        for args in [
            &["a", "origin/x"][..],
            &["-t", "a", "main"],
            &["-q", "a", "origin/y"],
        ] {
            let mut v = vec![
                "-c".to_owned(),
                format!("branch.autoSetupRebase={rebase}"),
                "branch".to_owned(),
            ];
            v.extend(args.iter().map(|s| s.to_string()));
            cases.push(v);
        }
    }
    let refs: Vec<Vec<&str>> = cases
        .iter()
        .map(|c| c.iter().map(String::as_str).collect())
        .collect();
    let refs: Vec<&[&str]> = refs.iter().map(Vec::as_slice).collect();
    each(upstreams, &refs);

    // Invalid values in the configuration file end every branch command.
    let bad_rebase = || upstreams().git(["config", "branch.autoSetupRebase", "bogus"]);
    each(bad_rebase, &[&["branch"], &["branch", "-t", "a", "main"]]);
    let bad_merge = || upstreams().git(["config", "branch.autoSetupMerge", "bogus"]);
    each(bad_merge, &[&["branch"], &["branch", "--no-track", "a"]]);
}

#[test]
fn tracking_config_edits() {
    // Existing values: a single merge is replaced, several are kept (with
    // a warning), other keys stay where they are.
    let stale = || {
        upstreams()
            .git(["config", "branch.z.merge", "refs/heads/foo"])
            .git(["config", "branch.z.rebase", "false"])
            .git(["config", "branch.w.remote", "foo"])
            .git(["config", "branch.w.merge", "refs/heads/foo"])
            .git(["config", "branch.w.rebase", "false"])
            .git(["config", "--add", "branch.w.merge", "refs/heads/bar"])
            .git(["config", "--add", "branch.w.remote", "bar"])
    };
    each(
        stale,
        &[
            &["branch", "z", "origin/x"],
            &["branch", "w", "origin/x"],
            &[
                "-c",
                "branch.autoSetupRebase=always",
                "branch",
                "w",
                "origin/x",
            ],
            &["branch", "-u", "origin/x", "multi"],
        ],
    );
}

#[test]
fn tracking_ambiguous() {
    // Two remotes map into the same namespace: the branch is created, the
    // tracking setup fails.
    let two = || {
        upstreams()
            .git(["config", "remote.two.url", "/nowhere"])
            .git([
                "config",
                "remote.two.fetch",
                "+refs/heads/*:refs/remotes/origin/*",
            ])
    };
    each(
        two,
        &[
            &["branch", "a", "origin/x"],
            &["branch", "-t", "a", "origin/HEAD"],
            &["branch", "--no-track", "a", "origin/x"],
            &["branch", "-u", "origin/x", "plain"],
        ],
    );
    // A remote with an unusual refspec.
    let tags = || {
        upstreams()
            .git(["config", "remote.t.url", "/nowhere"])
            .git(["config", "remote.t.fetch", "+refs/tags/*:refs/remotes/t/*"])
            .git(["update-ref", "refs/remotes/t/v1", "HEAD"])
    };
    each(tags, &[&["branch", "-t", "a", "t/v1"]]);
    // A tag and a branch of the same name.
    let tag_main = || upstreams().git(["tag", "main"]);
    each(
        tag_main,
        &[&["branch", "a", "main"], &["branch", "-t", "a", "main"]],
    );
}

#[test]
fn tracking_detached_and_unborn() {
    let detached = || upstreams().git(["checkout", "-q", "--detach"]);
    each(
        detached,
        &[
            &["branch", "a"],
            &["branch", "-t", "a"],
            &["branch", "--track=inherit", "a"],
        ],
    );
    let unborn = || upstreams().git(["checkout", "-q", "--orphan", "unb"]);
    each(
        unborn,
        &[
            &["branch", "a"],
            &["branch", "-t", "a"],
            &["branch", "--track=inherit", "a"],
        ],
    );
}

/// `-f`/`--force`: resetting an existing branch, refused for the
/// checked-out one; also for `-d`, `-m` and `-c`.
#[test]
fn force() {
    each(
        tracking,
        &[
            &["branch", "-f", "side"],
            &["branch", "-f", "side", "HEAD~1"],
            &["branch", "--force", "merged", "work"],
            &["branch", "-f", "main", "HEAD~1"],
            &["branch", "-f", "new", "HEAD~1"],
            &["branch", "-f", "-t", "merged", "main"],
            &["branch", "-f", "--no-track", "side", "origin/main"],
            &["branch", "-f", "side", "nope"],
            &["branch", "--no-force", "side"],
            &["branch", "-d", "-f", "work"],
            &["branch", "-m", "-f", "merged", "side"],
            &["branch", "-c", "-f", "merged", "side"],
            &["branch", "-f"],
        ],
    );
    let detached = || tracking().git(["checkout", "-q", "--detach"]);
    each(detached, &[&["branch", "-f", "main", "HEAD~1"]]);
}

/// `-d` with `@{-<n>}` and `[<branch>]@{upstream}`.
#[test]
fn delete_interpreted_names() {
    each(
        || tracking().git(["switch", "-q", "loc"]),
        &[
            &["branch", "-d", "@{u}"],
            &["branch", "-D", "@{upstream}"],
            &["branch", "-d", "@{-1}"],
            &["branch", "-d", "loc@{u}"],
            &["branch", "-d", "nope@{u}"],
            &["branch", "-d", "merged@{u}"],
            &["branch", "-d", "-r", "side@{u}"],
        ],
    );
    each(
        || {
            tracking()
                .git(["checkout", "-q", "-t", "-b", "x", "merged"])
                .git(["branch", "-D", "merged"])
        },
        &[&["branch", "-D", "@{u}"]],
    );
}
