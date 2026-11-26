// SPDX-License-Identifier: MIT
//! Differential tests of path-limited `rev-list` and `log` against the
//! reference git: the example history of the "History Simplification"
//! section of Documentation/rev-list-options.adoc in every mode,
//! `--follow`, and telling paths from revisions.

use rgit_difftest::{Check, Fixture};

fn merge(fx: Fixture, branch: &str) -> Fixture {
    fx.git([
        "merge",
        "-q",
        "--no-ff",
        "--no-commit",
        "-s",
        "ours",
        "--allow-unrelated-histories",
        branch,
    ])
}

fn commit(fx: Fixture, msg: &str) -> Fixture {
    fx.git(["add", "-A"])
        .git(["commit", "-q", "--allow-empty", "-m", msg])
        .git(["tag", msg])
        .tick()
}

/// ```text
///   .-A---M---N---O---P---Q
///  /     /   /   /   /   /
/// I     B   C   D   E   Y
///  \   /   /   /   /   /
///   `-------------'   X
/// ```
/// `foo` is the path of interest.
fn example() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("foo", "asdf\n")
        .write("quux", "quux\n");
    fx = commit(fx, "I");
    for (branch, file, content) in [
        ("b", "foo", "foo\n"),
        ("c", "other", "c\n"),
        ("d", "foo", "baz\n"),
        ("e", "quux", "xyzzy\n"),
    ] {
        fx = fx
            .git(["checkout", "-q", "-b", branch, "I"])
            .write(file, content);
        fx = commit(fx, &branch.to_uppercase());
    }
    fx = fx
        .git(["checkout", "-q", "--orphan", "x"])
        .git(["rm", "-rfq", "."])
        .write("side", "side\n");
    fx = commit(fx, "X");
    fx = fx.write("side", "side 2\n");
    fx = commit(fx, "Y");
    fx = fx.git(["checkout", "-q", "main"]).write("foo", "foo\n");
    fx = commit(fx, "A");
    fx = merge(fx, "B");
    fx = commit(fx, "M");
    fx = merge(fx, "C").write("foo", "foobar\n");
    fx = commit(fx, "N");
    let fx = merge(fx, "D");
    let fx = fx.write("foo", "foobarbaz\n");
    let fx = commit(fx, "O");
    let fx = merge(fx, "E").write("quux", "xyzzy\n");
    let fx = commit(fx, "P");
    let fx = merge(fx, "Y").write("side", "side 2\n");
    commit(fx, "Q")
}

fn same(fx: &mut Fixture, args: &[&str]) {
    fx.compare(args).checks(Check::OUTPUT).run();
}

#[test]
fn documentation_example_in_every_mode() {
    let mut fx = example();
    let modes: &[&[&str]] = &[
        &[],
        &["--full-history"],
        &["--full-history", "--parents"],
        &["--simplify-merges"],
        &["--dense"],
        &["--sparse"],
        &["--full-history", "--sparse"],
        &["--show-pulls"],
        &["--simplify-merges", "--show-pulls"],
        &["--first-parent"],
        &["--remove-empty"],
        &["--ancestry-path", "B..Q"],
        &["--ancestry-path", "--simplify-merges", "B..Q"],
        &["--topo-order", "--parents"],
        &["--date-order", "--full-history", "--parents"],
        &["--children", "--full-history"],
        &["--boundary", "A..Q"],
        &["--reverse", "--parents"],
        &["-n", "2", "--parents"],
        &["--simplify-by-decoration"],
        &["--bisect-all", "I..Q"],
    ];
    for mode in modes {
        let mut args = vec!["log", "--format=%s %p"];
        args.extend_from_slice(mode);
        if !mode.iter().any(|a| a.contains("..")) {
            args.push("Q");
        }
        if mode.contains(&"--bisect-all") {
            args[0] = "rev-list";
            args.remove(1);
        }
        args.extend(["--", "foo"]);
        same(&mut fx, &args);
    }
    same(
        &mut fx,
        &["rev-list", "--parents", "Q", "--", "foo", "side"],
    );
    same(&mut fx, &["rev-list", "--parents", "--all", "--", "nope"]);
    same(&mut fx, &["log", "-p", "--format=%s", "Q", "--", "foo"]);
    same(
        &mut fx,
        &[
            "log",
            "--stat",
            "--full-diff",
            "--format=%s",
            "Q",
            "--",
            "foo",
        ],
    );
    same(&mut fx, &["rev-list", "--objects", "Q", "--", "foo"]);
    same(&mut fx, &["rev-list", "--no-walk", "A", "C", "--", "foo"]);
    same(
        &mut fx,
        &["log", "--format=%s", "--simplify-by-decoration", "--all"],
    );
}

