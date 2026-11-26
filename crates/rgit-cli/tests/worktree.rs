// SPDX-License-Identifier: MIT
//! Differential tests of `rgit worktree` and of linked worktrees in other
//! commands, against the reference git. Every case runs in a temporary
//! fixture (rgit-difftest); linked worktrees are created inside it.

use rgit_difftest::{Check, Fixture};

/// A repository with two commits and the branches `other` and `old`.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "main"])
        .write("main/a", "a\n")
        .git(["-C", "main", "add", "a"])
        .git(["-C", "main", "commit", "-q", "-m", "one"])
        .tick()
        .write("main/b", "b\n")
        .git(["-C", "main", "add", "b"])
        .git(["-C", "main", "commit", "-q", "-m", "two"])
        .git(["-C", "main", "branch", "other"])
        .git(["-C", "main", "branch", "old", "HEAD~1"])
}

/// Admin files of the linked worktree `id` that the comparison includes.
fn admin_files<'a>(
    cmp: rgit_difftest::Comparison<'a>,
    ids: &[&str],
) -> rgit_difftest::Comparison<'a> {
    let mut cmp = cmp;
    for id in ids {
        for f in ["HEAD", "gitdir", "commondir", "locked", "logs/HEAD"] {
            cmp = cmp.git_file(format!("worktrees/{id}/{f}"));
        }
    }
    cmp
}

/// `worktree add` in its forms: the state (refs, reflogs, admin files, the
/// new work trees) and the messages must match.
#[test]
fn add() {
    let cases: &[&[&str]] = &[
        &["../w1"],
        &["../w1", "other"],
        &["../w1", "main"],
        &["-b", "nb", "../w1", "HEAD~1"],
        &["-B", "other", "../w1", "HEAD~1"],
        &["-d", "../w1"],
        &["--detach", "../w1", "old"],
        &["../w1", "HEAD~1"],
        &["--orphan", "../w1"],
        &["--orphan", "-b", "orph", "../w1"],
        &["--no-checkout", "../w1"],
        &["--lock", "../w1"],
        &["--lock", "--reason", "on a stick", "../w1"],
        &["-q", "../w1"],
        &["../sub/deep/w1"],
        &["../w1", "nonexistent"],
        &["-b", "other", "../w1"],
        &["-d", "-b", "x", "../w1"],
        &["--orphan", "-d", "../w1"],
        &["--orphan", "../w1", "HEAD"],
        &["--reason", "x", "../w1"],
        &["../w1", "HEAD", "extra"],
        &[],
    ];
    for args in cases {
        let mut fx = fixture();
        let mut argv = vec!["worktree", "add"];
        argv.extend_from_slice(args);
        let cmp = fx
            .compare(&argv)
            .in_dir("main")
            .repo("main")
            .skip(Check::Fsck);
        admin_files(cmp, &["w1"]).run();
    }
}

/// A branch checked out elsewhere, an existing path, a missing but
/// registered worktree.
#[test]
fn add_refusals() {
    let mut fx = fixture().git(["-C", "main", "worktree", "add", "-q", "../w1", "other"]);
    for args in [
        &["worktree", "add", "../w2", "other"][..],
        &["worktree", "add", "-B", "other", "../w2"],
        &["worktree", "add", "../w1"],
        &["worktree", "add", "-f", "../w2", "other"],
    ] {
        fx.compare(args)
            .in_dir("main")
            .repo("main")
            .skip(Check::Fsck)
            .run();
    }
    let mut fx = fixture()
        .git(["-C", "main", "worktree", "add", "-q", "-d", "../gone"])
        .remove("gone");
    for args in [
        &["worktree", "add", "-d", "../gone"][..],
        &["worktree", "add", "-f", "-d", "../gone"],
    ] {
        fx.compare(args)
            .in_dir("main")
            .repo("main")
            .skip(Check::Fsck)
            .run();
    }
}

fn with_worktrees() -> Fixture {
    fixture()
        .git(["-C", "main", "worktree", "add", "-q", "../w1"])
        .git([
            "-C", "main", "worktree", "add", "-q", "-d", "../w2", "HEAD~1",
        ])
        .git(["-C", "main", "worktree", "add", "-q", "--orphan", "../w3"])
        .git([
            "-C", "main", "worktree", "add", "-q", "--lock", "--reason", "usb", "../w4",
        ])
        .git(["-C", "main", "worktree", "add", "-q", "../gone"])
        .remove("gone")
}

#[test]
fn list() {
    let mut fx = with_worktrees();
    for args in [
        &["worktree", "list"][..],
        &["worktree", "list", "-v"],
        &["worktree", "list", "--porcelain"],
        &["worktree", "list", "--porcelain", "-z"],
        &["worktree", "list", "-z"],
        &["worktree", "list", "-v", "--porcelain"],
        &["worktree", "list", "--expire", "1.day.ago"],
    ] {
        fx.compare(args).in_dir("main").output_only().run();
        fx.compare(args).in_dir("w1").output_only().run();
    }
}

