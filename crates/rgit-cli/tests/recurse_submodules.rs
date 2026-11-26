// SPDX-License-Identifier: MIT
//! Differential tests of `--recurse-submodules` (#121): the work tree
//! commands (`checkout`, `switch`, `reset`, `read-tree`, `restore`),
//! `ls-files`, `fetch`, `pull`, `push` and `branch`, against the reference
//! git. Every case runs in a fresh copy of a temporary fixture
//! (rgit-difftest) with its own `$HOME`.

use rgit_difftest::{Check, Fixture};

fn checks() -> impl Iterator<Item = Check> {
    Check::OUTPUT
        .into_iter()
        .chain([Check::Worktree, Check::Index, Check::Refs])
}

/// `inner` (one commit), `sub` (two commits, the second with `inner` as
/// submodule `nested`) and `super` with branches:
/// `none` (no submodule), `one` (`sub` at its first commit), `two` (`sub`
/// at its second commit, with the nested submodule), `file` (`sub`
/// replaced by a file). `super` is on `one` with `sub` checked out.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["config", "--global", "protocol.file.allow", "always"])
        // The copies of the fixture keep the mtimes, not the inodes.
        .git(["config", "--global", "core.checkStat", "minimal"])
        .git(["config", "--global", "core.trustctime", "false"])
        .git(["init", "-q", "inner"])
        .write("inner/i", "i\n")
        .git(["-C", "inner", "add", "i"])
        .git(["-C", "inner", "commit", "-q", "-m", "inner"])
        .git(["init", "-q", "sub"])
        .write("sub/a", "a\n")
        .git(["-C", "sub", "add", "a"])
        .git(["-C", "sub", "commit", "-q", "-m", "first"])
        .git(["-C", "sub", "branch", "first"])
        .tick()
        .write("sub/b", "b\n")
        .git(["-C", "sub", "add", "b"])
        .git(["-C", "sub", "submodule", "add", "-q", "../inner", "nested"])
        .git(["-C", "sub", "commit", "-q", "-m", "second"])
        .git(["init", "-q", "super"])
        .write("super/f", "f\n")
        .git(["-C", "super", "add", "f"])
        .git(["-C", "super", "commit", "-q", "-m", "base"])
        .git(["-C", "super", "branch", "none"])
        .git(["-C", "super", "submodule", "add", "-q", "../sub", "sub"])
        .git(["-C", "super/sub", "submodule", "update", "-q", "--init"])
        .git(["-C", "super", "commit", "-q", "-m", "two"])
        .git(["-C", "super", "branch", "two"])
        .git([
            "-C",
            "super/sub",
            "checkout",
            "-q",
            "--recurse-submodules",
            "first",
        ])
        .git(["-C", "super", "add", "sub"])
        .git(["-C", "super", "commit", "-q", "-m", "one"])
        .git(["-C", "super", "branch", "one"])
        .git(["-C", "super", "checkout", "-q", "-b", "file", "one"])
        .git(["-C", "super", "rm", "-q", "sub"])
        .write("super/sub", "now a file\n")
        .git(["-C", "super", "add", "sub"])
        .git(["-C", "super", "commit", "-q", "-m", "file"])
        .git([
            "-C",
            "super",
            "checkout",
            "-q",
            "--recurse-submodules",
            "one",
        ])
}

fn compare(mut fx: Fixture, args: &[&str]) {
    fx.compare(args)
        .in_dir("super")
        .repo("super")
        .checks(checks())
        .run();
}

/// After a command: the state of the submodule repository too.
fn compare_sub(mut fx: Fixture, args: &[&str]) {
    fx.compare(args)
        .in_dir("super")
        .repo("super/sub")
        .checks([Check::ExitCode, Check::Stderr, Check::Refs, Check::Worktree])
        .run();
}

#[test]
fn checkout_moves_submodules() {
    for target in ["two", "none", "file", "one"] {
        compare(fixture(), &["checkout", "--recurse-submodules", target]);
        compare_sub(fixture(), &["checkout", "--recurse-submodules", target]);
    }
    compare(
        fixture(),
        &["-c", "submodule.recurse=true", "switch", "--detach", "two"],
    );
    compare(
        fixture(),
        &["switch", "--recurse-submodules", "--detach", "none"],
    );
    compare(fixture(), &["checkout", "--no-recurse-submodules", "two"]);
}

#[test]
fn checkout_refuses_local_changes() {
    let fx = fixture().write("super/sub/a", "changed\n");
    compare(fx, &["checkout", "--recurse-submodules", "two"]);
    let fx = fixture()
        .write("super/sub/new", "new\n")
        .git(["-C", "super/sub", "add", "new"]);
    compare(fx, &["checkout", "--recurse-submodules", "two"]);
    // Forced: the local changes go.
    let fx = fixture().write("super/sub/a", "changed\n");
    compare(fx, &["checkout", "-f", "--recurse-submodules", "two"]);
    let fx = fixture().write("super/sub/a", "changed\n");
    compare(fx, &["checkout", "-f", "--recurse-submodules", "HEAD"]);
}

