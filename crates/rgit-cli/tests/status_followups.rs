// SPDX-License-Identifier: MIT
//! Differential tests of the `status` follow-ups: `color.status.*`,
//! `status.compareBranches` (also in `checkout` and `commit --dry-run`),
//! and the `am` banner.

use rgit_difftest::{Check, Fixture};

fn repo() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .write("b", "b\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .tick()
}

#[test]
fn colors() {
    let fx = repo()
        .git(["checkout", "-q", "-b", "side"])
        .write("a", "side\n")
        .git(["commit", "-q", "-am", "side"])
        .git(["checkout", "-q", "main"])
        .write("a", "main\n")
        .git(["commit", "-q", "-am", "main"])
        .git(["update-ref", "refs/remotes/origin/main", "HEAD~1"])
        .git(["config", "remote.origin.url", "/nonexistent"])
        .git([
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ])
        .git(["config", "branch.main.remote", "origin"])
        .git(["config", "branch.main.merge", "refs/heads/main"])
        .write("b", "changed\n")
        .write("new", "n\n")
        .git(["add", "new"])
        .write("untracked", "u\n");
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx;
    let settings: &[&[&str]] = &[
        &["-c", "color.status=always"],
        &["-c", "color.ui=always", "-c", "color.status.header=bold"],
        &[
            "-c",
            "color.status=always",
            "-c",
            "status.color.untracked=blue",
            "-c",
            "color.status.localBranch=yellow",
            "-c",
            "color.status.remoteBranch=cyan",
            "-c",
            "color.status.branch=magenta",
        ],
    ];
    for c in settings {
        for args in [&[][..], &["-sb"], &["-s", "-z"], &["--porcelain", "-b"]] {
            let mut argv = c.to_vec();
            argv.push("status");
            argv.extend_from_slice(args);
            fx.compare(&argv).checks(Check::OUTPUT).run();
        }
    }
    // A merge conflict, then a detached HEAD.
    let mut fx = fx.git(["stash", "-q", "-u"]);
    let _ = fx.git_output(["merge", "side"]);
    fx.compare(["-c", "color.status=always", "status"])
        .checks(Check::OUTPUT)
        .run();
    let mut fx = fx
        .git(["merge", "--abort"])
        .git(["checkout", "-q", "--detach", "HEAD~1"]);
    for args in [&["status"][..], &["status", "-sb"]] {
        let mut argv = vec![
            "-c",
            "color.status=always",
            "-c",
            "color.status.nobranch=blue",
        ];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
}

#[test]
fn compare_branches() {
    let fx = repo()
        .git(["init", "-q", "--bare", "up.git"])
        .git(["remote", "add", "origin", "up.git"])
        .git(["push", "-q", "origin", "main"])
        .git(["fetch", "-q", "origin"])
        .git(["checkout", "-q", "-b", "topic", "origin/main"])
        .git(["config", "push.default", "current"])
        .git(["push", "-q", "origin", "topic"])
        .write("c", "c\n")
        .git(["add", "c"])
        .git(["commit", "-q", "-m", "c"]);
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx;
    let specs = [
        "@{upstream} @{push}",
        "@{push} @{upstream}",
        "@{push}",
        "@{push} @{upstream} @{push}",
        "bogus @{upstream}",
        "",
    ];
    let run = |fx: &mut Fixture| {
        for spec in specs {
            let cfg = format!("status.compareBranches={spec}");
            for args in [
                &["status"][..],
                &["status", "--no-ahead-behind"],
                &["checkout", "topic"],
            ] {
                let mut argv = vec!["-c", cfg.as_str()];
                argv.extend_from_slice(args);
                fx.compare(&argv).checks(Check::OUTPUT).run();
            }
            fx.compare(["-c", cfg.as_str(), "commit", "--dry-run", "--allow-empty"])
                .checks(Check::OUTPUT)
                .run();
        }
    };
    // Ahead of both, then behind the push branch, then the push branch is
    // not fetched.
    run(&mut fx);
    let mut fx = fx
        .git(["push", "-q", "origin", "topic"])
        .git(["reset", "-q", "--hard", "HEAD~1"]);
    run(&mut fx);
    let mut fx = fx.git(["update-ref", "-d", "refs/remotes/origin/topic"]);
    run(&mut fx);
}

#[test]
fn am_banner() {
    let fx = repo()
        .write("a", "two\n")
        .git(["commit", "-q", "-am", "two"])
        .tick()
        .git(["format-patch", "-q", "-1", "-o", "patches"])
        .git(["reset", "-q", "--hard", "HEAD~1"])
        .write("a", "conflict\n")
        .git(["commit", "-q", "-am", "other"]);
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx;
    let _ = fx.git_output(["am", "patches/0001-two.patch"]);
    for args in [&["status"][..], &["status", "-uno"]] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}
