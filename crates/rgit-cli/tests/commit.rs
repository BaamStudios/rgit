// SPDX-License-Identifier: MIT
//! `commit` against the reference git: commit ids, refs, reflogs, index,
//! `COMMIT_EDITMSG`, the summary, the "nothing to commit" status and the
//! message template the editor sees. Dates are fixed by the fixture.

use rgit_difftest::{Check, Fixture};

/// An editor that shows the template on stderr and writes `edited`.
const EDITOR: &str = "f() { cat \"$1\" >&2; printf 'edited\\n\\n# gone\\n' >\"$1\"; }; f";

fn base() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
}

/// Two commits on `main`, plus a staged and an unstaged change.
fn history() -> Fixture {
    base()
        .write("a", "1\n2\n3\n")
        .write("b", "b\n")
        .git(["add", "a", "b"])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .write("a", "1\n2\n3\n4\n")
        .git(["commit", "-q", "-a", "-m", "two"])
        .tick()
}

fn all_checks() -> Vec<Check> {
    vec![
        Check::ExitCode,
        Check::Stdout,
        Check::Stderr,
        Check::Refs,
        Check::Reflogs,
        Check::Objects,
        Check::Index,
        Check::Worktree,
    ]
}

fn check(mut fx: Fixture, args: &[&str]) {
    if fx.is_skipped() {
        return;
    }
    fx.compare(args)
        .checks(all_checks())
        .git_file("COMMIT_EDITMSG")
        .run();
}

fn check_env(mut fx: Fixture, args: &[&str], env: &[(&str, &str)]) {
    if fx.is_skipped() {
        return;
    }
    let mut c = fx
        .compare(args)
        .checks(all_checks())
        .git_file("COMMIT_EDITMSG");
    for (k, v) in env {
        c = c.env(k, v);
    }
    c.run();
}

#[test]
fn root_commit() {
    let fx = base()
        .write("a", "a\n")
        .write("d/b", "b\n")
        .git(["add", "."]);
    check(fx, &["commit", "-m", "first\nline two\n\nbody"]);
    let fx = base().write("a", "a\n").git(["add", "."]);
    check(fx, &["commit", "-q", "-m", "first"]);
}

#[test]
fn messages() {
    let cases: &[&[&str]] = &[
        &["commit", "-m", "a", "-m", "b  \n\n\n c"],
        &["commit", "-m", ""],
        &["commit", "-m", "  ", "--allow-empty-message"],
        &["commit", "-m", "#x"],
        &["commit", "-m", "Signed-off-by: x"],
        &["commit", "-m", "x", "-F", "a"],
        &["commit", "-F", "missing"],
        &["commit", "-F", "msg"],
        &["commit", "-F", "-"],
        &["commit", "--cleanup=verbatim", "-m", "  x  \n\n\n#c"],
        &["commit", "--cleanup=strip", "-m", "#c\nx"],
        &["commit", "--cleanup=bogus", "-m", "x"],
        &["-c", "commit.cleanup=strip", "commit", "-m", "#c\nx"],
    ];
    for args in cases {
        let fx = history()
            .write("msg", "from file\n\n\nbody  \n")
            .write("b", "changed\n")
            .git(["add", "b"]);
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        fx.compare(*args)
            .checks(all_checks())
            .git_file("COMMIT_EDITMSG")
            .stdin("stdin message\n")
            .run();
    }
}

#[test]
fn nothing_to_commit() {
    check(history(), &["commit", "-m", "x"]);
    check(history().write("u", "u\n"), &["commit", "-m", "x"]);
    check(
        history().write("a", "x\n").write("u", "u\n"),
        &["commit", "-m", "x"],
    );
    check(history().remove("b"), &["commit", "-m", "x"]);
    check(history().write("u", "u\n"), &["commit", "-uno", "-m", "x"]);
    check(history(), &["commit", "-uno", "-m", "x"]);
    check(history().write("d/e/u", "u\n"), &["commit", "-m", "x"]);
    check(
        history().write("d/e/u", "u\n"),
        &["commit", "-uall", "-m", "x"],
    );
    check(
        history().write("a", "x\n").write("u", "u\n"),
        &["-c", "advice.statusHints=false", "commit", "-m", "x"],
    );
    check(
        history(),
        &["-c", "advice.statusHints=false", "commit", "-m", "x"],
    );
    check(
        history().write("u", "u\n"),
        &["-c", "status.showUntrackedFiles=no", "commit", "-m", "x"],
    );
    check(base(), &["commit", "-m", "x"]);
    check(base().write("u", "u\n"), &["commit", "-m", "x"]);
    check(history(), &["commit", "--amend", "-m", "x"]);
    check(
        history()
            .git(["commit", "-q", "--allow-empty", "-m", "empty"])
            .write("u", "u\n"),
        &["commit", "--amend", "-m", "x"],
    );
    check(history(), &["commit", "-u", "-m", "x"]);
    check(
        history()
            .write("sp ace", "s\n")
            .write("ü", "u\n")
            .write("q\"uote", "q\n"),
        &["commit", "-m", "x"],
    );
    check(
        history().write("ü", "u\n"),
        &["-c", "core.quotePath=false", "commit", "-m", "x"],
    );
    check(history(), &["commit", "-ubad", "-m", "x"]);
}

