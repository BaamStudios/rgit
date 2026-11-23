// SPDX-License-Identifier: MIT
//! Differential tests of `git column`, `column.ui`/`column.<cmd>` and the
//! `--column` options of `tag`, `branch` and `status` against the
//! reference git.

use rgit_difftest::{Check, Fixture};

const WORDS: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\n";

#[test]
fn git_column() {
    let mut fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return;
    }
    let mut cases: Vec<Vec<String>> = Vec::new();
    for w in 12..=24 {
        for mode in [
            "column",
            "row",
            "column,dense",
            "row,dense",
            "plain",
            "never",
            "always",
        ] {
            cases.push(vec![
                "column".into(),
                format!("--mode={mode}"),
                format!("--width={w}"),
            ]);
        }
    }
    for extra in [
        &["--mode=column", "--width=20", "--indent=  "][..],
        &["--mode=column,dense", "--width=19", "--indent=ab"],
        &["--mode=column", "--width=20", "--padding=0"],
        &["--mode=row", "--width=20", "--padding=3"],
        &["--mode=column", "--width=20", "--nl=Z\n"],
        &["--mode=column", "--padding=-1"],
        &["--mode=bogus"],
        &["--mode=column", "--width=1"],
        &[],
        &["--command=tag"],
    ] {
        let mut v = vec!["column".to_owned()];
        v.extend(extra.iter().map(|s| s.to_string()));
        cases.push(v);
    }
    for args in &cases {
        fx.compare(args).stdin(WORDS).checks(Check::OUTPUT).run();
    }
    fx.compare(["column", "--mode=column", "--width=12"])
        .stdin("äöü\nab\n日本語\nc\nd\ne\nf\na\tb\n")
        .checks(Check::OUTPUT)
        .run();
    fx.compare([
        "-c",
        "column.ui=always",
        "column",
        "--command=status",
        "--width=40",
    ])
    .stdin(WORDS)
    .checks(Check::OUTPUT)
    .run();
    fx.compare([
        "-c",
        "column.ui=never",
        "-c",
        "column.status=row",
        "column",
        "--command=status",
        "--width=40",
    ])
    .stdin(WORDS)
    .checks(Check::OUTPUT)
    .run();
    fx.compare(["-c", "column.ui=bogus", "column", "--command=tag"])
        .stdin(WORDS)
        .checks(Check::OUTPUT)
        .run();
}

fn repo() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["commit", "-q", "--allow-empty", "-m", "one"]);
    for b in [
        "a/b/c", "abc", "bam", "bar", "foo", "j/k", "l", "m/m", "mb", "n", "o/o", "q", "topic",
    ] {
        fx = fx.git(["branch", b]);
    }
    for t in [
        "a1", "aa1", "cba", "t210", "t211", "v0.2.1", "v1.0", "v1.0.1", "v1.1.3",
    ] {
        fx = fx.git(["tag", t]);
    }
    fx
}

#[test]
fn tag_and_branch() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    for (cols, args) in [
        ("41", &["tag", "-l", "--column=row"][..]),
        (
            "40",
            &["-c", "column.tag=row", "-c", "column.ui=dense", "tag", "-l"],
        ),
        ("40", &["tag", "--column", "-n"]),
        ("40", &["-c", "column.ui=row dense", "tag", "-l", "-n"]),
        ("40", &["tag", "--column=bogus"]),
        ("40", &["tag", "--column", "--format=%(refname)"]),
        ("40", &["tag", "--column", "--no-column"]),
        ("81", &["branch", "--column=column"]),
        (
            "80",
            &[
                "-c",
                "column.ui=column",
                "-c",
                "column.branch=dense",
                "branch",
            ],
        ),
        ("80", &["branch", "--column", "-v"]),
        ("80", &["-c", "column.ui=column", "branch", "-v"]),
        ("80", &["branch", "--column=bogus"]),
        ("30", &["branch", "--column=row", "-a"]),
    ] {
        fx.compare(args)
            .env("COLUMNS", cols)
            .checks(Check::OUTPUT)
            .run();
    }
}

#[test]
fn status_untracked() {
    let mut fx = repo()
        .write("dir1/untracked", "u\n")
        .write("dir2/untracked", "u\n")
        .write("dir2/modified", "m\n")
        .write("untracked", "u\n")
        .write("a file", "u\n");
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["status", "--column"][..],
        &["status", "--column=column dense"],
        &[
            "-c",
            "status.displayCommentPrefix=true",
            "status",
            "--column=row",
        ],
        &["-c", "column.status=always", "status"],
        &["-c", "column.ui=always", "status", "--no-column"],
        &["status", "--column", "-s"],
        &["status", "--column=bogus"],
    ] {
        fx.compare(args)
            .env("COLUMNS", "50")
            .checks(Check::OUTPUT)
            .run();
    }
}
