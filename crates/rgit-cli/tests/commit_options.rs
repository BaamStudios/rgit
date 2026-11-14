// SPDX-License-Identifier: MIT
//! `commit` options of #128 against the reference git: templates, `-s`,
//! `--trailer`, `-C`/`-c`, `--fixup`, `--squash`, `-v`, `--dry-run` and
//! the status formats, the `post-rewrite` hook. Editors are shell
//! functions that show what they got on standard error.

use rgit_difftest::{Check, Fixture};

/// Shows the file on stderr and keeps it.
const SHOW: &str = "f() { cat \"$1\" >&2; }; f";
/// Shows the file and appends a line.
const APPEND: &str = "f() { cat \"$1\" >&2; echo appended >>\"$1\"; }; f";

fn base() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
}

/// Three commits on `main` (the second with a body and a trailer) and a
/// staged change, an unstaged change and an untracked file.
fn history() -> Fixture {
    base()
        .write("a", "1\n2\n3\n")
        .write("b", "b\n")
        .git(["add", "a", "b"])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .write("a", "1\n2\n3\n4\n")
        .git([
            "commit",
            "-q",
            "-a",
            "-m",
            "two subject\ncontinued\n\nbody of two\n\nAcked-by: X <x@x>",
        ])
        .tick()
        .write("b", "b2\n")
        .git(["commit", "-q", "-a", "-m", "three"])
        .tick()
        .write("c", "c\n")
        .git(["add", "c"])
        .write("a", "changed\n")
        .write("u", "u\n")
        .write("tmpl", "template line  \n\n\n# comment in template\n\n")
}