#[test]
fn paths_without_dashdash() {
    let mut fx = example();
    for args in [
        &["log", "--format=%s", "foo"][..],
        &["log", "--format=%s", "Q", "foo", "quux"],
        &["log", "--format=%s", "foo", "Q"],
        &["log", "--format=%s", "*oo"],
        &["log", "--format=%s", ":/foo"],
        &["log", "--format=%s", "nope"],
        &["log", "--format=%s", "nope", "--", "foo"],
        &["rev-list", "foo"],
        &["rev-list", "HEAD", "foo"],
        &["log", "--format=%s", "-n1", "foo"],
    ] {
        fx.compare(args).run();
    }
    // A tag named like a file is ambiguous without `--`.
    let mut fx = fx.git(["tag", "foo", "A"]);
    fx.compare(["log", "--format=%s", "foo"]).run();
    fx.compare(["log", "--format=%s", "foo", "--"]).run();
}

#[test]
fn follow_renames_and_copies() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("f", "l1\nl2\nl3\nl4\nl5\n");
    fx = commit(fx, "base");
    fx = fx
        .git(["checkout", "-q", "-b", "side"])
        .write("f", "l1\nl2\nl3 side\nl4\nl5\n");
    fx = commit(fx, "S");
    fx = fx.git(["checkout", "-q", "main"]).write("g", "g\n");
    fx = commit(fx, "G");
    fx = fx
        .git(["merge", "-q", "-s", "ours", "-m", "M", "side"])
        .tick();
    fx = fx.git(["mv", "f", "h"]);
    fx = commit(fx, "R");
    fx = fx.write("h", "l1\nl2\nl3\nl4\nl5 changed\n");
    fx = commit(fx, "C");
    fx = fx.write("k", "l1\nl2\nl3\nl4\nl5 changed\n");
    fx = commit(fx, "K");
    for args in [
        &["log", "--format=%s", "--follow", "h"][..],
        &["log", "--format=%s", "--follow", "--", "k"],
        &["log", "--format=%s", "--follow", "--first-parent", "h"],
        &["log", "--format=%s", "--follow", "--reverse", "h"],
        &["log", "--format=%s", "--follow", "-n", "2", "h"],
        &["log", "--format=%s", "--follow", "--no-follow", "h"],
        &["log", "--format=%s", "--follow", "*"],
        &["log", "--format=%s", "--follow"],
        &["log", "--format=%s", "--follow", "h", "k"],
        &["-c", "log.follow=true", "log", "--format=%s", "h"],
        &["-c", "log.follow=true", "log", "--format=%s", "h", "k"],
        // The diffs show the followed file with its source.
        &["log", "--format=%s", "--follow", "--name-status", "h"],
        &["log", "--format=%s", "--follow", "--name-status", "k"],
        &["log", "--format=%s", "--follow", "--stat", "-p", "k"],
        &[
            "log",
            "--format=%s",
            "--follow",
            "--raw",
            "--no-renames",
            "h",
        ],
        &["log", "--format=%s", "--follow", "--full-diff", "h"],
    ] {
        fx.compare(args).run();
    }
}