#[test]
fn checkout_bad_value() {
    compare(fixture(), &["checkout", "--recurse-submodules=foo", "two"]);
    compare(
        fixture(),
        &["read-tree", "--recurse-submodules=on-demand", "HEAD"],
    );
}

#[test]
fn removal_absorbs_git_directory() {
    let fx = fixture()
        .git(["-C", "super", "submodule", "deinit", "-q", "-f", "sub"])
        .git(["-C", "super", "checkout", "-q", "two"])
        .git(["-C", "super", "config", "submodule.sub.url", "../sub"])
        .git(["-C", "super", "submodule", "absorbgitdirs"]);
    compare(fx, &["checkout", "--recurse-submodules", "none"]);
}

#[test]
fn read_tree_and_reset() {
    compare(
        fixture(),
        &[
            "read-tree",
            "--recurse-submodules",
            "-u",
            "-m",
            "HEAD",
            "two",
        ],
    );
    compare(
        fixture(),
        &[
            "read-tree",
            "--recurse-submodules",
            "-n",
            "-m",
            "HEAD",
            "two",
        ],
    );
    compare_sub(
        fixture(),
        &["reset", "--hard", "--recurse-submodules", "two"],
    );
    let fx = fixture().write("super/sub/a", "changed\n");
    compare_sub(fx, &["reset", "--hard", "--recurse-submodules"]);
    compare(
        fixture(),
        &["reset", "--keep", "--recurse-submodules", "none"],
    );
}

#[test]
fn restore_submodule() {
    let fx = fixture()
        .git(["-C", "super/sub", "checkout", "-q", "main"])
        .write("super/sub/a", "changed\n");
    compare_sub(fx, &["restore", "--recurse-submodules", "sub"]);
    let fx = fixture().git(["-C", "super/sub", "checkout", "-q", "main"]);
    compare_sub(
        fx,
        &["checkout", "--recurse-submodules", "HEAD", "--", "sub"],
    );
}

#[test]
fn ls_files() {
    let fx = || {
        fixture().git([
            "-C",
            "super",
            "checkout",
            "-q",
            "--recurse-submodules",
            "two",
        ])
    };
    compare(fx(), &["ls-files", "--recurse-submodules"]);
    compare(fx(), &["ls-files", "--recurse-submodules", "--stage"]);
    compare(
        fx(),
        &["ls-files", "--recurse-submodules", "-z", "sub/nested"],
    );
    compare(fx(), &["ls-files", "--recurse-submodules", "*b"]);
    compare(
        fx(),
        &["ls-files", "--recurse-submodules", "--error-unmatch"],
    );
    compare(fx(), &["ls-files", "--recurse-submodules", "--others"]);
    let fx = fx().git(["-C", "super", "config", "submodule.sub.active", "false"]);
    compare(fx, &["ls-files", "--recurse-submodules"]);
}

/// A clone of `super` (on `two`) with its submodules, and new commits
/// upstream in `sub` (`third`) and `inner`, recorded in `super` (`next`).
fn fetch_fixture() -> Fixture {
    fixture()
        .git([
            "-C",
            "super",
            "checkout",
            "-q",
            "--recurse-submodules",
            "two",
        ])
        .git(["clone", "-q", "--recurse-submodules", "super", "down"])
        .tick()
        .write("inner/i", "i2\n")
        .git(["-C", "inner", "commit", "-q", "-am", "inner2"])
        .write("sub/a", "a3\n")
        .git(["-C", "sub", "commit", "-q", "-am", "third"])
}

fn compare_down(mut fx: Fixture, args: &[&str]) {
    fx.compare(args)
        .in_dir("down")
        .repo("down/sub")
        .checks(Check::OUTPUT.into_iter().chain([Check::Refs]))
        .run();
}

#[test]
fn fetch_recurses() {
    compare_down(fetch_fixture(), &["fetch", "--recurse-submodules"]);
    compare_down(fetch_fixture(), &["fetch", "--recurse-submodules", "-j2"]);
    compare_down(
        fetch_fixture(),
        &["fetch", "--recurse-submodules", "--quiet"],
    );
    compare_down(fetch_fixture(), &["fetch", "--recurse-submodules=no"]);
    compare_down(fetch_fixture(), &["fetch"]);
    compare_down(
        fetch_fixture(),
        &["-c", "fetch.recurseSubmodules=true", "fetch"],
    );
    compare_down(fetch_fixture(), &["fetch", "--recurse-submodules=foo"]);
}

#[test]
fn fetch_on_demand() {
    let fx = || {
        fetch_fixture()
            .git(["-C", "super/sub", "fetch", "-q"])
            .git(["-C", "super/sub", "checkout", "-q", "origin/main"])
            .git(["-C", "super", "commit", "-q", "-am", "next"])
    };
    compare_down(fx(), &["fetch"]);
    compare_down(fx(), &["fetch", "--recurse-submodules=on-demand"]);
    compare_down(fx(), &["pull", "--no-rebase", "--recurse-submodules"]);
}