fn checks() -> Vec<Check> {
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

fn run(mut fx: Fixture, args: &[&str], editor: &str) {
    if fx.is_skipped() {
        return;
    }
    fx.compare(args)
        .checks(checks())
        .env("GIT_EDITOR", editor)
        .git_file("COMMIT_EDITMSG")
        .stdin("from stdin\n")
        .run();
}

fn each(cases: &[&[&str]], editor: &str, fixture: fn() -> Fixture) {
    for args in cases {
        run(fixture(), args, editor);
    }
}

#[test]
fn templates() {
    let cases: &[&[&str]] = &[
        &["commit", "-t", "tmpl"],
        &["commit", "--template=tmpl", "-s"],
        &["commit", "-t", "tmpl", "--cleanup=whitespace"],
        &["commit", "-t", "tmpl", "--cleanup=verbatim"],
        &["commit", "-t", "tmpl", "--trailer", "A: b"],
        &["commit", "-t", "tmpl", "-m", "given"],
        &["commit", "-t", "tmpl", "--amend"],
        &["commit", "-t", "tmpl", "--squash", "HEAD"],
        &["commit", "-t", "missing"],
        &["commit", "-t", ":(optional)missing"],
        &["commit", "-t", ""],
        &["-c", "commit.template=tmpl", "commit"],
        &["-c", "commit.template=missing", "commit"],
        &["-c", "commit.template=", "commit"],
        &["-c", "commit.template", "commit"],
    ];
    each(cases, SHOW, history);
    each(&cases[..5], APPEND, history);
    // Only comments or a signoff added: the template counts as unedited.
    let comment = "f() { echo '# more' >>\"$1\"; }; f";
    run(history(), &["commit", "-t", "tmpl"], comment);
    let sob = "f() { echo 'Signed-off-by: Q <q@q>' >>\"$1\"; }; f";
    run(history(), &["commit", "-t", "tmpl"], sob);
}

#[test]
fn signoff_and_trailers() {
    let cases: &[&[&str]] = &[
        &["commit", "-s", "-m", "msg"],
        &["commit", "-s", "-m", "msg\n\nFoo: bar"],
        &["commit", "-s", "-m", "msg\n\nSigned-off-by: X <x@x>"],
        &["commit", "-s", "-m", "msg\n\nnot a trailer"],
        &["commit", "-s", "-m", "msg\n\n# comment"],
        &["commit", "-s", "--no-signoff", "-m", "msg"],
        &["commit", "-s", "-e", "-m", "msg"],
        &["commit", "-s"],
        &["commit", "-s", "--no-status"],
        &["commit", "-s", "-F", "-"],
        &["commit", "-s", "--cleanup=verbatim", "-m", "msg  "],
        &[
            "commit",
            "-s",
            "--allow-empty-message",
            "--cleanup=verbatim",
            "-F",
            "/dev/null",
        ],
        &[
            "commit",
            "--trailer",
            "Acked-by: Z <z@z>",
            "--trailer",
            "fixes=123",
            "-m",
            "m",
        ],
        &["commit", "--trailer", "A: b", "-s", "-m", "msg"],
        &["commit", "-s", "--trailer", "A: b", "-e", "-m", "msg"],
        &["commit", "--trailer", "A: b", "-m", "msg\n\n# comment"],
        &["commit", "--trailer", "A: b", "--no-trailer", "-m", "msg"],
        &[
            "-c",
            "trailer.ifexists=replace",
            "commit",
            "--trailer",
            "Acked-by: new",
            "-C",
            "HEAD~1",
        ],
        &[
            "-c",
            "trailer.separators=:#",
            "commit",
            "--trailer",
            "Issue#12",
            "-m",
            "m",
        ],
    ];
    each(cases, SHOW, history);
}

#[test]
fn reuse_message() {
    let cases: &[&[&str]] = &[
        &["commit", "-C", "HEAD~1"],
        &["commit", "--reuse-message=HEAD~1"],
        &["commit", "-c", "HEAD~1"],
        &["commit", "--reedit-message", "HEAD~1"],
        &["commit", "-C", "HEAD~1", "-e"],
        &["commit", "-c", "HEAD~1", "--no-edit"],
        &["commit", "-C", "HEAD~1", "--reset-author"],
        &["commit", "-C", "HEAD~1", "--author", "Q <q@q>"],
        &["commit", "-C", "HEAD~1", "--date", "2001-01-01T00:00:00Z"],
        &["commit", "-C", "HEAD~1", "--amend"],
        &["commit", "-C", "HEAD~1", "-s"],
        &["commit", "-C", "nosuch"],
        &["commit", "-C", "HEAD", "-c", "HEAD"],
        &["commit", "-C", "HEAD", "-F", "tmpl"],
        &["commit", "-c", "HEAD", "-m", "x"],
        &["commit", "-C", "HEAD", "-m", "x"],
        &["commit", "-F", "tmpl", "-c", "HEAD"],
        &["commit", "--reset-author", "-m", "x"],
        &[
            "commit",
            "-C",
            "HEAD",
            "--reset-author",
            "--author",
            "Q <q@q>",
        ],
    ];
    each(cases, SHOW, history);
    let fx = history().env("GIT_AUTHOR_DATE", "1112912000 -0700");
    run(fx, &["commit", "-C", "HEAD~1", "--reset-author"], SHOW);
    let fx = history().env("GIT_AUTHOR_DATE", "1112912000 -0700");
    run(fx, &["commit", "-m", "later author date"], SHOW);
}

#[test]
fn fixup_and_squash() {
    let cases: &[&[&str]] = &[
        &["commit", "--fixup", "HEAD~1"],
        &["commit", "--fixup=HEAD~1", "-m", "extra", "-m", "more"],
        &["commit", "--fixup", "HEAD~1", "-e"],
        &["commit", "--fixup=amend:HEAD~1"],
        &["commit", "--fixup=amend:HEAD~1", "--only"],
        &["commit", "--fixup=reword:HEAD~1"],
        &["commit", "--fixup", "reword:HEAD~1"],
        &["commit", "--fixup=amend:HEAD~1", "-m", "x"],
        &["commit", "--fixup=reword:HEAD~1", "-m", "x"],
        &["commit", "--fixup=reword:HEAD~1", "-a"],
        &["commit", "--fixup=reword:HEAD~1", "--only"],
        &["commit", "--fixup=reword:HEAD~1", "--", "a"],
        &["commit", "--fixup=amend:HEAD~1", "-F", "tmpl"],
        &["commit", "--fixup=bogus:HEAD~1"],
        &["commit", "--fixup", "HEAD~1", "-C", "HEAD"],
        &["commit", "--fixup", "HEAD~1", "--squash", "HEAD"],
        &["commit", "--fixup", "nosuch"],
        &["commit", "--fixup", "HEAD", "--only"],
        &["commit", "--squash", "HEAD~1"],
        &["commit", "--squash", "HEAD~1", "-m", "sq msg"],
        &["commit", "--squash", "HEAD~1", "-C", "HEAD~2"],
        &["commit", "--squash", "HEAD", "-C", "HEAD"],
        &["commit", "--squash", "HEAD", "-c", "HEAD"],
        &["commit", "--squash", "HEAD~1", "-F", "tmpl"],
        &["commit", "--squash", "HEAD~1", "-s", "-m", "x"],
        &["commit", "--squash", "nosuch"],
    ];
    each(cases, SHOW, history);
    each(&cases[3..6], APPEND, history);
    // The body of an amend! message must not be empty.
    let only_subject = "f() { echo 'amend! two subject continued' >\"$1\"; }; f";
    run(history(), &["commit", "--fixup=amend:HEAD~1"], only_subject);
    run(
        history(),
        &["commit", "--fixup=amend:HEAD~1", "--allow-empty-message"],
        only_subject,
    );
    // A second amend! drops the first amend! line from the body.
    let fx = history().git([
        "-c",
        "core.editor=true",
        "commit",
        "-q",
        "--fixup=reword:HEAD~1",
    ]);
    run(fx, &["commit", "--fixup=reword:HEAD"], APPEND);
}

#[test]
fn verbose() {
    let cases: &[&[&str]] = &[
        &["commit", "-v"],
        &["commit", "-vv"],
        &["commit", "-v", "-v", "-a"],
        &["commit", "-v", "--no-verbose"],
        &["commit", "-v", "--no-status"],
        &["commit", "-v", "--amend"],
        &["commit", "-v", "--cleanup=scissors"],
        &["commit", "-v", "--cleanup=verbatim"],
        &["commit", "-v", "-U1"],
        &["commit", "-v", "-m", "x"],
        &["-c", "commit.verbose=true", "commit"],
        &["-c", "commit.verbose=2", "commit"],
        &["-c", "commit.verbose=-1", "commit"],
        &["-c", "commit.verbose=true", "commit", "--no-verbose"],
        &["-c", "diff.mnemonicPrefix=true", "commit", "-v"],
        &["-c", "core.commentChar=;", "commit", "-v"],
    ];
    each(cases, SHOW, history);
    each(&cases[..3], APPEND, history);
}

#[test]
fn dry_run() {
    let cases: &[&[&str]] = &[
        &["commit", "--dry-run"],
        &["commit", "--dry-run", "-a"],
        &["commit", "--dry-run", "b"],
        &["commit", "--dry-run", "-i", "b"],
        &["commit", "--dry-run", "--amend"],
        &["commit", "--dry-run", "-uno"],
        &["commit", "--dry-run", "-v"],
        &["commit", "--dry-run", "-q", "-m", "x"],
        &["commit", "--short"],
        &["commit", "--short", "-a"],
        &["commit", "--short", "--branch"],
        &["commit", "--porcelain"],
        &["commit", "--porcelain", "-a", "--branch"],
        &["commit", "-z"],
        &["commit", "-z", "--short"],
        &["commit", "--long"],
        &["commit", "--short", "--long"],
        &["commit", "--long", "-z"],
        &["commit", "--short", "--amend", "-uall"],
    ];
    each(cases, SHOW, history);
    let clean = || {
        base()
            .write("a", "a\n")
            .git(["add", "a"])
            .git(["commit", "-q", "-m", "one"])
            .tick()
    };
    for args in [
        &["commit", "--dry-run"][..],
        &["commit", "--short"],
        &["commit", "--porcelain"],
        &["commit", "--dry-run", "-m", "x"],
    ] {
        run(clean(), args, SHOW);
        run(clean().write("u", "u\n"), args, SHOW);
    }
    // In a subdirectory, with a rename.
    let mut fx = history()
        .write("d/b2", "b2\n")
        .git(["rm", "-q", "b"])
        .git(["add", "d/b2"])
        .write("d/x y", "x\n");
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["commit", "--short"][..],
        &["commit", "--porcelain"],
        &["commit", "-z"],
        &["commit", "--dry-run"],
    ] {
        fx.compare(args).checks(checks()).in_dir("d").run();
    }
}

#[test]
fn allow_empty_message_instructions() {
    let cases: &[&[&str]] = &[
        &["commit", "--allow-empty-message"],
        &["commit", "--allow-empty-message", "--cleanup=whitespace"],
        &["commit", "--allow-empty-message", "--cleanup=scissors"],
    ];
    each(cases, SHOW, history);
}

#[test]
fn post_rewrite_hook() {
    let hook = "#!/bin/sh\necho \"post-rewrite [$*]\" >&2\ncat >&2\n";
    let fx = || {
        history()
            .write_executable(".git/hooks/post-rewrite", hook)
            .write_executable(
                ".git/hooks/post-commit",
                "#!/bin/sh\necho post-commit >&2\n",
            )
    };
    run(fx(), &["commit", "--amend", "-m", "amended"], SHOW);
    run(
        fx(),
        &["commit", "--amend", "--no-post-rewrite", "-m", "amended"],
        SHOW,
    );
    run(fx(), &["commit", "-m", "not amended"], SHOW);
}