/// Run in a subdirectory (helper so the case list stays flat).
trait InDir {
    fn in_dir_fixture(self, dir: &str) -> SubDir;
}

struct SubDir(Fixture, String);

impl InDir for Fixture {
    fn in_dir_fixture(self, dir: &str) -> SubDir {
        SubDir(self, dir.to_owned())
    }
}

fn check_in(sub: SubDir, args: &[&str]) {
    let SubDir(mut fx, dir) = sub;
    if fx.is_skipped() {
        return;
    }
    fx.compare(args).in_dir(dir).checks(all_checks()).run();
}

#[test]
fn subdirectory() {
    check_in(
        history()
            .write("d/u", "u\n")
            .write("a", "y\n")
            .in_dir_fixture("d"),
        &["commit", "-m", "x"],
    );
    check_in(
        history()
            .write("d/f", "f\n")
            .git(["add", "d/f"])
            .write("d/msg", "m\n")
            .in_dir_fixture("d"),
        &["commit", "-F", "msg"],
    );
    check_in(
        history()
            .write("d/f", "f\n")
            .git(["add", "d/f"])
            .write("a", "z\n")
            .in_dir_fixture("d"),
        &["commit", "-m", "x", "../a"],
    );
}

#[test]
fn linked_worktree() {
    // HEAD and its reflog live in the worktree's git directory.
    check_in(
        history()
            .git(["worktree", "add", "-q", "-b", "branch", "wt"])
            .in_dir_fixture("wt"),
        &["commit", "--allow-empty", "-m", "in worktree"],
    );
    check_in(
        history()
            .git(["worktree", "add", "-q", "--detach", "wt"])
            .in_dir_fixture("wt"),
        &["commit", "--allow-empty", "-m", "detached in worktree"],
    );
}

#[test]
fn all_and_paths() {
    let cases: &[&[&str]] = &[
        &["commit", "-a", "-m", "x"],
        &["commit", "-m", "x", "a"],
        &["commit", "-o", "-m", "x", "a"],
        &["commit", "--only", "-m", "x", "a"],
        &["commit", "-i", "-m", "x", "a"],
        &["commit", "-m", "x", "b"],
        &["commit", "-m", "x", "nothere"],
        &["commit", "-m", "x", "u"],
        &["commit", "-i", "-m", "x", "u"],
        &["commit", "-m", "x", "*"],
        &["commit", "-a", "-m", "x", "a"],
        &["commit", "-i", "-m", "x"],
        &["commit", "-o", "-m", "x"],
        &["commit", "-i", "-o", "-m", "x", "a"],
        &["commit", "-a", "-i", "-m", "x", "a"],
        &["commit", "-a", "-o", "-m", "x", "a"],
        &["commit", "--allow-empty", "--only", "-m", "x"],
        &["commit", "--amend", "--only", "--no-edit"],
        &["commit", "--amend", "-m", "x", "a"],
    ];
    for args in cases {
        // a: modified in the work tree; c: staged new file; b: deleted.
        let fx = history()
            .write("a", "1\n2\n3\n4\n5\n")
            .write("c", "c\n")
            .git(["add", "c"])
            .remove("b")
            .write("u", "u\n");
        check(fx, args);
    }
    // Removal through a path commit.
    let fx = history().git(["rm", "-q", "--cached", "b"]).remove("b");
    check(fx, &["commit", "-m", "x", "b"]);
    let fx = history().git(["rm", "-q", "--cached", "b"]);
    check(fx, &["commit", "-m", "x", "b"]);
}

