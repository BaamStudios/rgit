// SPDX-License-Identifier: MIT
//! Differential tests of commit message encodings (#182) and related
//! format details (#405, #208) against the reference git: the encoding
//! header, the "did not conform to UTF-8" warning and the repair of such
//! messages, NUL bytes, re-encoding in `log` (`i18n.logOutputEncoding`,
//! `--encoding`), `blame`, the sequencer, non-UTF-8 command-line
//! arguments, `%(describe)`, `%>|(<column>)` after `--graph` and POSIX
//! `TZ` values without rules.

use std::ffi::OsString;

use rgit_difftest::{Check, Fixture};

const STATE: [Check; 4] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Objects,
];

#[cfg(unix)]
fn raw(b: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(b.to_vec())
}

fn repo() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("f", "a\n")
        .git(["add", "f"])
}

/// Commits in ISO-8859-1, EUC-JP and UTF-8 (one invalid).
fn history() -> Fixture {
    repo()
        .write("latin1", b"caf\xe9 latin\n\nbody \xe9\n")
        .write("eucjp", b"\xa4\xcf\xa4\xec\xa4\xd2\n")
        .write("utf8", "utf \u{e9}\n")
        .git([
            "-c",
            "i18n.commitEncoding=ISO8859-1",
            "commit",
            "-q",
            "-F",
            "latin1",
        ])
        .tick()
        .write("f", "b\n")
        .git([
            "-c",
            "i18n.commitEncoding=eucJP",
            "commit",
            "-q",
            "-a",
            "-F",
            "eucjp",
        ])
        .tick()
        .write("f", "c\n")
        .git(["commit", "-q", "-a", "-F", "utf8"])
}

#[test]
fn commit_messages_that_are_not_utf8() {
    let mut fx = repo()
        .write(
            "bad",
            b"bad \xff and \xc3\xa9\n\nbody \xed\xa0\x80 \xef\xb7\x90\n",
        )
        .write("nul", b"a\0b\n");
    fx.compare(["commit", "-q", "-F", "bad"])
        .checks(STATE)
        .run();
    fx.compare(["commit", "-q", "-F", "nul"])
        .checks(STATE)
        .run();
    fx.compare([
        "-c",
        "i18n.commitEncoding=latin1",
        "commit",
        "-q",
        "-F",
        "bad",
    ])
    .checks(STATE)
    .run();
    let mut fx = fx.git(["commit", "-q", "--allow-empty", "-m", "x"]);
    let tree = String::from_utf8(fx.git_stdout(["write-tree"])).unwrap();
    let tree = tree.trim();
    fx.compare(["commit-tree", tree, "-F", "bad"])
        .checks(STATE)
        .run();
    fx.compare(["commit-tree", tree, "-F", "nul"])
        .checks(STATE)
        .run();
}

#[cfg(unix)]
#[test]
fn arguments_that_are_not_utf8() {
    let mut fx = repo();
    let msg = raw(b"caf\xe9");
    let author = raw(b"J\xf6rg <j@example.com>");
    let args: Vec<OsString> = vec![
        "-c".into(),
        "i18n.commitEncoding=ISO-8859-1".into(),
        "commit".into(),
        "-q".into(),
        "--author".into(),
        author,
        "-m".into(),
        msg,
    ];
    fx.compare(&args).checks(STATE).run();
}

#[test]
fn log_reencodes() {
    let mut fx = history();
    for args in [
        &["log", "--format=%s|%e|%b"][..],
        &["log", "--encoding=ISO-8859-1", "--format=%s"],
        &["log", "--encoding=none", "--format=%s"],
        &["log", "--encoding=none"],
        &["log", "--encoding=foo", "--format=%s"],
        &["log", "--encoding=foo"],
        &["log", "--encoding=UTF-8", "--format=%s"],
        &[
            "-c",
            "i18n.logOutputEncoding=ISO8859-1",
            "log",
            "--format=%B",
        ],
        &["-c", "i18n.commitEncoding=latin1", "log", "--format=%s"],
        &["log", "--pretty=raw"],
        &["rev-list", "--format=%s", "HEAD"],
        &["rev-list", "--encoding=latin1", "--format=%s", "HEAD"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}

#[test]
fn blame_reencodes() {
    let mut fx = history();
    for args in [
        &["blame", "--porcelain", "f"][..],
        &["blame", "--incremental", "--encoding=ISO-8859-1", "f"],
        &["blame", "--incremental", "--encoding=none", "f"],
        &[
            "-c",
            "i18n.logOutputEncoding=latin1",
            "blame",
            "--incremental",
            "f",
        ],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}

#[test]
fn rebase_converts_to_the_commit_encoding() {
    let mut fx = history()
        .git(["branch", "side", "HEAD~2"])
        .write("g", "g\n")
        .git(["add", "g"])
        .git(["commit", "-q", "-m", "g"]);
    fx.compare(["rebase", "-q", "--onto", "side", "HEAD~3"])
        .checks([Check::ExitCode, Check::Stdout, Check::Stderr, Check::Refs])
        .run();
}

#[test]
fn describe_placeholder_and_graph_columns() {
    let mut fx = repo()
        .git(["commit", "-q", "-m", "one"])
        .git(["tag", "-a", "-m", "v1", "v1"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "two"])
        .git(["tag", "light"]);
    for args in [
        &["log", "--format=%h %(describe)"][..],
        &["log", "--format=%(describe:tags)"],
        &["log", "--format=%(describe:tags=no,abbrev=4)"],
        &["log", "--format=%(describe:match=v*)"],
        &["log", "--format=%(describe:exclude=v*)"],
        &["log", "--graph", "--format=%h %>|(30)%s"],
        &["log", "--graph", "--format=%<|(25)%s|"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}

#[test]
fn posix_tz_without_rules() {
    let mut fx = repo();
    for t in [
        "1710000000",
        "1710075600",
        "1710200000",
        "1730620799",
        "1730620800",
    ] {
        fx = fx.env("GIT_COMMITTER_DATE", format!("@{t} +0000")).git([
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            t,
        ]);
    }
    for tz in ["CET-1CEST", "JST-9JDT", "XST8XDT"] {
        fx.compare(["log", "--format=%cd", "--date=iso-local"])
            .env("TZ", tz)
            .checks(Check::OUTPUT)
            .run();
    }
}
