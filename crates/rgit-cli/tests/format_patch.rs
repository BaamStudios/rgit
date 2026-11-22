// SPDX-License-Identifier: MIT
//! Differential tests of `rgit format-patch` against the reference git:
//! mails on standard output and in files, numbering, subject prefixes,
//! encoded and folded headers, recipients, threading, sign-offs and the
//! cover letter.

use rgit_difftest::{Check, Fixture, Normalizer};

fn commit(fx: Fixture, msg: &str) -> Fixture {
    fx.git(["commit", "-q", "--allow-empty", "-m", msg]).tick()
}

/// The default signature is the version, which differs by design; the
/// time in message ids differs between the two runs.
fn volatile() -> Normalizer {
    let version = regex::Regex::new(r"(?m)^-- \n[^\n]*$").unwrap();
    let time = regex::Regex::new(r"\.[0-9]+\.git\.").unwrap();
    Normalizer::new("signature version and message-id time", move |s| {
        let s = version.replace_all(s, "-- \n<VERSION>");
        time.replace_all(&s, ".<TIME>.git.").into_owned()
    })
}

/// A linear history with file changes, a rename, a mode change, a long
/// subject and a body, then a branch with odd subjects and authors.
fn history() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("f", "a\nb\nc\n")
        .git(["add", "f"]);
    fx = commit(fx, "initial");
    fx = fx
        .write("f", "a\nB\nc\n")
        .write("g", "new\n")
        .git(["add", "."]);
    fx = commit(fx, "second change\n\nbody line one\nbody two   \n\n\n");
    fx = fx.git(["mv", "g", "h"]);
    fx = commit(
        fx,
        "Rename g to h with a pretty long subject line that goes on and on and on and on",
    );
    fx = fx
        .git(["branch", "side"])
        .write_executable("f", "a\nB\nc\n")
        .git(["add", "f"]);
    fx = commit(fx, "make f executable");
    fx = fx.git(["checkout", "-q", "side"]);
    for (author, msg) in [
        (
            "Ünï Cödé <u@example.com>",
            "Sübject with ümlauts that is fairly long so it needs folding into lines",
        ),
        (
            "A. U. \"Q\" (x) Thor <q@example.com>",
            "plain: with . and [brackets] and =?x?= in it",
        ),
        (
            "Foo Bar Foo Bar Foo Bar Foo Bar Foo Bar Foo Bar Foo Bar Foo Bar Foo Bar Foo <l@example.com>",
            "long author",
        ),
        (
            "A U Thor <author@example.com>",
            "  [foo] Fix: a..b_c  -- x.y/z ÄÖ é!!",
        ),
        (
            "A U Thor <author@example.com>",
            "yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy",
        ),
    ] {
        fx = fx
            .git([
                "commit",
                "-q",
                "--allow-empty",
                "--author",
                author,
                "-m",
                msg,
            ])
            .tick();
    }
    fx = fx.write("s", "x\n").git(["add", "s"]);
    fx = commit(
        fx,
        "one\ntwo three\n\nbödy\n\nSigned-off-by: Someone <s@example.com>",
    );
    fx.git(["checkout", "-q", "main"])
}

fn same(fx: &mut Fixture, args: &[&str]) {
    let mut argv = vec!["format-patch"];
    argv.extend_from_slice(args);
    fx.compare(&argv)
        .checks(Check::OUTPUT)
        .normalize_stdout(volatile())
        .run();
}

#[test]
fn series_on_standard_output() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["--stdout", "--root", "main"][..],
        &["--stdout", "-2"],
        &["--stdout", "-1", "main~1"],
        &["--stdout", "main~2"],
        &["--stdout", "main~3..main"],
        &["--stdout", "main..side"],
        &["--stdout", "-p", "-2"],
        &["--stdout", "--stat", "-2"],
        &["--stdout", "--numstat", "-2"],
        &["--stdout", "--raw", "-2"],
        &["--stdout", "--no-renames", "-2"],
        &["--stdout", "-U1", "-3"],
        &["--stdout", "--root", "main", "--", "f"],
        &["--stdout", "-1", "--", "h"],
        &["--stdout", "-k", "-2"],
        &["--stdout", "-n", "-1"],
        &["--stdout", "-N", "-3"],
        &["--stdout", "--start-number=7", "-3"],
        &["--stdout", "--rfc", "-v3", "--subject-prefix=FOO", "-2"],
        &["--stdout", "--rfc=-WIP", "-1"],
        &["--stdout", "--subject-prefix=", "-1"],
        &["--stdout", "--subject-prefix=", "-n", "-1"],
        &["--stdout", "--zero-commit", "-2"],
        &["--stdout", "--signature=my sig", "-2"],
        &["--stdout", "--no-signature", "-2"],
        &["--stdout", "--signature=", "-1"],
        &["--stdout"],
        &[],
    ] {
        same(&mut fx, args);
    }
}

