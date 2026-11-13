// SPDX-License-Identifier: MIT
//! Differential tests of `clone` (#64) from local paths and `file://`:
//! output, refs, reflogs, config, index and work tree equal git's.

use rgit_difftest::{Check, Fixture};

fn source() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "src"])
        .git(["-C", "src", "commit", "-q", "--allow-empty", "-m", "zero"])
        .tick()
        .write("src/a", "a\n")
        .write_executable("src/d/b", "#!/bin/sh\n")
        .write("src/d/e/f", "deep\n")
        .symlink("a", "src/l")
        .git(["-C", "src", "add", "."])
        .git(["-C", "src", "commit", "-q", "-m", "one"])
        .git(["-C", "src", "branch", "side", "HEAD~1"])
        .git(["-C", "src", "tag", "v1", "HEAD~1"])
        .git(["-C", "src", "tag", "-a", "-m", "annotated", "v2"])
}

#[test]
fn clone_local_and_file_url_same_as_git() {
    let mut fx = source()
        .git(["init", "-q", "--bare", "empty.git"])
        .git(["clone", "-q", "--bare", "src", "bare.git"]);
    if fx.is_skipped() {
        return;
    }
    let url = format!("file://{}/src", fx.work_dir().display());
    let cases: &[(&[&str], &str)] = &[
        (&["clone", "src", "c1"], "c1"),
        (&["clone", "--no-local", "src", "c2"], "c2"),
        (&["clone", &url, "c3"], "c3"),
        (&["clone", "--bare", "src", "c4.git"], "c4.git"),
        (&["clone", "--mirror", "src", "c5.git"], "c5.git"),
        (&["clone", "-b", "side", "src", "c6"], "c6"),
        (&["clone", "-o", "up", "src", "c7"], "c7"),
        (&["clone", "--single-branch", "src", "c8"], "c8"),
        // A local clone copies every object, so git also writes tags outside
        // the branch; rgit clones local paths like --no-local (#64).
        (
            &[
                "clone",
                "--no-local",
                "--single-branch",
                "-b",
                "side",
                "src",
                "c9",
            ],
            "c9",
        ),
        (&["clone", "--no-local", "--no-tags", "src", "c10"], "c10"),
        (&["clone", "-n", "src", "c11"], "c11"),
        (&["clone", "-q", "src", "c12"], "c12"),
        (
            &[
                "clone",
                "-c",
                "foo.bar=baz",
                "-c",
                "foo.bar=qux",
                "src",
                "c13",
            ],
            "c13",
        ),
        (&["clone", "empty.git", "c14"], "c14"),
        (&["clone", "--bare", "empty.git", "c15"], "c15"),
        (&["clone", "bare.git"], "bare"),
        (&["clone", "src/.git", "c16"], "c16"),
        (&["clone", "--", "src", "c17"], "c17"),
        (
            &["clone", "--bare", "-o", "up", "src", "c18.git"],
            "c18.git",
        ),
    ];
    for (args, dir) in cases {
        let c = fx.compare(*args).repo(*dir);
        // A bare clone's git directory is in the work tree check; its
        // templates (hooks, description) are rgit's own.
        if dir.ends_with(".git") || *dir == "c15" {
            c.skip(Check::Worktree).run();
        } else {
            c.run();
        }
    }
}

#[test]
fn clone_tag_and_detached_head_same_as_git() {
    let mut fx = source()
        .git(["clone", "-q", "src", "det"])
        .git(["-C", "det", "checkout", "-q", "--detach", "HEAD~1"]);
    if fx.is_skipped() {
        return;
    }
    fx.compare(["clone", "-b", "v1", "src", "t1"])
        .repo("t1")
        .run();
    fx.compare(["clone", "-b", "v2", "src", "t2"])
        .repo("t2")
        .run();
    fx.compare(["clone", "det", "t3"]).repo("t3").run();
}

#[test]
fn clone_errors_and_cleanup_same_as_git() {
    let mut fx = source()
        .write("full/x", "x\n")
        .git(["init", "-q", "emptydir"]);
    if fx.is_skipped() {
        return;
    }
    std::fs::create_dir(fx.work_dir().join("hollow")).unwrap();
    let cases: &[&[&str]] = &[
        &["clone"],
        &["clone", "-h"],
        &["clone", "a", "b", "c"],
        &["clone", "nope", "x1"],
        &["clone", "src", "full"],
        &["clone", "src"],
        &["clone", "--depth", "0", "src", "x3"],
        &["clone", "-o", "bad name", "src", "x4"],
        &["clone", "-b", "nope", "src", "x5"],
        &["clone", "-b", "nope", "src", "hollow"],
        &["clone", "--upload-pack=false", "file://nowhere/x", "x6"],
    ];
    for args in cases {
        fx.compare(*args)
            .checks([
                Check::ExitCode,
                Check::Stdout,
                Check::Stderr,
                Check::Worktree,
            ])
            .run();
    }
}

#[test]
fn clone_over_ssh_same_as_git() {
    // A fake ssh that runs the remote command locally (variant "ssh":
    // `-o SendEnv=GIT_PROTOCOL` options, then the host).
    let mut fx = source().write_executable(
        "fake-ssh",
        "#!/bin/sh\nwhile test \"$1\" = -o; do shift 2; done\nshift\nexec sh -c \"$*\"\n",
    );
    if fx.is_skipped() {
        return;
    }
    let ssh = fx.work_dir().join("fake-ssh");
    for (args, dir) in [
        (&["clone", "host:src", "s1"][..], "s1"),
        (
            &["clone", "--bare", "ssh://host/./src", "s2.git"][..],
            "s2.git",
        ),
    ] {
        let c = fx
            .compare(args)
            .repo(dir)
            .env("GIT_SSH", &ssh)
            .env("GIT_SSH_VARIANT", "ssh");
        if dir.ends_with(".git") {
            c.skip(Check::Worktree).run();
        } else {
            c.run();
        }
    }
}
