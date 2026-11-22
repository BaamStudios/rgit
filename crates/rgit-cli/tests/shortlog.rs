// SPDX-License-Identifier: MIT
//! Differential tests of `rgit shortlog`, `rgit check-mailmap`,
//! `cat-file --use-mailmap` and mailmapped `log` against the reference git.

use rgit_difftest::{Check, Fixture};

fn commit(fx: Fixture, author: &str, msg: &str) -> Fixture {
    fx.git([
        "commit",
        "-q",
        "--allow-empty",
        "--author",
        author,
        "-m",
        msg,
    ])
    .tick()
}

/// Several authors (two of them mapped), `[PATCH]` subjects, trailers
/// and a `.mailmap`.
fn history() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    fx = commit(fx, "Bob <b@x>", "[PATCH] patch subject");
    fx = commit(fx, "alice <a@x>", "lower alice");
    fx = commit(fx, "Alice <A@x>", "  leading spaces\nsecond line\n\nbody");
    fx = commit(
        fx,
        "Bob <b@x>",
        "[PATCH 2/3] numbered\n\nSigned-off-by: Carol <c@x>\nHelped-by: Dan <d@x>",
    );
    fx = commit(
        fx,
        "Old Name <old@x>",
        "renamed\n\nSigned-off-by: Bob <b@x>\nSigned-off-by: Bob <b@x>",
    );
    fx = commit(
        fx,
        "Zed <z@x>",
        "a very long subject line that will certainly need to be wrapped by shortlog -w output",
    );
    fx.write(
        ".mailmap",
        "# comment\nNew Name <new@x> <old@x>\nAlice <a@x>\nCarol C. <c@x>\n",
    )
}

fn same(fx: &mut Fixture, args: &[&str]) {
    fx.compare(args).checks(Check::OUTPUT).run();
}

#[test]
fn shortlog_walks() {
    let mut fx = history();
    for args in [
        &["shortlog", "HEAD"][..],
        &["shortlog", "-n", "HEAD"],
        &["shortlog", "-s", "HEAD"],
        &["shortlog", "-sne", "HEAD"],
        &["shortlog", "-e", "HEAD~2"],
        &["shortlog", "-c", "-s", "HEAD"],
        &["shortlog", "-w", "HEAD"],
        &["shortlog", "-w30,2,4", "HEAD"],
        &["shortlog", "-w0", "HEAD"],
        &["shortlog", "--format=%h %s", "HEAD"],
        &["shortlog", "--format=two%nlines%n%nmore", "HEAD"],
        &["shortlog", "--format=", "-s", "HEAD"],
        &["shortlog", "-ns", "--group=trailer:signed-off-by", "HEAD"],
        &["shortlog", "-nse", "--group=trailer:Signed-off-by", "HEAD"],
        &[
            "shortlog",
            "-ns",
            "--group=author",
            "--group=trailer:helped-by",
            "HEAD",
        ],
        &["shortlog", "-s", "--group=format:%an/%cn", "HEAD"],
        &["shortlog", "-s", "--group=%ae", "HEAD"],
        &["shortlog", "-s", "--group=committer", "--no-group", "HEAD"],
        &["shortlog", "--group=bogus", "HEAD"],
        &["shortlog", "-wx", "HEAD"],
        &["shortlog", "--bogus", "HEAD"],
        &["shortlog", "-s", "HEAD~3..HEAD"],
        &["-c", "mailmap.file=nonexistent", "shortlog", "-s", "HEAD"],
    ] {
        same(&mut fx, args);
    }
    // git reports the bad blob once per mailmap it loads.
    fx.compare(["-c", "mailmap.blob=HEAD:", "shortlog", "-s", "HEAD"])
        .checks(Check::OUTPUT)
        .skip(Check::Stderr)
        .run();
}

#[test]
fn shortlog_reads_logs() {
    let mut fx = history();
    for fmt in ["--format=medium", "--format=raw", "--format=full"] {
        let log = fx.git_stdout(["log", fmt]);
        for args in [
            &["shortlog"][..],
            &["shortlog", "-e"],
            &["shortlog", "-c", "-s"],
        ] {
            fx.compare(args)
                .stdin(log.clone())
                .checks(Check::OUTPUT)
                .run();
        }
    }
    let crafted = b"commit 1\nAuthor: X <x@y>\nDate: now\n\n    [PATCH] first\n\n\
                    commit 2\nAuthor: X <x@y>\nfoo\n    second\n\
                    commit 3\nAuthor: No Email\n\n    third\n"
        .to_vec();
    fx.compare(["shortlog", "-e"])
        .stdin(crafted)
        .checks(Check::OUTPUT)
        .run();
    for args in [
        &["shortlog", "--group=author", "--group=committer"][..],
        &["shortlog", "--group=trailer:foo"],
        &["shortlog", "--group=format:%an"],
    ] {
        fx.compare(args)
            .stdin(Vec::new())
            .checks(Check::OUTPUT)
            .run();
    }
}

#[test]
fn check_mailmap_and_cat_file() {
    let mut fx = history()
        .write("extra.map", "Extra <extra@x> <z@x>\n")
        .git(["tag", "-a", "-m", "tag", "t1", "HEAD~1"]);
    for args in [
        &["check-mailmap"][..],
        &[
            "check-mailmap",
            "Some One <old@x>",
            "<a@x>",
            "a@x",
            "nobody@x",
            "X <>",
        ],
        &["check-mailmap", "--mailmap-file=extra.map", "Q <z@x>"],
        &["check-mailmap", "--mailmap-blob=HEAD", "Q <z@x>"],
        &["cat-file", "--use-mailmap", "-p", "HEAD~1"],
        &["cat-file", "--use-mailmap", "-s", "HEAD~1"],
        &["cat-file", "--mailmap", "commit", "HEAD~1"],
        &["cat-file", "--mailmap", "-p", "t1"],
        &["log", "--format=%an <%ae> %aN <%aE> %cN", "HEAD"],
        &["log", "--author=New", "--format=%h %aN"],
        &["log", "--author=old@x", "--format=%h %aN"],
        &["log", "--no-mailmap", "--author=old@x", "--format=%h %aN"],
    ] {
        same(&mut fx, args);
    }
    fx.compare(["check-mailmap", "--stdin", "first <old@x>"])
        .stdin(b"Q <c@x>\nc@x\n\n".to_vec())
        .checks(Check::OUTPUT)
        .run();
    for input in ["HEAD~1\nt1\n", "HEAD\n"] {
        for kind in ["--batch", "--batch-check"] {
            fx.compare(["cat-file", "--use-mailmap", kind])
                .stdin(input.as_bytes().to_vec())
                .checks(Check::OUTPUT)
                .run();
        }
    }
    fx.compare(["cat-file", "--batch-command"])
        .stdin(b"info HEAD~1\nmailmap yes\ninfo HEAD~1\ncontents HEAD~1\nmailmap false\ninfo HEAD~1\nmailmap maybe\n".to_vec())
        .checks(Check::OUTPUT)
        .run();
}