#[test]
fn log_merge_shows_commits_touching_conflicts() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "0\n")
        .write("b", "0\n")
        .write("c", "0\n");
    fx = commit(fx, "base");
    fx = fx.git(["checkout", "-q", "-b", "side"]).write("a", "s\n");
    fx = commit(fx, "s1");
    fx = fx.write("b", "s\n");
    fx = commit(fx, "s2");
    fx = fx.write("c", "s\n");
    fx = commit(fx, "s3");
    fx = fx.git(["checkout", "-q", "main"]).write("a", "m\n");
    fx = commit(fx, "m1");
    fx = fx.write("c", "m\n");
    fx = commit(fx, "m2");
    fx = fx.write("d", "m\n");
    fx = commit(fx, "m3");
    let merge_args = [
        &["log", "--merge", "--format=%s"][..],
        &[
            "log",
            "--merge",
            "--format=%s",
            "--left-right",
            "--boundary",
        ],
        &["log", "--merge", "--format=%s", "--", "b"],
        &["log", "--merge", "--format=%s", "--", "a", "b"],
        &["log", "--merge", "--format=%s", "-p"],
        &["rev-list", "--merge", "HEAD"],
    ];
    for args in merge_args {
        fx.compare(args).run();
    }
    // Conflicts in a and c.
    assert!(!fx.git_output(["merge", "-q", "side"]).status.success());
    for args in merge_args {
        fx.compare(args).run();
    }
    fx = fx.git(["add", "a"]);
    fx.compare(["log", "--merge", "--format=%s"]).run();
}

/// Rare combinations: `--bisect*` when path limiting leaves no commit,
/// `--bisect-all --parents`, `--remove-empty --boundary` (also with
/// `--first-parent` across a merge that adds the path).
#[test]
fn bisect_and_remove_empty_edge_cases() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    for (file, content) in [("a", "1"), ("b", "1"), ("f", "1"), ("a", "2")] {
        fx = commit(fx.write(file, content), &format!("{file}{content}"));
    }
    fx = fx.git(["checkout", "-q", "-b", "side"]);
    for content in ["1", "2"] {
        fx = commit(fx.write("s", content), &format!("s{content}"));
    }
    fx = fx.git(["checkout", "-q", "main"]);
    for (file, content) in [("b", "2"), ("f", "2"), ("a", "3")] {
        fx = commit(fx.write(file, content), &format!("{file}{content}"));
    }
    fx = fx.git(["merge", "-q", "--no-ff", "-m", "m", "side"]).tick();
    fx = commit(fx.write("a", "4"), "a4");
    for args in [
        &["rev-list", "--bisect-vars", "HEAD", "--", "zz"][..],
        &[
            "rev-list",
            "--bisect-vars",
            "--bisect-all",
            "HEAD",
            "--",
            "zz",
        ],
        &["rev-list", "--bisect-vars", "HEAD~2..HEAD", "--", "f"],
        &["rev-list", "--bisect-vars", "HEAD", "^side", "--", "s"],
        &["rev-list", "--bisect", "--count", "HEAD", "--", "zz"],
        &["rev-list", "--bisect-vars", "HEAD..HEAD"],
        &["rev-list", "--bisect-all", "--parents", "HEAD", "--", "f"],
        &[
            "rev-list",
            "--bisect-all",
            "--full-history",
            "--parents",
            "HEAD",
            "--",
            "f",
        ],
        &[
            "rev-list",
            "--remove-empty",
            "--boundary",
            "HEAD",
            "--",
            "f",
        ],
        &[
            "rev-list",
            "--remove-empty",
            "--boundary",
            "HEAD",
            "--",
            "b",
        ],
        &[
            "rev-list",
            "--remove-empty",
            "--boundary",
            "--parents",
            "HEAD",
            "--",
            "f",
        ],
        &[
            "rev-list",
            "--remove-empty",
            "--boundary",
            "--full-history",
            "HEAD",
            "--",
            "s",
        ],
        &[
            "rev-list",
            "--remove-empty",
            "--boundary",
            "--first-parent",
            "HEAD",
            "--",
            "s",
        ],
        &[
            "rev-list",
            "--remove-empty",
            "--first-parent",
            "HEAD",
            "--",
            "s",
        ],
    ] {
        same(&mut fx, args);
    }
}
