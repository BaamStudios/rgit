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
    ] {
        fx.compare(args).run();
    }
}