#[test]
fn headers() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["--stdout", "main..side"][..],
        &["--stdout", "--no-encode-email-headers", "main..side"],
        &["--stdout", "-s", "main..side"],
        &["--stdout", "--from", "-2", "side"],
        &["--stdout", "--from=Me <me@example.com>", "-2", "side"],
        &[
            "--stdout",
            "--from=A U Thor <author@example.com>",
            "-1",
            "side~1",
        ],
        &[
            "--stdout",
            "--force-in-body-from",
            "--from=A U Thor <author@example.com>",
            "-1",
            "side~1",
        ],
        &[
            "--stdout",
            "--to=a@example.com",
            "--to=B <b@example.com>",
            "--cc=c@example.com",
            "--add-header=X-Foo: bar",
            "--add-header=Cc: d@example.com",
            "-2",
        ],
        &[
            "--stdout",
            "--to=x@example.com",
            "--no-to",
            "--to=y@example.com",
            "-1",
        ],
        &["--stdout", "--in-reply-to=foo@bar", "-3"],
        &["--stdout", "--thread", "-3"],
        &["--stdout", "--thread=deep", "-3"],
        &["--stdout", "--thread", "--in-reply-to=<foo@bar>", "-3"],
        &[
            "--stdout",
            "--thread=deep",
            "--in-reply-to=foo@bar",
            "--cover-letter",
            "-3",
        ],
        &["--stdout", "--thread", "--cover-letter", "-3"],
        &["--stdout", "--pretty=mboxrd", "-1", "side"],
    ] {
        same(&mut fx, args);
    }
    fx = fx
        .git([
            "config",
            "format.headers",
            "Organization: rgit\nTo: t@example.com\n",
        ])
        .git(["config", "format.cc", "C <c@example.com>"])
        .git(["config", "format.subjectPrefix", "PATCH rgit"])
        .git(["config", "format.signature", "configured"])
        .git(["config", "format.thread", "deep"])
        .git(["config", "format.numbered", "auto"])
        .git(["config", "format.from", "F R Om <from@example.com>"]);
    for args in [
        &["--stdout", "-2"][..],
        &["--stdout", "--no-add-header", "--no-thread", "-1"],
        &["--stdout", "--no-cc", "--no-from", "-n", "-1"],
        &["--stdout", "--rfc", "-1"],
    ] {
        same(&mut fx, args);
    }
}

#[test]
fn cover_letter() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["--stdout", "--cover-letter", "-3"][..],
        &["--stdout", "--cover-letter", "--root", "main"],
        &["--stdout", "--cover-letter", "main..side"],
        &["--stdout", "--cover-letter", "-N", "-1"],
        &["--stdout", "--cover-letter", "--zero-commit", "-v2", "-2"],
        &[
            "--stdout",
            "--cover-letter",
            "--from=X Y <x@example.com>",
            "-2",
        ],
    ] {
        same(&mut fx, args);
    }
    fx = fx.git([
        "config",
        "branch.main.description",
        "Description subject\ncontinued\n\nAnd its body.\n",
    ]);
    for mode in ["message", "subject", "auto", "none"] {
        same(
            &mut fx,
            &[
                "--stdout",
                "--cover-letter",
                &format!("--cover-from-description={mode}"),
                "-2",
            ],
        );
    }
}

#[test]
fn files() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["-o", "out", "--signature=s", "-3"][..],
        &["-o", "out/", "--signature=s", "--cover-letter", "-v2", "-2"],
        &[
            "-o",
            "out",
            "--signature=s",
            "--numbered-files",
            "--cover-letter",
            "--start-number=3",
            "-2",
        ],
        &[
            "-o",
            "out",
            "--signature=s",
            "--suffix=.txt",
            "--filename-max-length=20",
            "-3",
        ],
        &["--signature=s", "--suffix=", "main..side"],
        &["-q", "-o", "out", "--signature=s", "-2"],
        &["--output=all.mbox", "--signature=s", "-3"],
    ] {
        let mut argv = vec!["format-patch"];
        argv.extend_from_slice(args);
        fx.compare(&argv)
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
fn errors() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["--stdout", "-n", "-k", "-1"][..],
        &["--stdout", "-k", "--rfc", "-1"],
        &["--stdout", "-k", "--subject-prefix=X", "-1"],
        &["--stdout", "--name-only", "-1"],
        &["--stdout", "--name-status", "-1"],
        &["--stdout", "-o", "x", "-1"],
        &["--stdout", "--output=x", "-1"],
        &["--stdout", "--from=foo", "-1"],
        &[
            "--stdout",
            "--cover-letter",
            "--cover-from-description=bad",
            "-1",
        ],
        &["--stdout", "--start-number=x", "-1"],
        &["--stdout", "nonexistent"],
    ] {
        same(&mut fx, args);
    }
}
