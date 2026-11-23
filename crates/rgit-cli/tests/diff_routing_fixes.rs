// SPDX-License-Identifier: MIT
//! Differential tests of diff behaviour the conformance scripts need once
//! the plain diff front ends run through rgit: pickaxe option errors and
//! textconv, `diff.suppressBlankEmpty`, combined diffs of several commits,
//! `--attr-source`, symbolic links and diff drivers, the implicit `HEAD`
//! next to a file named `HEAD`, aliases over `whatchanged`, and trees with
//! duplicate entries.

use rgit_difftest::{Check, Fixture};

fn compare_all(fx: &mut Fixture, cases: &[&[&str]]) {
    let mut failures = Vec::new();
    for argv in cases {
        if let Err(m) = fx.compare(*argv).checks(Check::OUTPUT).try_run() {
            failures.push(format!("{argv:?}\n{}", m.report()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn rgit_fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
}

#[test]
fn pickaxe_usage_and_textconv() {
    let mut fx = rgit_fixture()
        .write("f", "Picked\n")
        .write("b", "a\0b\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .write("f", "other\n")
        .write("b", "x\0a\n")
        .git(["commit", "-q", "-a", "-m", "two"]);
    compare_all(
        &mut fx,
        &[
            &["log", "-Gregex", "-Sstring"],
            &["log", "-Gregex", "--find-object=HEAD"],
            &["log", "--pickaxe-all", "--find-object=HEAD"],
            &["log", "-Gregex", "--pickaxe-regex"],
            &["log", "-S", ""],
            &["log", "-G", ""],
            &["log", "--format=%s", "-i", "-Gpicked"],
            &["log", "--format=%s", "--regexp-ignore-case", "-SPICKED"],
            &["log", "--format=%s", "-i", "-SPICK", "--pickaxe-regex"],
            &["log", "--format=%s", "-Ga"],
            &["diff", "-Gregex", "-Sstring", "HEAD~1", "HEAD"],
        ],
    );
    let mut fx = fx.write(".gitattributes", "b diff=bin\n");
    compare_all(
        &mut fx,
        &[
            &["-c", "diff.bin.textconv=cat", "log", "--format=%s", "-Ga"],
            &["-c", "diff.bin.textconv=false", "log", "-Ga"],
            &[
                "-c",
                "diff.bin.textconv=false",
                "log",
                "--no-textconv",
                "-Ga",
            ],
        ],
    );
}

#[test]
fn suppress_blank_empty_and_symlinks() {
    let mut fx = rgit_fixture()
        .write("f", "\nx\n\n")
        .git(["add", "f"])
        .git(["commit", "-q", "-m", "one"])
        .write("f", "\ny\n\n")
        .write("file.bin", "x\n")
        .symlink("file.bin", "link.bin")
        .git(["add", "-N", "file.bin", "link.bin"])
        .write(".gitattributes", "*.bin diff=bin\n");
    compare_all(
        &mut fx,
        &[
            &["-c", "diff.suppressBlankEmpty=true", "diff", "--", "f"],
            &[
                "-c",
                "diff.suppressBlankEmpty=true",
                "diff",
                "--color",
                "--",
                "f",
            ],
            &["-c", "diff.suppressBlankEmpty=false", "diff", "--", "f"],
            &["-c", "diff.bin.binary=true", "diff", "file.bin", "link.bin"],
        ],
    );
}

#[test]
fn combined_diff_of_several_commits() {
    let mut fx = rgit_fixture()
        .write("a", "1\n2\n3\n")
        .write("b", "x\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "base"])
        .git(["branch", "side"])
        .tick()
        .write("a", "1\n2\n3\n4\n")
        .git(["commit", "-q", "-a", "-m", "main"])
        .tick()
        .git(["checkout", "-q", "side"])
        .write("b", "y\n")
        .git(["commit", "-q", "-a", "-m", "side"])
        .tick()
        .git(["checkout", "-q", "main"])
        .git(["merge", "-q", "--no-edit", "side"])
        .tick()
        .write("a", "0\n1\n2\n3\n4\n")
        .git(["commit", "-q", "-a", "--amend", "--no-edit"]);
    compare_all(
        &mut fx,
        &[
            &["diff", "HEAD", "HEAD^", "side"],
            &["diff", "--stat", "HEAD", "HEAD^", "side"],
            &["diff", "--raw", "HEAD", "HEAD^", "side"],
            &["diff", "--patch-with-stat", "HEAD", "HEAD^", "side"],
            &["diff", "--exit-code", "HEAD", "HEAD^", "side"],
            &["diff", "HEAD", "HEAD^", "side", "--", "a"],
        ],
    );
}

#[test]
fn attr_source_selects_the_driver() {
    let lines: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    let moved: String = (1..=30)
        .map(|i| format!("line {}\n", if i % 7 == 0 { 31 - i } else { i }))
        .collect();
    let mut fx = rgit_fixture()
        .write("file1", &lines)
        .write("file2", &moved)
        .write(".gitattributes", "file* diff=driverA\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .git(["branch", "branchA"])
        .git(["clone", "-q", "--bare", ".", "bare.git"]);
    compare_all(
        &mut fx,
        &[
            &[
                "-C",
                "bare.git",
                "--attr-source=branchA",
                "-c",
                "diff.driverA.algorithm=patience",
                "diff",
                "HEAD:file1",
                "HEAD:file2",
            ],
            &[
                "-C",
                "bare.git",
                "--attr-source=invalid-branch",
                "diff",
                "HEAD:file1",
                "HEAD:file2",
            ],
        ],
    );
}

#[test]
fn file_named_head_and_deprecated_aliases() {
    let mut fx = rgit_fixture().write("HEAD", "").git(["add", "HEAD"]).git([
        "commit",
        "-q",
        "-m",
        "head file",
    ]);
    compare_all(
        &mut fx,
        &[
            &["show", "-s", "--pretty=format:%s"],
            &["log", "--oneline"],
            &["-c", "alias.whatchanged=status", "whatchanged", "-h"],
            &["diff-tree", "--graph", "HEAD", "--"],
        ],
    );
}
