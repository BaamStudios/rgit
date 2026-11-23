// SPDX-License-Identifier: MIT
//! Differential tests of plumbing diff forms against the reference git:
//! `diff-files -0`..`-3`, `-s` clearing every format, `--check` finding
//! leftover conflict markers, missing blobs in patches and pathspecs
//! naming a submodule with a trailing slash.

use rgit_difftest::{Check, Fixture};

fn rgit_fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
}

fn compare_all(fx: &mut Fixture, cases: &[&[&str]]) {
    let mut failures = Vec::new();
    for argv in cases {
        if let Err(m) = fx.compare(*argv).checks(Check::OUTPUT).try_run() {
            failures.push(format!("{argv:?}\n{}", m.report()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn diff_files_stages() {
    let mut fx = rgit_fixture()
        .write("b1", "1\n")
        .write("b2", "2\n")
        .write("b3", "3\n");
    if fx.is_skipped() {
        return;
    }
    let blob = |fx: &Fixture, f: &str| {
        String::from_utf8(fx.git_stdout(["hash-object", "-w", f]))
            .unwrap()
            .trim()
            .to_owned()
    };
    let (b1, b2, b3) = (blob(&fx, "b1"), blob(&fx, "b2"), blob(&fx, "b3"));
    let info = format!(
        "100644 {b1} 1\tall\n100644 {b2} 2\tall\n100644 {b3} 3\tall\n\
         100644 {b2} 2\tours\n100644 {b3} 3\tours\n100644 {b1} 1\tbase\n"
    );
    fx.compare(["update-index", "--index-info"])
        .stdin(info)
        .run();
    fx = fx.write("all", "").write("ours", "").write("base", "x\n");
    compare_all(
        &mut fx,
        &[
            &["diff-files"],
            &["diff-files", "-0"],
            &["diff-files", "-1"],
            &["diff-files", "-2"],
            &["diff-files", "-3"],
            &["diff-files", "--base"],
            &["diff-files", "--ours"],
            &["diff-files", "--theirs"],
            &["diff-files", "-3", "-p"],
            &["diff-files", "-1", "--stat"],
        ],
    );
}

#[test]
fn no_patch_clears_every_format() {
    let mut fx = rgit_fixture()
        .write("d/a", "a\nb\n")
        .write("c", "c \n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .write("d/a", "a\nc\n")
        .write("c", "c\n<<<<<<< x\n");
    if fx.is_skipped() {
        return;
    }
    let mut cases: Vec<Vec<&str>> = Vec::new();
    for format in [
        "--dirstat",
        "--cumulative",
        "--dirstat-by-file",
        "--check",
        "--compact-summary",
        "--stat",
        "--raw",
    ] {
        cases.push(vec!["diff-files", format, "-s", "-p"]);
        cases.push(vec!["diff", format, "-s"]);
    }
    let cases: Vec<&[&str]> = cases.iter().map(Vec::as_slice).collect();
    compare_all(&mut fx, &cases);
}

#[test]
fn check_finds_conflict_markers() {
    let mut lines = String::from("x\n");
    for m in ["<<<<<<<", "=======", ">>>>>>>", "|||||||"] {
        for suffix in ["", " ", "\t", "x", " y", "\ty", "\r"] {
            lines.push_str(m);
            lines.push_str(suffix);
            lines.push('\n');
        }
    }
    lines.push_str("<<<<<<<<\n ========\n<<<\n<<< a\n=== x\n");
    let mut fx = rgit_fixture()
        .write("f", "x\n")
        .write("g", "x\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .write("f", &lines)
        .write("g", &lines)
        .write(".gitattributes", "g conflict-marker-size=3\n");
    if fx.is_skipped() {
        return;
    }
    compare_all(
        &mut fx,
        &[
            &["diff", "--check"],
            &["diff-files", "--check"],
            &["-c", "core.whitespace=-trailing-space", "diff", "--check"],
            &["diff", "--check", "--", "g"],
        ],
    );
    fx = fx.git(["add", "f"]);
    compare_all(
        &mut fx,
        &[
            &["diff", "--cached", "--check"],
            &["diff-index", "--cached", "--check", "HEAD"],
        ],
    );
}

#[test]
fn patch_of_a_missing_blob_fails() {
    let zero = [0u8; 20];
    let mut tree = b"100644 foo\0".to_vec();
    tree.extend_from_slice(&zero);
    let mut fx = rgit_fixture()
        .write("bogus", tree)
        .write("foo", "bar\n")
        .git(["add", "foo"]);
    if fx.is_skipped() {
        return;
    }
    let bogus = String::from_utf8(fx.git_stdout([
        "hash-object",
        "--literally",
        "-w",
        "-t",
        "tree",
        "bogus",
    ]))
    .unwrap()
    .trim()
    .to_owned();
    let good = String::from_utf8(fx.git_stdout(["write-tree"]))
        .unwrap()
        .trim()
        .to_owned();
    let empty = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    let cases: Vec<Vec<&str>> = vec![
        vec!["diff-tree", empty, &bogus],
        vec!["diff-tree", "-p", empty, &bogus],
        vec!["diff-tree", "-p", &bogus, empty],
        vec!["diff-tree", "-p", &good, &bogus],
        vec!["diff-tree", "-R", "-p", &good, &bogus],
        vec!["diff-tree", "--stat", &good, &bogus],
        vec!["diff-index", "--cached", "-p", &bogus],
        vec!["diff-index", "--cached", &bogus],
    ];
    let cases: Vec<&[&str]> = cases.iter().map(Vec::as_slice).collect();
    compare_all(&mut fx, &cases);
}

#[test]
fn submodule_pathspec_with_trailing_slash() {
    let mut fx = rgit_fixture()
        .git(["init", "-q", "sub"])
        .write("sub/a", "a\n")
        .git(["-C", "sub", "add", "a"])
        .git(["-C", "sub", "commit", "-q", "-m", "first"])
        .git(["add", "sub"])
        .git(["commit", "-q", "-m", "first"])
        .tick()
        .write("sub/a", "b\n")
        .git(["-C", "sub", "commit", "-q", "-a", "-m", "second"])
        .git(["add", "sub"])
        .git(["commit", "-q", "-m", "second"]);
    compare_all(
        &mut fx,
        &[
            &["diff", "--name-only", "HEAD^", "HEAD", "sub/"],
            &["diff", "--name-only", "HEAD^", "HEAD", "--", "sub/whatever"],
            &["diff", "--name-only", "HEAD^", "sub/"],
            &["diff-tree", "--name-only", "HEAD^", "HEAD", "sub/"],
            &["diff-index", "--name-only", "HEAD^", "sub/"],
        ],
    );
}
