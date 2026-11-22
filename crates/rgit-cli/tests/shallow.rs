// SPDX-License-Identifier: MIT
//! Differential tests of shallow clones and fetches (#94): `clone
//! --depth/--shallow-since/--shallow-exclude/--reject-shallow`, `fetch
//! --depth/--deepen/--unshallow/--update-shallow`, push from a shallow
//! clone. The server side is the reference git (`PATH` starts with its
//! directory).

use rgit_difftest::{Check, Fixture};

mod common;
use common::servers::server_path;

fn source() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "src"]);
    for i in 1..=5 {
        fx = fx
            .tick()
            .write("src/f", format!("{i}\n"))
            .git(["-C", "src", "add", "f"])
            .git(["-C", "src", "commit", "-q", "-m", &format!("c{i}")]);
    }
    fx.git(["-C", "src", "branch", "side", "HEAD~1"])
        .git(["-C", "src", "tag", "v1", "HEAD~3"])
}

#[test]
fn shallow_clone_and_fetch_same_as_git() {
    let mut fx = source();
    if fx.is_skipped() {
        return;
    }
    let url = format!("file://{}/src", fx.work_dir().display());
    let path = server_path(&fx);
    let clones: &[(&[&str], &str)] = &[
        (&["clone", "--depth", "2", &url, "d1"], "d1"),
        (
            &["clone", "--depth=1", "--no-single-branch", &url, "d2"],
            "d2",
        ),
        (&["clone", "--shallow-exclude=v1", &url, "d3"], "d3"),
        (&["clone", "--depth", "1", "src", "d4"], "d4"),
        (&["clone", "--depth=1", "--branch", "v1", &url, "d5"], "d5"),
        (
            &["clone", "--shallow-since=2005-04-07T22:13:16", &url, "d6"],
            "d6",
        ),
    ];
    for (args, dir) in clones {
        fx.compare(*args).repo(dir).env("PATH", &path).run();
    }
    let shal = format!("file://{}/d1", fx.work_dir().display());
    for (args, dir) in [
        (&["clone", &shal, "e1"][..], "e1"),
        (&["clone", "--reject-shallow", &shal, "e2"][..], "e2"),
        (&["clone", "--reject-shallow", "d1", "e3"][..], "e3"),
    ] {
        fx.compare(args).repo(dir).env("PATH", &path).run();
    }
    let fx = fx
        .tick()
        .write("src/f", "6\n")
        .git(["-C", "src", "commit", "-q", "-a", "-m", "c6"]);
    let mut fx = fx;
    let steps: &[&[&str]] = &[
        &["-C", "d1", "rev-parse", "--is-shallow-repository"],
        &["-C", "d1", "fetch"],
        &["-C", "d1", "fetch", "--deepen=1"],
        &["-C", "d1", "log", "--oneline", "origin/main"],
        &["-C", "d1", "fetch", "--depth=1"],
        &["-C", "d1", "log", "--oneline", "origin/main"],
        &["-C", "d1", "fetch", "--unshallow"],
        &["-C", "d1", "rev-parse", "--is-shallow-repository"],
        &["-C", "d1", "fetch", "--unshallow"],
        &["-C", "d1", "fetch", "--depth=0"],
        &["-C", "d1", "fetch", "--deepen=1", "--depth=1"],
        &["-C", "d1", "fetch", "--deepen=-1"],
        &["-C", "d2", "fetch", "--shallow-exclude=v1"],
        &["-C", "d2", "log", "--oneline", "--all"],
    ];
    for args in steps {
        fx.compare(*args).repo(args[1]).env("PATH", &path).run();
    }
}

#[test]
fn fetch_from_shallow_repository_same_as_git() {
    let fx = source()
        .git(["clone", "-q", "--no-local", "--depth=2", "src", "shallow"])
        .git(["-C", "shallow", "checkout", "-q", "--orphan", "other"])
        .git(["-C", "shallow", "commit", "-q", "-m", "other"])
        .git(["init", "-q", "full"])
        .git(["init", "-q", "full2"]);
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx;
    let path = server_path(&fx);
    // Refs whose history the server cut are left out without
    // --update-shallow.
    fx.compare([
        "-C",
        "full",
        "fetch",
        "../shallow/.git",
        "refs/heads/*:refs/remotes/shallow/*",
    ])
    .repo("full")
    .env("PATH", &path)
    .run();
    fx.compare([
        "-C",
        "full2",
        "fetch",
        "--update-shallow",
        "../shallow/.git",
        "refs/heads/*:refs/remotes/shallow/*",
    ])
    .repo("full2")
    .env("PATH", &path)
    .run();
    fx.compare(["-C", "full2", "rev-parse", "--is-shallow-repository"])
        .repo("full2")
        .run();
}

#[test]
fn push_from_shallow_clone_same_as_git() {
    let fx = source().git(["clone", "-q", "--bare", "src", "srv.git"]);
    if fx.is_skipped() {
        return;
    }
    let url = format!("file://{}/srv.git", fx.work_dir().display());
    let fx = fx
        .git(["clone", "-q", "--depth", "2", &url, "sh"])
        .git(["init", "-q", "--bare", "empty.git"])
        .tick()
        .write("sh/f", "new\n")
        .git(["-C", "sh", "commit", "-q", "-a", "-m", "new"]);
    let mut fx = fx;
    let path = server_path(&fx);
    for args in [
        &["-C", "sh", "push", "origin", "main"][..],
        &["-C", "sh", "push", "origin", "main:refs/heads/new"][..],
        &["-C", "sh", "push", "../empty.git", "main"][..],
    ] {
        fx.compare(args)
            .repo("srv.git")
            .env("PATH", &path)
            .checks([
                Check::ExitCode,
                Check::Stdout,
                Check::Stderr,
                Check::Refs,
                Check::Objects,
                Check::Fsck,
            ])
            .run();
    }
}