/// A main worktree whose HEAD points at something that is no reference
/// name is listed as broken, without a branch (t2402).
#[test]
fn list_with_broken_main_head() {
    let mut fx = with_worktrees().write("main/.git/HEAD", "ref: .broken\n");
    for args in [
        &["worktree", "list"][..],
        &["worktree", "list", "--porcelain"],
    ] {
        fx.compare(args).in_dir("w1").output_only().run();
    }
}

#[test]
fn lock_unlock() {
    let cases: &[&[&str]] = &[
        &["lock", "../w1"],
        &["lock", "--reason", "why", "w1"],
        &["lock", "w4"],
        &["lock", "."],
        &["lock", "nowhere"],
        &["unlock", "w4"],
        &["unlock", "w1"],
        &["unlock", "."],
        &["lock"],
    ];
    for args in cases {
        let mut fx = with_worktrees();
        let mut argv = vec!["worktree"];
        argv.extend_from_slice(args);
        let cmp = fx
            .compare(&argv)
            .in_dir("main")
            .repo("main")
            .skip(Check::Fsck);
        admin_files(cmp, &["w1", "w4"]).run();
    }
}

#[test]
fn move_and_remove() {
    let cases: &[&[&str]] = &[
        &["move", "w1", "../moved"],
        &["move", "w1", "../w2"],
        &["move", "w4", "../moved"],
        &["move", "-f", "-f", "w4", "../moved"],
        &["move", ".", "../moved"],
        &["move", "w1", "../gone"],
        &["move", "-f", "w1", "../gone"],
        &["remove", "w1"],
        &["remove", "w4"],
        &["remove", "-f", "-f", "w4"],
        &["remove", "."],
        &["remove", "gone"],
        &["remove"],
    ];
    for args in cases {
        let mut fx = with_worktrees();
        let mut argv = vec!["worktree"];
        argv.extend_from_slice(args);
        let cmp = fx
            .compare(&argv)
            .in_dir("main")
            .repo("main")
            .skip(Check::Fsck);
        admin_files(cmp, &["w1", "w2", "w4"]).run();
    }
    // A dirty worktree is only removed with --force.
    let mut fx = with_worktrees().write("w1/new", "n\n");
    fx.compare(["worktree", "remove", "w1"])
        .in_dir("main")
        .repo("main")
        .skip(Check::Fsck)
        .run();
    fx.compare(["worktree", "remove", "--force", "w1"])
        .in_dir("main")
        .repo("main")
        .skip(Check::Fsck)
        .run();
    // Submodule repositories in the worktree's admin directory: never
    // moved, removed only with --force.
    for args in [
        &["move", "w1", "../moved"][..],
        &["move", "-f", "w1", "../moved"],
        &["remove", "w1"],
        &["remove", "-f", "w1"],
    ] {
        let mut fx = with_worktrees().write("main/.git/worktrees/w1/modules/sub/HEAD", "x\n");
        let mut argv = vec!["worktree"];
        argv.extend_from_slice(args);
        fx.compare(&argv)
            .in_dir("main")
            .repo("main")
            .skip(Check::Fsck)
            .run();
    }
}

/// `worktree add --guess-remote` (or `worktree.guessRemote`) without a
/// commit in a repository with an unborn HEAD uses the remote branch.
#[test]
fn add_guess_remote_unborn() {
    for cfg in [
        &["worktree", "add", "--guess-remote"][..],
        &["-c", "worktree.guessRemote=true", "worktree", "add"],
    ] {
        let mut fx = Fixture::new()
            .rgit(env!("CARGO_BIN_EXE_rgit"))
            .git(["init", "-q", "-b", "main", "repo"])
            .git(["init", "-q", "-b", "main", "up"])
            .git(["-C", "up", "commit", "-q", "--allow-empty", "-m", "c"])
            .git(["-C", "up", "branch", "foo"])
            .git(["-C", "repo", "remote", "add", "up", "../up"])
            .git(["-C", "repo", "fetch", "-q", "up"]);
        let mut argv = cfg.to_vec();
        argv.push("../foo");
        fx.compare(&argv)
            .in_dir("repo")
            .repo("repo")
            .skip(Check::Fsck)
            .run();
    }
}

#[test]
fn prune() {
    let mut fx = with_worktrees()
        .write("main/.git/worktrees/file", "")
        .write("main/.git/worktrees/nogitdir/x", "")
        .write("main/.git/worktrees/empty/gitdir", "");
    for args in [
        &["worktree", "prune", "-n"][..],
        &["worktree", "prune", "-v", "--expire", "1.day.ago"],
        &["worktree", "prune", "-v"],
        &["worktree", "prune", "x"],
    ] {
        fx.compare(args)
            .in_dir("main")
            .repo("main")
            .skip(Check::Fsck)
            .run();
    }
}