#[test]
fn amend_author_date() {
    let cases: &[&[&str]] = &[
        &["commit", "--amend", "-m", "amended"],
        &["commit", "--amend", "--no-edit"],
        &["commit", "--amend", "--no-edit", "--reset-author"],
        &["commit", "--amend", "--allow-empty", "-m", "x"],
        &["commit", "--amend", "--author=Q <q@q>", "-m", "x"],
        &["commit", "--amend", "--date=@1234567890 +0000", "-m", "x"],
        &["commit", "--author=Zed <z@z>", "-m", "x"],
        &["commit", "--author=C O Mitter", "-m", "x"],
        &["commit", "--author=nobody", "-m", "x"],
        &["commit", "--date=2005-04-07 22:13:13 +0200", "-m", "x"],
        &["commit", "--date=garbage", "-m", "x"],
        &["commit", "--reset-author", "-m", "x"],
        &["commit", "--reset-author", "--author=x <y>", "-m", "x"],
        &["commit", "--allow-empty", "-m", "empty"],
        &["commit", "-q", "--allow-empty", "-m", "empty"],
    ];
    for args in cases {
        let fx = history().write("c", "c\n").git(["add", "c"]);
        check(fx, args);
    }
    let fx = base();
    check(fx, &["commit", "--amend", "-m", "x"]);
    // Amending the root commit.
    let fx = base()
        .write("a", "a\n")
        .git(["add", "a"])
        .git(["commit", "-q", "-m", "r"])
        .tick();
    check(fx, &["commit", "--amend", "-m", "root again"]);
}

#[test]
fn detached_and_tracking() {
    let fx = history()
        .git(["checkout", "-q", "--detach", "HEAD~1"])
        .write("c", "c\n")
        .git(["add", "c"]);
    check(fx, &["commit", "-m", "on detached"]);
    let fx = history().git(["checkout", "-q", "--detach", "HEAD~1"]);
    check(fx, &["commit", "-m", "x"]);
    let fx = history()
        .git(["checkout", "-q", "--detach", "HEAD~1"])
        .git(["commit", "-q", "--allow-empty", "-m", "moved"]);
    check(fx, &["commit", "-m", "x"]);
    let fx = history().git(["checkout", "-q", "--detach", "main"]);
    check(fx, &["commit", "-m", "x"]);
    // Upstream states: up to date, ahead, behind, diverged, gone.
    let up = |fx: Fixture| {
        fx.git(["config", "branch.main.remote", "origin"])
            .git(["config", "branch.main.merge", "refs/heads/main"])
            .git(["config", "remote.origin.url", "/nonexistent"])
            .git([
                "config",
                "remote.origin.fetch",
                "+refs/heads/*:refs/remotes/origin/*",
            ])
    };
    // A remote without fetch refspecs gives no upstream.
    check(
        history()
            .git(["config", "branch.main.remote", "origin"])
            .git(["config", "branch.main.merge", "refs/heads/main"])
            .git(["update-ref", "refs/remotes/origin/main", "HEAD"]),
        &["commit", "-m", "x"],
    );
    // A local upstream.
    check(
        history()
            .git(["branch", "base", "HEAD~1"])
            .git(["config", "branch.main.remote", "."])
            .git(["config", "branch.main.merge", "refs/heads/base"]),
        &["commit", "-m", "x"],
    );
    check(
        up(history()).git(["update-ref", "refs/remotes/origin/main", "HEAD"]),
        &["commit", "-m", "x"],
    );
    check(
        up(history()).git(["update-ref", "refs/remotes/origin/main", "HEAD~1"]),
        &["commit", "-m", "x"],
    );
    check(
        up(history()).git(["update-ref", "refs/remotes/origin/main", "HEAD~1"]),
        &["-c", "advice.statusHints=false", "commit", "-m", "x"],
    );
    let fx = up(history())
        .git(["update-ref", "refs/remotes/origin/main", "HEAD"])
        .git(["reset", "-q", "--soft", "HEAD~1"])
        .git(["stash", "-q"]);
    check(fx, &["commit", "-m", "x"]);
    let fx = up(history())
        .git(["checkout", "-q", "-b", "side", "HEAD~1"])
        .git(["commit", "-q", "--allow-empty", "-m", "side"])
        .git(["update-ref", "refs/remotes/origin/main", "side"])
        .git(["checkout", "-q", "main"]);
    check(fx, &["commit", "-m", "x"]);
    check(up(history()), &["commit", "-m", "x"]);
    let fx = up(history())
        .git(["update-ref", "refs/remotes/origin/main", "HEAD~1"])
        .write("c", "c\n")
        .git(["add", "c"]);
    check_env(fx, &["commit"], &[("GIT_EDITOR", EDITOR)]);
}