/// `work` with a submodule `lib` that has a remote (`lib.git`), and a
/// bare `pub.git` for the superproject; `lib` and `work` have a commit
/// more than their remotes.
fn push_fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["config", "--global", "protocol.file.allow", "always"])
        .git(["config", "--global", "receive.autogc", "false"])
        .git(["init", "-q", "--bare", "pub.git"])
        .git(["init", "-q", "--bare", "lib.git"])
        .git(["init", "-q", "work"])
        .git(["-C", "work", "config", "push.default", "matching"])
        .git(["init", "-q", "work/lib"])
        .git(["-C", "work/lib", "config", "push.default", "matching"])
        .write("work/lib/x", "x\n")
        .git(["-C", "work/lib", "add", "x"])
        .git(["-C", "work/lib", "commit", "-q", "-m", "x"])
        .git(["-C", "work/lib", "remote", "add", "origin", "../../lib.git"])
        .git(["-C", "work/lib", "push", "-q", "origin", "main"])
        .git(["-C", "work", "add", "lib"])
        .git(["-C", "work", "commit", "-q", "-m", "lib"])
        .git(["-C", "work", "push", "-q", "../pub.git", "main"])
        .tick()
        .write("work/lib/y", "y\n")
        .git(["-C", "work/lib", "add", "y"])
        .git(["-C", "work/lib", "commit", "-q", "-m", "y"])
        .git(["-C", "work", "add", "lib"])
        .git(["-C", "work", "commit", "-q", "-m", "lib y"])
}

#[test]
fn push_recurses() {
    let run = |args: &[&str]| {
        let mut fx = push_fixture();
        fx.compare(args)
            .in_dir("work")
            .repo("lib.git")
            .checks([Check::ExitCode, Check::Stderr, Check::Refs])
            .run();
        let mut fx = push_fixture();
        fx.compare(args)
            .in_dir("work")
            .repo("pub.git")
            .checks([Check::ExitCode, Check::Stdout, Check::Refs])
            .run();
    };
    run(&["push", "--recurse-submodules=check", "../pub.git", "main"]);
    run(&[
        "-c",
        "push.recurseSubmodules=check",
        "push",
        "../pub.git",
        "main",
    ]);
    run(&[
        "push",
        "--recurse-submodules=on-demand",
        "../pub.git",
        "main",
    ]);
    run(&["push", "--recurse-submodules=only", "../pub.git", "main"]);
    run(&["push", "--recurse-submodules=no", "../pub.git", "main"]);
    run(&["push", "--recurse-submodules=yes", "../pub.git", "main"]);
    run(&[
        "-c",
        "push.recurseSubmodules=yes",
        "push",
        "../pub.git",
        "main",
    ]);
    run(&[
        "push",
        "--dry-run",
        "--recurse-submodules=on-demand",
        "../pub.git",
        "main",
    ]);
}

#[test]
fn branch_recurses() {
    let fx = || {
        fixture()
            .git([
                "-C",
                "super",
                "checkout",
                "-q",
                "--recurse-submodules",
                "two",
            ])
            .git([
                "-C",
                "super",
                "config",
                "submodule.propagateBranches",
                "true",
            ])
    };
    compare_sub(fx(), &["branch", "--recurse-submodules", "topic"]);
    compare(fx(), &["branch", "--recurse-submodules", "topic", "one"]);
    compare(fx(), &["branch", "--recurse-submodules", "-D", "one"]);
    let taken = fx().git(["-C", "super/sub", "branch", "topic"]);
    compare_sub(taken, &["branch", "--recurse-submodules", "topic"]);
    let taken = fx().git(["-C", "super/sub", "branch", "topic", "first"]);
    compare_sub(
        taken,
        &["branch", "--recurse-submodules", "--force", "topic"],
    );
    compare_sub(
        fx(),
        &["branch", "--track", "--recurse-submodules", "topic", "two"],
    );
    compare(
        fx(),
        &[
            "-c",
            "submodule.propagateBranches=false",
            "branch",
            "--recurse-submodules",
            "x",
        ],
    );
}

/// A submodule path that is a symbolic link (here to another repository)
/// is never followed: git names it and ends before anything is written.
#[test]
fn symlinked_submodule_path() {
    let linked = || {
        fixture()
            .remove("super/sub")
            .git(["init", "-q", "super/target"])
            .git(["-C", "super/target", "fetch", "-q", "../../sub"])
            .symlink("target", "super/sub")
    };
    for args in [
        &["restore", "--recurse-submodules", "sub"][..],
        &["checkout", "-f", "--recurse-submodules", "none"],
        &["checkout", "-f", "--recurse-submodules", "two"],
        &["read-tree", "-u", "--reset", "--recurse-submodules", "none"],
    ] {
        let mut fx = linked();
        fx.compare(args)
            .in_dir("super")
            .repo("super")
            .checks(Check::OUTPUT)
            .run();
    }
}