#[test]
fn repair() {
    // A moved linked worktree, repaired from the main worktree and from
    // inside.
    let mut fx = with_worktrees();
    std::fs::rename(fx.work_dir().join("w1"), fx.work_dir().join("w1moved")).ok();
    for args in [
        &["worktree", "repair"][..],
        &["worktree", "repair", "../w1moved"],
    ] {
        let cmp = fx
            .compare(args)
            .in_dir("main")
            .repo("main")
            .skip(Check::Fsck);
        admin_files(cmp, &["w1"]).run();
    }
    let mut fx = with_worktrees().write("w2/.git", "garbage\n");
    fx.compare(["worktree", "repair"])
        .in_dir("main")
        .repo("main")
        .skip(Check::Fsck)
        .run();
    fx.compare(["worktree", "repair", "nowhere"])
        .in_dir("main")
        .repo("main")
        .skip(Check::Fsck)
        .run();
}

/// `rev-parse --git-path`: repeated slashes and `.lock` do not change
/// whether a path is per worktree, and `GIT_INDEX_FILE`,
/// `GIT_GRAFT_FILE` and `GIT_OBJECT_DIRECTORY` relocate their paths.
#[test]
fn git_path_rules() {
    let mut fx = with_worktrees();
    let paths = [
        "logs/HEAD.lock",
        "info//sparse-checkout",
        "info/sparse-checkout",
        "packed-refs.lock",
        "config.lock",
        "index",
        "index/foo",
        "info/grafts",
        "info/////grafts",
        "objects",
        "objects/foo",
        "objects2",
    ];
    let mut args = vec!["rev-parse"];
    for p in paths {
        args.extend(["--git-path", p]);
    }
    fx.compare(&args).in_dir("w1").output_only().run();
    for (var, value) in [
        ("GIT_INDEX_FILE", "foo"),
        ("GIT_GRAFT_FILE", "foo"),
        ("GIT_OBJECT_DIRECTORY", "foo"),
    ] {
        fx.compare(&args)
            .in_dir("w1")
            .env(var, value)
            .output_only()
            .run();
    }
}

/// Commands inside a linked worktree use its own `HEAD`, index and
/// per-worktree refs, and see the other worktrees' refs by name.
#[test]
fn refs_in_linked_worktree() {
    let mut fx = with_worktrees().write("w1/c", "c\n");
    for args in [
        &["status"][..],
        &["branch"],
        &["branch", "-vv"],
        &[
            "rev-parse",
            "HEAD",
            "main-worktree/HEAD",
            "worktrees/w2/HEAD",
        ],
        &[
            "rev-parse",
            "--git-dir",
            "--git-common-dir",
            "--git-path",
            "HEAD",
            "--git-path",
            "objects",
            "--git-path",
            "refs/bisect/x",
        ],
        &["symbolic-ref", "HEAD"],
    ] {
        fx.compare(args).in_dir("w1").output_only().run();
    }
    fx.compare(["add", "c"])
        .in_dir("w1")
        .repo("main")
        .skip(Check::Fsck)
        .run();
    let cmp = fx
        .compare(["commit", "-q", "-m", "three"])
        .in_dir("w1")
        .repo("main")
        .skip(Check::Fsck);
    admin_files(cmp, &["w1"]).run();
    fx.compare(["update-ref", "refs/bisect/bad", "HEAD"])
        .in_dir("w1")
        .repo("main")
        .skip(Check::Fsck)
        .run();
    fx.compare(["for-each-ref"])
        .in_dir("w1")
        .output_only()
        .run();
    fx.compare(["for-each-ref"])
        .in_dir("main")
        .output_only()
        .run();
    fx.compare(["reflog", "main-worktree/HEAD"])
        .in_dir("w1")
        .output_only()
        .run();
    fx.compare(["rev-parse", "--git-common-dir"])
        .in_dir("main")
        .output_only()
        .run();
}

/// Branches checked out in another worktree are protected.
#[test]
fn other_worktrees_protect_branches() {
    let cases: &[&[&str]] = &[
        &["switch", "w1"],
        &["switch", "--ignore-other-worktrees", "w1"],
        &["checkout", "w1"],
        &["branch", "-d", "w1"],
        &["branch", "-f", "w1", "HEAD~1"],
        &["branch", "-M", "other", "w1"],
        &["branch", "-m", "w1", "renamed"],
        &["branch"],
    ];
    for args in cases {
        let mut fx = with_worktrees();
        let cmp = fx
            .compare(*args)
            .in_dir("main")
            .repo("main")
            .skip(Check::Fsck);
        admin_files(cmp, &["w1"]).run();
    }
}
