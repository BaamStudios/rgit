// SPDX-License-Identifier: MIT
//! Differential tests of `diff-tree --stdin` and of the commit forms with
//! `-v`, `--pretty`, `--format` and `--always` against the reference git.

use rgit_difftest::{Check, Fixture};

/// one -> two -> three, side branch from one, merged.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("dir/sub", "s\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .write("a", "a\nb\n")
        .git(["commit", "-q", "-a", "-m", "two"])
        .tick()
        .git(["checkout", "-q", "-b", "side", "HEAD~1"])
        .write("dir/sub", "s\nx\n")
        .git(["commit", "-q", "-a", "-m", "side"])
        .tick()
        .git(["checkout", "-q", "main"])
        .write("c", "c\n")
        .git(["add", "c"])
        .git(["commit", "-q", "-m", "three"])
        .tick()
        .git(["merge", "-q", "--no-edit", "side"])
}

fn stdout(fx: &Fixture, args: &[&str]) -> String {
    String::from_utf8(fx.git_stdout(args)).unwrap()
}

#[test]
fn stdin_forms_match_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let commits = stdout(&fx, &["rev-list", "HEAD"]);
    let t1 = stdout(&fx, &["rev-parse", "HEAD~3^{tree}"]);
    let t2 = stdout(&fx, &["rev-parse", "HEAD^{tree}"]);
    let first = stdout(&fx, &["rev-parse", "HEAD~1"]);
    let root = stdout(&fx, &["rev-parse", "HEAD~3"]);
    let tag_target = stdout(&fx, &["rev-parse", "HEAD:a"]);
    let trees = format!("{} {}\n", t1.trim(), t2.trim());
    let with_parent = format!("{} {}\n", first.trim(), root.trim());
    let mixed = format!("\nnot an id\n{}{}", first, tag_target);
    let inputs: &[(&str, &[&str])] = &[
        (&trees, &["diff-tree", "--stdin"]),
        (&trees, &["diff-tree", "-r", "--stdin"]),
        (&trees, &["diff-tree", "-p", "--stdin"]),
        (&with_parent, &["diff-tree", "--stdin"]),
        (&mixed, &["diff-tree", "--stdin"]),
        (&commits, &["diff-tree", "--stdin"]),
        (&commits, &["diff-tree", "--stdin", "-p", "--root"]),
        (&commits, &["diff-tree", "--stdin", "-m"]),
        (&commits, &["diff-tree", "--stdin", "-c"]),
        (&commits, &["diff-tree", "--stdin", "--cc"]),
        (&commits, &["diff-tree", "--stdin", "-s"]),
        (&commits, &["diff-tree", "--stdin", "--exit-code"]),
        (&commits, &["diff-tree", "--stdin", "--no-commit-id", "-r"]),
        (
            &commits,
            &["diff-tree", "--stdin", "--name-only", "--", "dir"],
        ),
        (
            &commits,
            &["diff-tree", "--stdin", "--always", "--", "nothing"],
        ),
        (&commits, &["diff-tree", "--stdin", "-v"]),
        (&commits, &["diff-tree", "--stdin", "-v", "--root", "-p"]),
        (&commits, &["diff-tree", "--stdin", "--pretty=oneline"]),
        (&commits, &["diff-tree", "--stdin", "--format=%s", "-s"]),
        (
            &commits,
            &[
                "diff-tree",
                "-r",
                "--stdin",
                "--name-only",
                "--format=%s",
                "dir",
            ],
        ),
    ];
    let mut failures = Vec::new();
    for (input, argv) in inputs {
        let r = fx
            .compare(*argv)
            .stdin(input.as_bytes().to_vec())
            .checks(Check::OUTPUT)
            .try_run();
        if let Err(m) = r {
            failures.push(format!("{argv:?}\n{}", m.report()));
        }
    }
    // The first commit with a change: one header line, exit code 1.
    let head_only = first.clone();
    if let Err(m) = fx
        .compare(["diff-tree", "--stdin", "--quiet"])
        .stdin(head_only.into_bytes())
        .checks(Check::OUTPUT)
        .try_run()
    {
        failures.push(m.report().to_owned());
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn commit_forms_with_pretty_match_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["diff-tree", "--pretty", "HEAD~1"],
        &["diff-tree", "--pretty", "-p", "HEAD~1"],
        &["diff-tree", "--pretty", "--stat", "--summary", "HEAD~1"],
        &["diff-tree", "--pretty", "--patch-with-stat", "HEAD~1"],
        &[
            "diff-tree",
            "--pretty=oneline",
            "--patch-with-raw",
            "HEAD~1",
        ],
        &["diff-tree", "--pretty", "--root", "HEAD~3"],
        &["diff-tree", "--pretty", "--root", "-r", "HEAD~3"],
        &["diff-tree", "--pretty", "HEAD~3"],
        &["diff-tree", "--pretty", "HEAD"],
        &["diff-tree", "--pretty", "-c", "HEAD"],
        &["diff-tree", "--pretty", "--cc", "--stat", "HEAD"],
        &["diff-tree", "-v", "HEAD~1"],
        &["diff-tree", "--format=%s%n%an", "HEAD~1"],
        &["diff-tree", "--quiet", "HEAD~1"],
        &["diff-tree", "--exit-code", "HEAD~1"],
        &["diff-tree", "--always", "HEAD~1", "--", "nothing"],
        &["diff-tree", "HEAD~1", "--", "nothing"],
    ];
    let mut failures = Vec::new();
    for argv in cases {
        if let Err(m) = fx.compare(*argv).checks(Check::OUTPUT).try_run() {
            failures.push(format!("{argv:?}\n{}", m.report()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
