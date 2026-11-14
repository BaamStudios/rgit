// SPDX-License-Identifier: MIT
//! Differential tests of `merge` (#57): `HEAD`, refs, reflogs, the index
//! with its conflict stages, the work tree, the state files (`MERGE_HEAD`,
//! `MERGE_MSG`, `MERGE_MODE`, `SQUASH_MSG`, `AUTO_MERGE`, `ORIG_HEAD`) and
//! every message, against the reference git.

use rgit_difftest::{Check, Comparison, Fixture};

/// `main` changes f (last line) and g on top of `init` (f, g, d/x);
/// `side` changes f (first line) and adds h, then s2 (annotated tag `vs`);
/// `conf` changes g (conflicts with main); `md` deletes g
/// (modify/delete); `ahead` is main plus a commit; `orphan` is unrelated.
/// `main` is checked out.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("f", "1\n2\n3\n4\n5\n")
        .write("g", "g\n")
        .write("d/x", "x\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "init"])
        .git(["tag", "v0"])
        .git(["branch", "side"])
        .git(["branch", "conf"])
        .git(["branch", "md"])
        .tick()
        .write("f", "1\n2\n3\n4\n5x\n")
        .write("g", "main g\n")
        .git(["commit", "-q", "-a", "-m", "main1"])
        .git(["checkout", "-q", "side"])
        .tick()
        .write("f", "1x\n2\n3\n4\n5\n")
        .write("h", "h\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "side1"])
        .tick()
        .write("s2", "s2\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "side2\n\nbody of side2"])
        .git(["tag", "-a", "-m", "tag message", "vs"])
        .git(["update-ref", "refs/remotes/origin/side", "HEAD"])
        .git(["checkout", "-q", "conf"])
        .tick()
        .write("g", "conf g\n")
        .git(["commit", "-q", "-a", "-m", "conf1"])
        .git(["checkout", "-q", "md"])
        .git(["rm", "-q", "g"])
        .git(["commit", "-q", "-m", "md1"])
        .git(["checkout", "-q", "--orphan", "orphan"])
        .git(["rm", "-q", "-r", "-f", "."])
        .write("o", "o\n")
        .git(["add", "o"])
        .git(["commit", "-q", "-m", "orphan1"])
        .git(["checkout", "-q", "-f", "main"])
        .git(["branch", "ahead"])
        .git(["checkout", "-q", "ahead"])
        .tick()
        .write("n", "n\n")
        .git(["add", "n"])
        .git(["commit", "-q", "-m", "ahead1"])
        .git(["checkout", "-q", "main"])
        .tick()
}

fn files(c: Comparison<'_>) -> Comparison<'_> {
    c.git_file("MERGE_MODE")
        .git_file("SQUASH_MSG")
        .git_file("AUTO_MERGE")
}

/// Runs each case in a fresh fixture prepared by `prep`; reports every
/// differing case.
fn cases(prep: impl Fn(Fixture) -> Fixture, cases: &[&[&str]]) {
    cases_skipping(prep, &[], cases);
}

/// [`cases`] without the checks `skip`.
fn cases_skipping(prep: impl Fn(Fixture) -> Fixture, skip: &[Check], cases: &[&[&str]]) {
    let mut failed = Vec::new();
    for args in cases {
        let fx = prep(fixture());
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        let mut c = files(fx.compare(*args));
        for check in skip {
            c = c.skip(*check);
        }
        if let Err(m) = c.try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

#[test]
fn merge_clean() {
    cases(
        |fx| fx,
        &[
            &["merge", "side"],
            &["merge", "-q", "side"],
            &["merge", "-v", "side"],
            &["merge", "-n", "side"],
            &["merge", "--no-stat", "side"],
            &["-c", "merge.stat=false", "merge", "side"],
            &["-c", "merge.stat=false", "merge", "--stat", "side"],
            &["merge", "--no-commit", "side"],
            &["merge", "--no-commit", "--no-ff", "side"],
            &["merge", "--squash", "side"],
            &["merge", "--log", "side"],
            &["merge", "--log=1", "side"],
            &["-c", "merge.log=true", "merge", "side"],
            &["-c", "merge.log=1", "merge", "side"],
            &["merge", "--no-log", "side"],
            &["merge", "-m", "my message", "side"],
            &["merge", "-m", "a", "-m", "b", "--log", "side"],
            &["merge", "--signoff", "side"],
            &["merge", "--into-name", "foo", "side"],
            &["-c", "merge.suppressDest=", "merge", "side"],
            &[
                "-c",
                "merge.suppressDest=ma*",
                "merge",
                "--into-name",
                "mx",
                "side",
            ],
            &["merge", "vs"],
            &["merge", "--log", "vs"],
            &["merge", "side~1"],
            &["merge", "side^"],
            &["merge", "side^0"],
            &["merge", "--log", "side~1"],
            &["merge", "origin/side"],
            &["merge", "refs/heads/side"],
            &["merge", "side", "side"],
            &["merge", "-s", "ours", "side"],
            &["merge", "-s", "recursive", "side"],
            &["merge", "-s", "ort", "side"],
            &["merge", "-s", "foo", "side"],
            &["merge", "-X", "foo", "side"],
            &["merge", "--allow-unrelated-histories", "orphan"],
            &["merge", "orphan"],
            &["merge", "--ff-only", "side"],
            &["-c", "merge.ff=only", "merge", "side"],
            &["-c", "advice.diverging=false", "merge", "--ff-only", "side"],
            &["merge", "--compact-summary", "side"],
            &["-c", "merge.stat=compact", "merge", "side"],
            &["-c", "merge.stat=compact", "merge", "--stat", "side"],
            &["merge", "--cleanup=scissors", "-e", "side"],
        ],
    );
    // A name that exists only as remote-tracking branches.
    cases(
        |fx| {
            fx.git(["update-ref", "refs/remotes/origin/nl", "side"])
                .git(["update-ref", "refs/remotes/up/nl", "side"])
                .git(["update-ref", "refs/remotes/up/other", "side"])
        },
        &[&["merge", "nl"], &["merge", "other"], &["merge", "nothere"]],
    );
    cases(
        |fx| {
            fx.git(["update-ref", "refs/remotes/origin/nl", "side"])
                .git(["update-ref", "refs/heads/origin/nl", "side"])
        },
        &[&["merge", "nl"]],
    );
}

/// `FETCH_HEAD` as written by `fetch`.
#[test]
fn merge_fetch_head() {
    let line = |flag: &str, desc: &str, rev: &str, fx: &Fixture| {
        let id = String::from_utf8(fx.git_stdout(["rev-parse", rev])).unwrap();
        format!("{}\t{flag}\t{desc}\n", id.trim())
    };
    let with = |entries: &'static [(&'static str, &'static str, &'static str)]| {
        move |fx: Fixture| {
            let text: String = entries.iter().map(|(f, d, r)| line(f, d, r, &fx)).collect();
            fx.write(".git/FETCH_HEAD", text)
        }
    };
    cases(
        with(&[("", "branch 'side' of ../x", "side")]),
        &[&["merge", "FETCH_HEAD"], &["merge", "--log", "FETCH_HEAD"]],
    );
    cases(
        with(&[("", "'abc' of ../x", "side")]),
        &[&["merge", "FETCH_HEAD"]],
    );
    cases(with(&[("", "../x", "side")]), &[&["merge", "FETCH_HEAD"]]);
    cases(
        with(&[("not-for-merge", "branch 'side' of ../x", "side")]),
        &[&["merge", "FETCH_HEAD"]],
    );
    cases(|fx| fx, &[&["merge", "FETCH_HEAD"]]);
}

#[test]
fn merge_fast_forward() {
    cases(
        |fx| fx,
        &[
            &["merge", "ahead"],
            &["merge", "-q", "ahead"],
            &["merge", "--ff-only", "ahead"],
            &["merge", "--no-ff", "ahead"],
            &["-c", "merge.ff=false", "merge", "ahead"],
            &["-c", "merge.ff=false", "merge", "--ff", "ahead"],
            &["merge", "--squash", "ahead"],
            &["merge", "--no-commit", "ahead"],
            &["merge", "-n", "ahead"],
            &["merge", "-s", "ours", "ahead"],
            &["merge", "--no-ff", "--no-commit", "ahead"],
        ],
    );
}

#[test]
fn merge_up_to_date_and_errors() {
    cases(
        |fx| fx,
        &[
            &["merge", "main"],
            &["merge", "v0"],
            &["merge", "HEAD"],
            &["merge", "-q", "v0"],
            &["merge", "nope"],
            &["merge"],
            &["merge", "--abort"],
            &["merge", "--continue"],
            &["merge", "--quit"],
            &["merge", "--abort", "side"],
            &["merge", "--continue", "side"],
            &["merge", "--squash", "--no-ff", "side"],
            &["merge", "--squash", "--commit", "side"],
            &["merge", "-h"],
        ],
    );
}

#[test]
fn merge_conflicts() {
    cases(
        |fx| fx,
        &[
            &["merge", "conf"],
            &["merge", "-q", "conf"],
            &["merge", "md"],
            &["merge", "--squash", "conf"],
            &["merge", "--no-commit", "conf"],
            &["merge", "--no-ff", "conf"],
            &["merge", "-m", "custom", "conf"],
            &["-c", "merge.conflictStyle=diff3", "merge", "conf"],
            &["-c", "core.commentChar=;", "merge", "conf"],
            &["merge", "-X", "ours", "conf"],
            &["merge", "-X", "theirs", "conf"],
            &["merge", "-X", "theirs", "md"],
            &["-c", "commit.cleanup=scissors", "merge", "conf"],
            &["-c", "commit.cleanup=scissors", "merge", "--squash", "conf"],
        ],
    );
}

/// What follows a conflicted merge.
#[test]
fn merge_after_conflict() {
    let conflicted = |fx: Fixture| fx.git_output_ok(&["merge", "conf"]);
    cases(
        conflicted,
        &[
            &["merge", "--abort"],
            &["merge", "--quit"],
            &["merge", "--continue"],
            &["merge", "side"],
            &["status"],
            &["commit", "-q", "-a", "-m", "resolved"],
        ],
    );
    let resolved = |fx: Fixture| {
        fx.git_output_ok(&["merge", "conf"])
            .write("g", "resolved\n")
            .git(["add", "g"])
    };
    cases(
        resolved,
        &[
            &["merge", "--continue"],
            &["merge", "side"],
            &["merge", "--abort"],
            &["commit", "--no-edit"],
        ],
    );
    // Local changes survive an abort.
    cases(
        |fx| {
            fx.write("d/x", "local\n")
                .write("u", "untracked\n")
                .git_output_ok(&["merge", "conf"])
        },
        &[&["merge", "--abort"]],
    );
    // Committing a squash.
    cases(
        |fx| fx.git(["merge", "-q", "--squash", "side"]),
        &[&["commit", "--no-edit"], &["commit", "-m", "given"]],
    );
    cases(
        |fx| {
            fx.git_output_ok(&["merge", "--squash", "conf"])
                .write("g", "resolved\n")
                .git(["add", "g"])
        },
        &[&["commit", "--no-edit"]],
    );
    cases(
        |fx| fx.git(["merge", "-q", "--no-commit", "side"]),
        &[
            &["merge", "--abort"],
            &["merge", "--continue"],
            &["merge", "side"],
        ],
    );
}

/// Local changes and untracked files.
#[test]
fn merge_local_changes() {
    // Staged changes refuse a true merge. git also records internal
    // commits and a reflog entry "updating HEAD" on the way, which rgit
    // does not.
    let staged = || fixture().write("d/x", "staged\n").git(["add", "d/x"]);
    let mut fx = staged();
    if fx.is_skipped() {
        return;
    }
    files(fx.compare(["merge", "side"]))
        .skip(Check::Objects)
        .skip(Check::Reflogs)
        .run();
    cases(|_| staged(), &[&["merge", "ahead"]]);
    // A dirty file the merge does not touch is kept. (git stores the
    // local changes in internal commits on the way.)
    cases_skipping(
        |fx| fx.write("d/x", "dirty\n"),
        &[Check::Objects],
        &[&["merge", "side"], &["merge", "ahead"], &["merge", "conf"]],
    );
    // A dirty file the merge changes refuses it.
    cases_skipping(
        |fx| fx.write("f", "dirty\n"),
        &[Check::Objects],
        &[&["merge", "side"], &["merge", "ahead"]],
    );
    // Untracked files in the way.
    cases(
        |fx| fx.write("h", "untracked\n").write("n", "untracked\n"),
        &[&["merge", "side"], &["merge", "ahead"]],
    );
}

/// Merging into an unborn branch, from a detached HEAD, with an upstream.
#[test]
fn merge_special_heads() {
    cases(
        |fx| {
            fx.git(["checkout", "-q", "--orphan", "new"])
                .git(["rm", "-q", "-r", "-f", "."])
        },
        &[
            &["merge", "side"],
            &["merge", "side", "conf"],
            &["merge", "--squash", "side"],
        ],
    );
    cases(
        |fx| fx.git(["checkout", "-q", "--detach"]),
        &[&["merge", "side"], &["merge", "ahead"], &["merge"]],
    );
    cases(
        |fx| {
            fx.git(["config", "branch.main.remote", "."]).git([
                "config",
                "branch.main.merge",
                "refs/heads/side",
            ])
        },
        &[
            &["merge"],
            &["-c", "merge.defaultToUpstream=false", "merge"],
        ],
    );
    cases(
        |fx| fx.git(["config", "branch.main.remote", "origin"]),
        &[&["merge"]],
    );
    cases(
        |fx| fx.git(["config", "branch.main.mergeOptions", "--no-ff -q"]),
        &[&["merge", "ahead"]],
    );
}

/// The merge hooks.
#[test]
fn merge_hooks() {
    let hooks = |fx: Fixture| {
        let mut fx = fx;
        for h in [
            "pre-merge-commit",
            "prepare-commit-msg",
            "commit-msg",
            "post-merge",
        ] {
            fx = fx.write_executable(
                format!(".git/hooks/{h}"),
                format!(
                    "#!/bin/sh\necho \"{h} $* [$GIT_REFLOG_ACTION] [$GIT_EDITOR] [$GIT_INDEX_FILE]\"\n"
                ),
            );
        }
        fx
    };
    cases(
        hooks,
        &[
            &["merge", "side"],
            &["merge", "--no-verify", "side"],
            &["merge", "ahead"],
            &["merge", "--squash", "side"],
            &["merge", "conf"],
        ],
    );
    let failing = |fx: Fixture| {
        hooks(fx).write_executable(
            ".git/hooks/pre-merge-commit",
            "#!/bin/sh\necho nope\nexit 1\n",
        )
    };
    cases(
        failing,
        &[&["merge", "side"], &["merge", "--no-verify", "side"]],
    );
    let editor = |fx: Fixture| {
        fx.write_executable(
            ".git/ed.sh",
            "#!/bin/sh\ncat \"$1\" >&2\necho edited >> \"$1\"\n",
        )
        .write_executable(".git/empty.sh", "#!/bin/sh\n: > \"$1\"\n")
    };
    cases(
        editor,
        &[
            &["-c", "core.editor=.git/ed.sh", "merge", "-e", "side"],
            &[
                "-c",
                "core.editor=.git/ed.sh",
                "merge",
                "-e",
                "-m",
                "given",
                "side",
            ],
            &["-c", "core.editor=.git/empty.sh", "merge", "-e", "side"],
            &["-c", "core.editor=.git/ed.sh", "merge", "--no-edit", "side"],
        ],
    );
}

trait FixtureExt {
    fn git_output_ok(self, args: &[&str]) -> Fixture;
}

impl FixtureExt for Fixture {
    /// Runs a reference git command that may fail (a conflicted merge).
    fn git_output_ok(self, args: &[&str]) -> Fixture {
        let _ = self.git_output(args.iter().copied());
        self
    }
}
