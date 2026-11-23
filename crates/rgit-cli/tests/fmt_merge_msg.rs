// SPDX-License-Identifier: MIT
//! Differential tests of `fmt-merge-msg` (#397) against the reference git.

use rgit_difftest::Fixture;

/// `x` has two commits more than `main`, `y` one, tag `t1` (annotated)
/// points to `x`; `FETCH_HEAD` lists `x`, `y` (not for merge) and `t1`.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "a"])
        .git(["checkout", "-q", "-b", "x"])
        .tick()
        .write("b", "b\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "b commit"])
        .tick()
        .write("c", "c\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "c commit"])
        .git(["tag", "-a", "-m", "tag message", "t1"])
        .git(["checkout", "-q", "-b", "y", "main"])
        .tick()
        .write("d", "d\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "d commit"])
        .git(["checkout", "-q", "main"])
        .git(["fetch", "-q", ".", "x", "y", "tag", "t1"])
}

fn run(prep: impl Fn(Fixture) -> Fixture, stdin: Option<&str>, cases: &[&[&str]]) {
    let mut failed = Vec::new();
    for args in cases {
        let mut fx = prep(fixture());
        if fx.is_skipped() {
            return;
        }
        let mut c = fx.compare(*args);
        if let Some(s) = stdin {
            c = c.stdin(s.as_bytes().to_vec());
        }
        if let Err(m) = c.try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

#[test]
fn from_file() {
    run(
        |fx| fx,
        None,
        &[
            &["fmt-merge-msg", "-F", ".git/FETCH_HEAD"],
            &["fmt-merge-msg", "--log", "-F", ".git/FETCH_HEAD"],
            &["fmt-merge-msg", "--log=1", "-F", ".git/FETCH_HEAD"],
            &["fmt-merge-msg", "--log=x", "-F", ".git/FETCH_HEAD"],
            &["fmt-merge-msg", "--no-log", "-F", ".git/FETCH_HEAD"],
            &["fmt-merge-msg", "--summary", "-F", ".git/FETCH_HEAD"],
            &[
                "fmt-merge-msg",
                "-m",
                "custom",
                "--log",
                "-F",
                ".git/FETCH_HEAD",
            ],
            &["fmt-merge-msg", "-m", "custom", "-F", ".git/FETCH_HEAD"],
            &[
                "fmt-merge-msg",
                "--into-name",
                "other",
                "-F",
                ".git/FETCH_HEAD",
            ],
            &[
                "-c",
                "merge.log=1",
                "fmt-merge-msg",
                "-F",
                ".git/FETCH_HEAD",
            ],
            &[
                "-c",
                "merge.summary=true",
                "fmt-merge-msg",
                "-F",
                ".git/FETCH_HEAD",
            ],
            &[
                "-c",
                "merge.summary=true",
                "-c",
                "merge.log=false",
                "fmt-merge-msg",
                "-F",
                ".git/FETCH_HEAD",
            ],
            &["fmt-merge-msg", "-F", "nope"],
            &["fmt-merge-msg", "extra"],
            &["fmt-merge-msg", "-h"],
        ],
    );
    run(
        |fx| fx.git(["checkout", "-q", "--detach"]),
        None,
        &[&["fmt-merge-msg", "-F", ".git/FETCH_HEAD"]],
    );
}

#[test]
fn from_stdin() {
    run(|fx| fx, Some(""), &[&["fmt-merge-msg"]]);
    run(|fx| fx, Some("garbage\n"), &[&["fmt-merge-msg"]]);
    run(
        |fx| fx,
        Some("0123456789012345678901234567890123456789\t\tbranch 'z' of .\n"),
        &[&["fmt-merge-msg"]],
    );
}

#[test]
fn several_tags_and_merged_heads() {
    // Several tag messages each get a comment line naming the tag; heads
    // already merged get an empty shortlog heading; when every head is
    // merged there is no message at all.
    let tags = |fx: Fixture| {
        fx.git(["tag", "-a", "-m", "second tag", "t2", "y"])
            .git(["tag", "-a", "-m", " ", "tempty", "y"])
    };
    let cases: &[&[&str]] = &[
        &["fmt-merge-msg", "-F", ".git/FETCH_HEAD"],
        &["fmt-merge-msg", "--log", "-F", ".git/FETCH_HEAD"],
        &["fmt-merge-msg", "-m", "custom", "-F", ".git/FETCH_HEAD"],
    ];
    run(
        |fx| {
            tags(fx).git([
                "fetch", "-q", ".", "tag", "t1", "tag", "t2", "tag", "tempty",
            ])
        },
        None,
        cases,
    );
    run(
        |fx| {
            tags(fx)
                .git(["merge", "-q", "--ff-only", "x"])
                .git(["fetch", "-q", ".", "x", "y"])
        },
        None,
        cases,
    );
    run(
        |fx| {
            tags(fx)
                .git(["merge", "-q", "--ff-only", "x"])
                .git(["fetch", "-q", ".", "x", "tag", "t1"])
        },
        None,
        cases,
    );
    run(tags, None, &[&["merge", "-q", "--no-edit", "t1", "t2"]]);
}
