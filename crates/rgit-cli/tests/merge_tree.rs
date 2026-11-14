// SPDX-License-Identifier: MIT
//! Differential tests of `rgit merge-tree --write-tree` against the
//! reference git: result tree, conflicted file info, messages and exit
//! code. Renames are disabled on both sides (rename detection is #151).

use rgit_difftest::{Check, Fixture};

const LINES: &str = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";

/// `base` with one file per kind of change; `side` and `main` change
/// them so that every kind of conflict occurs once. `clean` changes only
/// what `main` does not touch. `x1`/`x2` form a criss-cross history.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("content", LINES)
        .write("clean", LINES)
        .write("md", "a\n")
        .write("dm", "a\n")
        .write("df", "x\n")
        .write("bin", "l\n")
        .write("mode", "k\n")
        .write("type", "t\n")
        .write("dir/sub/f", "f\n")
        .write("sp ace", "s\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "base"])
        .git(["tag", "base"])
        .tick()
        .git(["checkout", "-q", "-b", "side"])
        .write("content", "1\n2\n3\n4\n5\n6\n7\n8\nT\n")
        .write("clean", "C\n2\n3\n4\n5\n6\n7\n8\n9\n")
        .write("md", "b\n")
        .remove("dm")
        .remove("df")
        .write("df/f", "in\n")
        .write("bin", "T\0x")
        .write_executable("mode", "k\n")
        .write("added", "theirs\n")
        .write("type", "t2\n")
        .write("dir/sub/f", "side\n")
        .write("sp ace", "side\n")
        .git(["add", "-A"])
        .git(["commit", "-q", "-m", "side"])
        .tick()
        .git(["checkout", "-q", "-b", "clean", "base"])
        .write("content", "O\n2\n3\n4\n5\n6\n7\n8\n9\n")
        .git(["commit", "-q", "-a", "-m", "clean"])
        .tick()
        .git(["checkout", "-q", "main"])
        .write("content", "O\n2\n3\n4\n5\n6\n7\n8\n9\n")
        .remove("md")
        .write("dm", "c\n")
        .write("df", "y\n")
        .write("bin", "O\0x")
        .write("added", "ours\n")
        .remove("type")
        .symlink("target", "type")
        .remove("dir")
        .write("dir", "file now\n")
        .write("sp ace", "main\n")
        .git(["add", "-A"])
        .git(["commit", "-q", "-m", "main"])
        .tick()
        // Criss-cross: x1 and x2 both merge the other's first commit and
        // resolve the conflict differently.
        .git(["checkout", "-q", "-b", "x1", "base"])
        .write("content", "1\nX1\n3\n4\n5\n6\n7\n8\n9\n")
        .git(["commit", "-q", "-a", "-m", "x1"])
        .tick()
        .git(["checkout", "-q", "-b", "x2", "base"])
        .write("content", "1\nX2\n3\n4\n5\n6\n7\n8\n9\n")
        .git(["commit", "-q", "-a", "-m", "x2"])
        .tick()
        .git(["checkout", "-q", "-b", "y1", "x1"])
        .git(["merge", "-q", "-s", "ours", "-m", "y1", "x2"])
        .write("content", "1\nY1\n3\n4\n5\n6\n7\n8\n9\n")
        .git(["commit", "-q", "-a", "-m", "y1 resolution"])
        .tick()
        .git(["checkout", "-q", "-b", "y2", "x2"])
        .git(["merge", "-q", "-s", "ours", "-m", "y2", "x1"])
        .write("content", "1\nY2\n3\n4\n5\n6\n7\n8\n9\n")
        .git(["commit", "-q", "-a", "-m", "y2 resolution"])
        .tick()
        .git(["checkout", "-q", "--orphan", "unrelated"])
        .git(["rm", "-rqf", "."])
        .write("content", "u\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "unrelated"])
        .git(["checkout", "-q", "-f", "main"])
}

fn compare(fx: &mut Fixture, args: &[&str]) {
    let mut argv = vec!["merge-tree"];
    argv.extend_from_slice(args);
    fx.compare(&argv).checks(Check::OUTPUT).run();
}

#[test]
fn merge_tree_conflicts_and_output_formats() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["-Xno-renames", "main", "side"],
        &["--write-tree", "-Xno-renames", "side", "main"],
        &["-Xno-renames", "-z", "main", "side"],
        &["-Xno-renames", "--name-only", "main", "side"],
        &["-Xno-renames", "--name-only", "-z", "main", "side"],
        &["-Xno-renames", "--no-messages", "main", "side"],
        &["-Xno-renames", "--quiet", "main", "side"],
        &["-Xno-renames", "-Xours", "main", "side"],
        &["-Xno-renames", "-Xtheirs", "main", "side"],
        &["-Xno-renames", "main", "clean"],
        &["-Xno-renames", "--messages", "side", "clean"],
        &["-Xno-renames", "--messages", "-z", "side", "clean"],
        &["-Xno-renames", "--quiet", "side", "clean"],
        &["-Xno-renames", "main", "main"],
        &["-Xno-renames", "main", "base"],
        &["-Xno-renames", "--merge-base=base", "main", "side"],
        &[
            "-Xno-renames",
            "--merge-base=base^{tree}",
            "main^{tree}",
            "side^{tree}",
        ],
        &["-Xno-renames", "y1", "y2"],
        &["-Xno-renames", "-z", "y2", "y1"],
    ];
    for args in cases {
        compare(&mut fx, args);
    }
    for style in ["diff3", "zdiff3"] {
        let config = format!("merge.conflictStyle={style}");
        for branches in [["main", "side"], ["y1", "y2"]] {
            let mut argv = vec!["-c", config.as_str(), "merge-tree", "-Xno-renames"];
            argv.extend_from_slice(&branches);
            fx.compare(&argv).checks(Check::OUTPUT).run();
            // The conflicted file itself.
            let tree = String::from_utf8(fx.git_output(&argv).stdout).unwrap();
            let tree = tree.lines().next().unwrap();
            fx.compare(["cat-file", "-p", &format!("{tree}:content")])
                .checks(Check::OUTPUT)
                .run();
        }
    }
}

#[test]
fn merge_tree_errors() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["main"],
        &["a", "b", "c", "d"],
        &["--trivial-merge", "main", "side"],
        &["--write-tree", "--trivial-merge", "main", "side"],
        &["--quiet", "--name-only", "main", "side"],
        &["--no-quiet", "main", "side"],
        &["--stdin", "--merge-base=base", "main", "side"],
        &["nope", "side"],
        &["main", "main:content"],
        &["main", "main^{tree}"],
        &["--merge-base=nope", "main", "side"],
        &["-Xbogus", "main", "side"],
        &["-Xfind-renames=x", "main", "side"],
        &["-Xno-renames", "main", "unrelated"],
        &[
            "-Xno-renames",
            "--allow-unrelated-histories",
            "main",
            "unrelated",
        ],
        &["-h"],
    ];
    for args in cases {
        compare(&mut fx, args);
    }
}
