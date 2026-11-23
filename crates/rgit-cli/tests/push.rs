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

/// `protocol.version` (v1 asks for `version 1`, v2 falls back to v0) and
/// `protocol.<name>.allow`.
#[test]
fn push_protocol_version_and_allow() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["-c", "protocol.version=1", "push", "origin", "main"][..],
        &["-c", "protocol.version=2", "push", "origin", "main:b"],
        &["-c", "protocol.version=0", "push", "origin", "main:c"],
        &[
            "-c",
            "protocol.file.allow=never",
            "push",
            "origin",
            "main:d",
        ],
        &["-c", "protocol.version=5", "push", "origin", "main:e"],
    ] {
        fx.compare(args).run();
    }
    // The client asks for version 1 and the server answers with it.
    let trace = fx.work_dir().join("push.trace");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(["-c", "protocol.version=1", "push", "origin", "main:f"])
        .current_dir(fx.work_dir())
        .env("HOME", fx.home_dir())
        .env("XDG_CONFIG_HOME", fx.home_dir().join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TRACE_PACKET", &trace)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let t = std::fs::read_to_string(&trace).unwrap();
    assert!(t.contains("push< version 1"), "{t}");
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

/// An object to send that is corrupt (its content does not match its id,
/// t5504): the push ends with the refs rejected and exit code 1 instead
/// of waiting for the server, which waits for the rest of the pack (#528).
#[test]
fn push_with_corrupt_object_ends() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx
        .write("greetings", "hello\n")
        .write("bye", "bye\n")
        .git(["add", "greetings"])
        .git(["commit", "-q", "-m", "greetings"])
        .git(["init", "-q", "--bare", "dst.git"]);
    let path = |hex: Vec<u8>| {
        let hex = String::from_utf8_lossy(&hex).trim().to_string();
        fx.work_dir()
            .join(".git/objects")
            .join(&hex[..2])
            .join(&hex[2..])
    };
    let good = path(fx.git_stdout(["rev-parse", ":greetings"]));
    let bad = path(fx.git_stdout(["hash-object", "-w", "bye"]));
    std::fs::remove_file(fx.work_dir().join("bye")).unwrap();
    std::fs::rename(&bad, &good).unwrap();
    let rp = receive_pack(&fx);
    fx.compare([
        "push",
        "--porcelain",
        "--receive-pack",
        &rp,
        "dst.git",
        "main:refs/heads/test",
    ])
    .only_exit_code()
    .run();
}

/// `--progress` to a pipe: `Enumerating objects`, `Counting objects`,
/// `Delta compression using up to N threads`, `Compressing objects`,
/// `Writing objects` with size and rate, and the `Total` line.
#[test]
fn push_progress_meters() {
    use rgit_difftest::Normalizer;
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let mut text = String::new();
    for i in 0..300 {
        text.push_str(&format!("line {i} of a longer file\n"));
    }
    let mut fx = fx
        .write("long", &text)
        .git(["add", "long"])
        .git(["commit", "-q", "-m", "long"])
        .tick()
        .git(["push", "-q", "origin", "main"])
        .write("long", text.replace("line 7 ", "line seven "))
        .write("other", &text[100..])
        .git(["add", "-A"])
        .git(["commit", "-q", "-m", "two"])
        .tick();
    fx.compare([
        "-c",
        "pack.threads=2",
        "push",
        "--progress",
        "origin",
        "main",
    ])
    .checks([Check::ExitCode, Check::Stderr])
    // Other delta bases are written ahead at other places.
    .normalize_stderr(Normalizer::regex(
        r"Writing objects: +\d+% \(\d+/\d+\)\r",
        "",
    ))
    .normalize_stderr(Normalizer::regex(
        r", [0-9.]+ (bytes|KiB|MiB) \| [0-9.]+ (bytes|KiB|MiB)/s",
        ", <SIZE> | <RATE>",
    ))
    .normalize_stderr(Normalizer::regex(
        r"Total (\d+) \(delta \d+\), reused (\d+) \(delta \d+\)",
        "Total $1 (delta <D>), reused $2 (delta <D>)",
    ))
    .run();
    // Nothing to send: no meters, only the `Total` line.
    fx.compare(["push", "--progress", "origin", "main:refs/heads/copy"])
        .checks([Check::ExitCode, Check::Stderr])
        .run();
}

/// `color.push` (`color.push.error`) and `color.transport`
/// (`color.transport.rejected`, `color.transport.reset`) colour the
/// `failed to push` line and the flag and summary of failed refs;
/// `color.ui` does not turn them on.
#[test]
fn push_colors() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx
        .write("f", "two\n")
        .git(["commit", "-q", "-a", "-m", "two"])
        .tick()
        .git(["push", "-q", "origin", "main"]);
    for args in [
        &[
            "-c",
            "color.transport=always",
            "-c",
            "color.advice=always",
            "-c",
            "color.push=always",
            "push",
            "origin",
            "main^:main",
        ][..],
        &["-c", "color.ui=always", "push", "origin", "main^:main"],
        &[
            "-c",
            "color.transport=always",
            "-c",
            "color.transport.rejected=blue",
            "-c",
            "color.transport.reset=green",
            "-c",
            "color.push=always",
            "-c",
            "color.push.error=magenta",
            "push",
            "origin",
            "main^:main",
            "main:new",
        ],
        &[
            "-c",
            "color.push=always",
            "push",
            "-v",
            "origin",
            "main^:main",
            "main:main",
        ],
    ] {
        fx.compare(args).run();
    }
}
