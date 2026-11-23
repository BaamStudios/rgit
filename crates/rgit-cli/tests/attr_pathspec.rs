// SPDX-License-Identifier: MIT
//! `attr:` pathspec magic in add, status and the diff family,
//! differential against git.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write(
            ".gitattributes",
            "*.red color=red\n*.blue color=blue\nkeep* keep\n",
        )
        .write("one.red", "1\n")
        .write("two.blue", "2\n")
        .write("dir/three.red", "3\n")
        .write("dir/keep.blue", "4\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "base"])
        .tick()
        .write("one.red", "1\nchanged\n")
        .write("two.blue", "2\nchanged\n")
        .write("dir/three.red", "3\nchanged\n")
        .write("dir/keep.blue", "4\nchanged\n")
        .write("new.red", "new\n")
        .write("dir/new.blue", "new\n")
        .write_executable("run.blue", "#!/bin/sh\n")
}

#[test]
fn add_with_attr_magic() {
    let cases: &[&[&str]] = &[
        &["add", "--all", ":(exclude,attr:color=blue)"],
        &["add", "-u", ":(exclude,attr:color=blue)"],
        &["add", ":(attr:color=red)"],
        &["add", ":(attr:color=red)dir"],
        &["add", ".", ":(exclude,attr:keep)"],
        &["add", ":(exclude,attr:color=red)dir/*"],
        &["add", "-A", ":(attr:!color)"],
        &["add", "-A", ":(attr:builtin_objectmode=100755)"],
        &["add", "--dry-run", "-v", ":(attr:color=blue)"],
    ];
    for args in cases {
        let mut fx = fixture();
        fx.compare(*args)
            .checks([Check::ExitCode, Check::Stdout, Check::Stderr, Check::Index])
            .run();
    }
    let mut fx = fixture().write("list", ":(exclude,attr:color=blue)\n");
    fx.compare(["add", "--all", "--pathspec-from-file=list"])
        .checks([Check::ExitCode, Check::Stdout, Check::Stderr, Check::Index])
        .run();
    let mut fx = fixture();
    fx.compare(["add", ".", ":(exclude,attr:color=blue)"])
        .in_dir("dir")
        .checks([Check::ExitCode, Check::Stdout, Check::Stderr, Check::Index])
        .run();
}

#[test]
fn status_and_diff_with_attr_magic() {
    let mut fx = fixture()
        .git(["add", "dir/three.red"])
        .git(["update-index", "--add", "new.red"]);
    let cases: &[&[&str]] = &[
        &["status", "--porcelain", "--", ":(attr:color=red)"],
        &["status", "--porcelain", "--", ":(exclude,attr:color=red)"],
        &[
            "status",
            "--porcelain",
            "--",
            ":(exclude,attr:builtin_objectmode=100644)",
        ],
        &["status", "-s", "-uall", "--", ":(attr:color=blue)"],
        &["diff", "--name-only", "--", ":(attr:color=red)"],
        &["diff", "--name-only", "--", ":(exclude,attr:color=red)"],
        &["diff", "--cached", "--name-only", "--", ":(attr:color=red)"],
        &["diff", "--stat", "HEAD", "--", ":(attr:keep)"],
        &[
            "diff-files",
            "--name-only",
            "--",
            ":(exclude,attr:color=blue)",
        ],
        &[
            "diff-index",
            "--name-only",
            "HEAD",
            "--",
            ":(attr:color=red)",
        ],
        &[
            "diff-index",
            "--cached",
            "--name-only",
            "HEAD",
            "--",
            ":(exclude,attr:color=red)",
        ],
    ];
    for args in cases {
        fx.compare(*args).checks(Check::OUTPUT).run();
    }
    let mut fx = fixture().git(["commit", "-q", "-a", "-m", "second"]);
    for args in [
        &[
            "diff-tree",
            "-r",
            "--name-only",
            "HEAD^",
            "HEAD",
            "--",
            ":(attr:color=red)",
        ][..],
        &[
            "diff",
            "--name-only",
            "HEAD^",
            "HEAD",
            "--",
            ":(exclude,attr:color=red)",
        ],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}