#[test]
fn summary_lines() {
    // Mode change, rename, binary change, deletion, symlink, many files.
    let fx = history()
        .write("bin", b"x\0y".as_slice())
        .write("dir/x", "x\n")
        .write("dir/y", "y\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "more"])
        .tick()
        .git(["mv", "b", "b2"])
        .git(["mv", "dir/x", "dir/x2"])
        .write("other/keep", "k\n")
        .git(["add", "other/keep"])
        .git(["mv", "dir/y", "other/y"])
        .write("bin", b"x\0yz".as_slice())
        .git(["update-index", "--chmod=+x", "a"])
        .git(["add", "bin"])
        .symlink("a", "link")
        .git(["add", "link"]);
    check(fx, &["commit", "-m", "x"]);
    let fx = history().git(["update-index", "--chmod=+x", "a"]);
    check(fx, &["commit", "-m", "mode only"]);
    let fx = history().git(["rm", "-q", "a"]);
    check(fx, &["commit", "-m", "delete"]);
    let fx = history()
        .write("sp ace", "s\n")
        .write("tab\there", "t\n")
        .git(["add", "."]);
    check(fx, &["commit", "-m", "names"]);
    let fx = history().git(["mv", "a", "a2"]);
    check(fx, &["-c", "diff.renames=false", "commit", "-m", "x"]);
}

#[test]
fn editor() {
    let cases: &[&[&str]] = &[
        &["commit"],
        &["commit", "-e", "-m", "given"],
        &["commit", "--amend"],
        &["commit", "--amend", "--author=Q <q@q>"],
        &["commit", "--no-status"],
        &["commit", "--cleanup=whitespace"],
        &["commit", "--cleanup=verbatim"],
        &["commit", "--cleanup=scissors"],
        &["-c", "core.commentChar=;", "commit"],
        &["-c", "core.commentChar=auto", "commit", "-e", "-m", "#hash"],
        &["-c", "commit.status=false", "commit"],
        &["commit", "--amend", "--reset-author"],
        &["commit", "-uno"],
        &["commit", "--amend", "--no-edit"],
        &["commit", "-a"],
        &["commit", "-m", "x", "--no-edit"],
    ];
    for args in cases {
        let fx = history()
            .write("c", "c\n")
            .git(["add", "c"])
            .write("a", "changed\n")
            .write("u", "u\n")
            .remove("b");
        check_env(fx, args, &[("GIT_EDITOR", EDITOR)]);
    }
    // Initial commit and an editor that fails or empties the message.
    let fx = base().write("a", "a\n").git(["add", "a"]);
    check_env(fx, &["commit"], &[("GIT_EDITOR", EDITOR)]);
    let fx = history().write("c", "c\n").git(["add", "c"]);
    check_env(fx, &["commit"], &[("GIT_EDITOR", "false")]);
    let fx = history().write("c", "c\n").git(["add", "c"]);
    check_env(fx, &["commit"], &[("GIT_EDITOR", "f() { : >\"$1\"; }; f")]);
    let fx = history().write("c", "c\n").git(["add", "c"]);
    check_env(fx, &["commit"], &[("GIT_EDITOR", ":")]);
}

#[test]
fn hooks_warn() {
    // rgit does not run hooks yet and says so (decision on #45); git runs
    // them. Only the commit itself is compared.
    let mut fx = history()
        .write("c", "c\n")
        .git(["add", "c"])
        .write_executable(".git/hooks/post-commit", "#!/bin/sh\nexit 0\n");
    if fx.is_skipped() {
        return;
    }
    fx.compare(["commit", "-m", "x"])
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Refs,
            Check::Reflogs,
            Check::Objects,
        ])
        .run();
}

#[test]
fn conclude_merge() {
    let merge = || {
        history()
            .git(["checkout", "-q", "-b", "side", "HEAD~1"])
            .write("s", "s\n")
            .git(["add", "s"])
            .git(["commit", "-q", "-m", "side"])
            .git(["checkout", "-q", "main"])
            .git(["merge", "-q", "--no-commit", "side"])
    };
    check(merge(), &["commit", "-m", "merged"]);
    check(merge(), &["commit", "-m", "x", "a"]);
    check(merge(), &["commit", "--amend", "-m", "x"]);
    check(merge().write("u", "u\n"), &["commit", "-a", "-m", "merged"]);
    check_env(merge(), &["commit"], &[("GIT_EDITOR", EDITOR)]);
    check_env(merge(), &["commit", "--no-edit"], &[("GIT_EDITOR", EDITOR)]);
}

#[test]
fn unmerged_index() {
    let mut fx = history()
        .git(["checkout", "-q", "-b", "side", "HEAD~1"])
        .write("a", "side\n")
        .git(["commit", "-q", "-a", "-m", "side"])
        .git(["checkout", "-q", "main"]);
    if fx.is_skipped() {
        return;
    }
    let _ = fx.git_output(["merge", "-q", "side"]);
    let _ = fx.git_output(["rm", "-q", "-f", ".git/MERGE_HEAD"]);
    std::fs::remove_file(fx.work_dir().join(".git/MERGE_HEAD")).ok();
    fx.compare(["commit", "-m", "x"])
        .checks([Check::ExitCode, Check::Stdout, Check::Stderr, Check::Refs])
        .run();
}
