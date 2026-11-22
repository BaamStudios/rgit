// SPDX-License-Identifier: MIT
//! Differential tests of `--word-diff`, `--word-diff-regex`,
//! `--color-words`, `diff.wordRegex` and `diff.<driver>.wordRegex`
//! against the reference git.

use rgit_difftest::{Check, Fixture};

/// (old, new) contents, one file each.
const CASES: &[(&str, &str)] = &[
    (
        "ctx1\nfoo bar  baz\nqux\nctx2\nctx3\nctx4\nctx5\nctx6\nctx7\nlast one\n",
        "ctx1\nfoo  BAR baz quux\nctx2\nctx3\nctx4\nctx5\nctx6\nctx7\nlast two\nadded line\n",
    ),
    ("a b\n", "a b c"),
    ("a\n\nb\n", "a\nb\n"),
    ("a\nb\n", "a\n\n\nb\n"),
    ("x  y\n", "x y\n"),
    ("a\nx  y\nb\n", "a\nb\n"),
    ("a\n  \nb\n", "a\n\t\nb\n"),
    ("a\nb c\nd\n", "a\nb\nc d\n"),
    ("p q r\ns\n", "p q\n"),
    ("a b\nc d\n", "x b\nc y\n"),
    ("  lead x\n", "lead  y\n"),
    ("f(a, b);\nint x = 1;\n", "f(a,b, c);\nlong x = 2;\n"),
    ("one two three four five\n", "one 2 three 4 five six\n"),
    ("", "new\nfile\n"),
    ("old\nfile\n", ""),
];

fn fixture() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write(".gitattributes", "*.c diff=cee\n");
    for (i, (old, _)) in CASES.iter().enumerate() {
        fx = fx.write(format!("f{i}"), old).write(format!("f{i}.c"), old);
    }
    fx = fx.git(["add", "."]).git(["commit", "-q", "-m", "first"]);
    for (i, (_, new)) in CASES.iter().enumerate() {
        fx = fx.write(format!("f{i}"), new).write(format!("f{i}.c"), new);
    }
    fx
}

#[test]
fn word_diff_matches_git() {
    let mut fx = fixture();
    let mut failures = Vec::new();
    let runs: &[&[&str]] = &[
        &["diff", "--word-diff"],
        &["diff", "--word-diff=plain", "--color"],
        &["diff", "--word-diff=porcelain"],
        &["diff", "--word-diff=color"],
        &["diff", "--color-words"],
        &["diff", "--color-words=[a-z]+|[^[:space:]]"],
        &["diff", "--word-diff-regex=."],
        &[
            "diff",
            "--word-diff=porcelain",
            "--word-diff-regex=[^[:space:]]",
        ],
        &["-c", "diff.wordRegex=.", "diff", "--word-diff"],
        &["-c", "diff.cee.wordRegex=[a-z]", "diff", "--word-diff"],
        &[
            "-c",
            "diff.cee.wordRegex=[a-z]",
            "diff",
            "--word-diff-regex=.",
        ],
        &["diff", "--word-diff=color", "--no-color"],
        &["diff", "--no-color", "--word-diff=color"],
        &["diff", "--color-words", "--word-diff=none"],
        &["diff", "--word-diff=bogus"],
        &["diff", "--word-diff-regex=("],
    ];
    for argv in runs {
        if let Err(m) = fx.compare(*argv).checks(Check::OUTPUT).try_run() {
            failures.push(format!("{argv:?}\n{}", m.report()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
