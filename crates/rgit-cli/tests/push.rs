// SPDX-License-Identifier: MIT
//! `push` over the local transport against the reference `git
//! receive-pack`: output, exit code, the client's refs, reflogs and
//! configuration, and the server's files (the server repository lives in
//! the fixture's work directory, so the work tree check covers it).

use rgit_difftest::{Check, Fixture};

/// The server program for both sides: the reference `git receive-pack`
/// without the exported `GIT_EXEC_PATH`. In the fixture the `git` first in
/// `PATH` and in the exec path is rgit, and receive-pack would run rgit's
/// (missing) `unpack-objects` instead of its own.
fn receive_pack(fx: &Fixture) -> String {
    format!(
        "env -u GIT_EXEC_PATH '{}' receive-pack",
        fx.reference_git().display()
    )
}

/// A client repository with a bare server `srv.git` as `origin`, plus a
/// non-bare `nb` that has `main` checked out (pushes to it pass
/// `--receive-pack`).
fn fixture() -> Fixture {
    let fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return fx;
    }
    let rp = receive_pack(&fx);
    fx.git(["init", "-q", "--bare", "srv.git"])
        .git(["init", "-q", "-b", "main"])
        .write("f", "one\n")
        .git(["add", "f"])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .git(["remote", "add", "origin", "srv.git"])
        .git(["config", "remote.origin.receivepack", &rp])
        .git(["clone", "-q", ".", "nb"])
}

fn run(fx: &mut Fixture, cases: &[&[&str]]) {
    for args in cases {
        let mut argv = vec!["push"];
        argv.extend_from_slice(args);
        fx.compare(&argv).run();
    }
}

#[test]
fn push_new_ff_force_delete_tags() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    run(
        &mut fx,
        &[
            &[],
            &["origin", "main"],
            &["-v", "origin", "main"],
            &["--porcelain", "origin", "main", "main:refs/heads/side"],
            &["origin", "main:x", "HEAD:refs/other/y"],
            &["-n", "origin", "main:dry"],
            &["origin", ":nothere"],
            &["origin", "nosuch"],
            &["origin", "HEAD~0:foo"],
        ],
    );
    let mut fx = fx
        .git(["push", "-q", "origin", "main"])
        .write("f", "two\n")
        .git(["commit", "-q", "-a", "-m", "two"])
        .tick()
        .git(["tag", "v1"])
        .git(["tag", "-a", "-m", "annotated", "v2"]);
    run(
        &mut fx,
        &[
            &["origin", "main"],
            &["-q", "origin", "main", "v1", "v2"],
            &["origin", "tag", "v1"],
            &["--tags", "origin"],
            &["--all", "origin"],
            &["origin", ":side"],
            &["-d", "origin", "x"],
            &["--porcelain", "origin", ":side"],
        ],
    );
}

#[test]
fn push_rejections() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx
        .git(["push", "-q", "origin", "main", "main:other"])
        .git(["tag", "t1"])
        .git(["push", "-q", "origin", "t1"])
        .write("f", "two\n")
        .git(["commit", "-q", "-a", "--amend", "-m", "amended"])
        .tick()
        .git(["tag", "-f", "t1"]);
    let rp = receive_pack(&fx);
    run(
        &mut fx,
        &[
            &["origin", "main"],
            &["origin", "main:other", "main:new"],
            &["--porcelain", "origin", "main", "t1"],
            &["-q", "origin", "main"],
            &["--atomic", "origin", "main", "main:new2"],
            &["origin", "HEAD^{tree}:refs/heads/tree"],
            &["-f", "origin", "main"],
            &["origin", "+t1"],
            &["--receive-pack", &rp, "nb", "main"],
            &[
                "--receive-pack",
                &rp,
                "--porcelain",
                "nb",
                "main",
                "main:fresh",
            ],
        ],
    );
}

#[test]
fn push_default_and_upstream() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    run(
        &mut fx,
        &[
            &[],
            &["-u", "origin", "main"],
            &["-n", "-u", "origin", "main:dry"],
            &["-h"],
            &["--all", "--tags"],
            &["-d", "origin"],
            &["--all", "origin", "main"],
            &["origin", "main:refs/heads/a b"],
            &["nosuchremote"],
        ],
    );
    for (mode, extra) in [
        ("nothing", None),
        ("matching", None),
        ("current", None),
        ("upstream", None),
        ("simple", Some("push.autoSetupRemote=true")),
    ] {
        let mut argv = vec!["-c".to_string(), format!("push.default={mode}")];
        if let Some(e) = extra {
            argv.extend(["-c".to_string(), e.to_string()]);
        }
        argv.push("push".into());
        fx.compare(&argv).run();
    }
    let mut fx = fx
        .git(["branch", "-q", "--set-upstream-to=origin/main"])
        .git(["config", "branch.main.merge", "refs/heads/other"]);
    fx.compare(["push"]).run();
    fx.compare(["-c", "push.default=upstream", "push"])
        .checks(Check::OUTPUT)
        .run();
    let mut fx = fx.git([
        "config",
        "remote.origin.push",
        "refs/heads/*:refs/heads/mirror/*",
    ]);
    run(&mut fx, &[&[], &["origin", "main"]]);
}

#[test]
fn push_over_ssh() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    // A stand-in for ssh that runs the remote command locally.
    let mut fx = fx.write_executable(
        "fakessh",
        "#!/bin/sh\nwhile [ \"${1#-}\" != \"$1\" ]; do shift; done\nshift\nexec sh -c \"$*\"\n",
    );
    let dir = fx.work_dir();
    let rp = receive_pack(&fx);
    let ssh = dir.join("fakessh");
    let scp_like = format!("localhost:{}/srv.git", dir.display());
    let url = format!("ssh://localhost{}/srv.git", dir.display());
    for args in [
        vec!["push", "--receive-pack", &rp, &url, "main"],
        vec!["push", "--receive-pack", &rp, &scp_like, "main:x", ":main"],
    ] {
        fx.compare(&args).env("GIT_SSH", &ssh).run();
    }
}
