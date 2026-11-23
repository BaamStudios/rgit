// SPDX-License-Identifier: MIT
//! Differential tests of `rgit blame` and `rgit annotate` against the
//! reference git: a history with a rename, merges (also one whose result
//! equals the second parent), several authors, an empty file, a file
//! without final newline, and work tree changes.

use rgit_difftest::{Check, Fixture, Normalizer};

fn commit(fx: Fixture, author: &str, msg: &str) -> Fixture {
    fx.tick()
        .env("GIT_AUTHOR_NAME", author)
        .git(["commit", "-q", "-a", "-m", msg])
}

fn history() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    if fx.is_skipped() {
        return fx;
    }
    fx = fx
        .write("f", "a+b\naab\nx\ny\nz\n1\n2\n3\n4\n5\n6\n")
        .write("ten", "1\n2\n3\n4\n5\n6\n7\n8\n9\na\n")
        .write("empty", "")
        .write("nonl", "no\nnewline")
        .write(
            "h.c",
            "int main(void)\n{\n\treturn 0;\n}\nstatic int foo(int x)\n{\n\treturn x;\n}\n",
        )
        .git(["add", "."]);
    fx = commit(fx, "A U Thor", "subject line\nsecond line\n\nbody");
    fx = fx
        .git(["mv", "f", "g"])
        .write("g", "a+b\naab\nx\ny\nz\n1\n2\n3\n4\n5\n6\nz\n");
    fx = commit(fx, "Ren", "rename");
    fx = fx
        .git(["checkout", "-q", "-b", "side"])
        .write("g", "a+b\naab\nx\ny\nz\n1\n2\n3\n4\n5\n6\nz\nside\n");
    fx = fx.write("ten", "1\n2\nthree\n4\n5\n6\n7\n8\n9\na\n");
    fx = commit(fx, "Side", "side");
    fx = fx
        .git(["checkout", "-q", "main"])
        .write("g", "top\na+b\naab\nx\ny\nz\n1\n2\n3\n4\n5\n6\nz\n");
    fx = fx.write("ten", "1\n2\n3\n4\n5\n6\nseven\n8\n9\na\n");
    fx = commit(fx, "M\u{e4}in \u{dc}", "main");
    fx = fx.tick().git(["merge", "-q", "--no-edit", "side"]);
    fx = fx.write(
        "h.c",
        "int main(void)\n{\n\treturn 0;\n}\nstatic int foo(int x)\n{\n\treturn x + 1;\n}\n",
    );
    fx = commit(fx, "H", "h");
    // A merge whose result is the second parent's version.
    fx = fx
        .git(["checkout", "-q", "-b", "p2"])
        .write("ten", "1\n2\nthree\n4\n5\n6\nseven\n8\n9\na\np2\n");
    fx = commit(fx, "P2", "p2");
    fx = fx
        .git(["checkout", "-q", "main"])
        .write("ten", "one\n2\nthree\n4\n5\n6\nseven\n8\n9\na\n");
    fx = commit(fx, "P1", "p1");
    fx = fx
        .tick()
        .git(["merge", "-q", "--no-edit", "-s", "ours", "p2"])
        .git(["checkout", "-q", "p2", "--", "ten"])
        .git(["commit", "-q", "--amend", "--no-edit"])
        .git(["tag", "t1", "HEAD~2"]);
    fx
}

fn compare(fx: &mut Fixture, cmd: &str, args: &[&str]) {
    let mut argv = vec![cmd];
    argv.extend_from_slice(args);
    fx.compare(&argv).checks(Check::OUTPUT).run();
}

#[test]
fn blame_formats_and_options() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["g"],
        &["ten"],
        &["h.c"],
        &["empty"],
        &["nonl"],
        &["-p", "g"],
        &["--line-porcelain", "g"],
        &["-p", "ten"],
        &["--incremental", "g"],
        &["--incremental", "ten"],
        &["-c", "g"],
        &["-n", "-f", "g"],
        &["-s", "g"],
        &["-s", "-n", "ten"],
        &["-e", "g"],
        &["-t", "g"],
        &["-l", "g"],
        &["-b", "g"],
        &["--root", "g"],
        &["--abbrev=4", "g"],
        &["--abbrev=12", "g"],
        &["--no-abbrev", "g"],
        &["--abbrev=40", "t1..", "--", "g"],
        &["--abbrev=41", "t1..", "--", "g"],
        &["--abbrev=9000", "^HEAD", "--", "g"],
        &["-c", "--abbrev=9000", "t1..", "--", "g"],
        &["--minimal", "--diff-algorithm", "myers", "g"],
        &["--diff-algorithm=myers", "--minimal", "g"],
        &["--minimal", "--diff-algorithm=patience", "g"],
        &["--date=short", "g"],
        &["--date=relative", "g"],
        &["--date=default", "g"],
        &["--color-lines", "ten"],
        &["--color-by-age", "ten"],
        &["-w", "g"],
        &["--diff-algorithm=patience", "g"],
        &["--first-parent", "ten"],
        &["-p", "--first-parent", "ten"],
        &["HEAD~1", "--", "g"],
        &["g", "HEAD~1"],
        &["HEAD~1", "g"],
        &["t1..", "--", "g"],
        &["HEAD~3..HEAD~1", "--", "g"],
        &["^HEAD~2", "--", "ten"],
        &["nope"],
        &["HEAD", "nope"],
        &["HEAD", "HEAD", "--", "g"],
        &["--bogus", "g"],
        &["--diff-algorithm=foo", "g"],
        &["--progress", "-p", "g"],
        &[],
        &["-h"],
    ];
    for args in cases {
        compare(&mut fx, "blame", args);
    }
    for args in [&["g"][..], &["ten"], &["-h"]] {
        compare(&mut fx, "annotate", args);
    }
}

#[test]
fn blame_line_ranges() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["-L", "2,4", "g"],
        &["-L", "4,2", "g"],
        &["-L", "/x/,+2", "g"],
        &["-L", "/a+b/,+1", "g"],
        &["-L", "3,-2", "g"],
        &["-L", ",3", "g"],
        &["-L", "12", "g"],
        &["-L", "1,99", "g"],
        &["-L", ":main", "h.c"],
        &["-L", ":foo", "h.c"],
        &["-L", "1,2", "-L", "9,10", "ten"],
        &["-L", "9,10", "-L", "1,2", "ten"],
        &["-L", "2,3", "-L", "3,5", "ten"],
        &["-L", "3,3", "-L", "/[0-9]/", "ten"],
        &["-L", "3,3", "-L", "^/[0-9]/,+1", "ten"],
        &["-L", "X", "g"],
        &["-L", "1,Y", "g"],
        &["-L", "1,+", "g"],
        &["-L", "0", "g"],
        &["-L", ",0", "g"],
        &["-L", "1,+0", "g"],
        &["-L", "99", "g"],
        &["-L", "/nomatch/", "g"],
        &["-L", "1,/nomatch/", "g"],
        &["-L", ":nomatch", "h.c"],
        &["-L", "1", "empty"],
    ];
    for args in cases {
        compare(&mut fx, "blame", args);
    }
}

#[test]
fn blame_work_tree() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    fx = fx
        .write(
            "g",
            "top\na+b\nchanged\nx\ny\nz\n1\n2\n3\n4\n5\n6\nz\nside\nnew\n",
        )
        .write("added", "new\nfile\n")
        .git(["add", "added"])
        .git(["mv", "h.c", "moved.c"])
        .write("contents", "top\nother\n");
    let times = Normalizer::drop_lines("^(author|committer)-(time|tz) ");
    for args in [
        &["-s", "g"][..],
        &["-s", "added"],
        &["-s", "-f", "moved.c"],
        &["-s", "--contents", "contents", "g"],
        &["-s", "--contents", "contents", "HEAD~1", "--", "g"],
        &["-s", "untracked"],
        &["-s", "HEAD", "--", "g"],
    ] {
        compare(&mut fx, "blame", args);
    }
    for args in [&["-p", "g"][..], &["-p", "--contents", "contents", "g"]] {
        let mut argv = vec!["blame"];
        argv.extend_from_slice(args);
        fx.compare(&argv)
            .checks(Check::OUTPUT)
            .normalize_stdout(times.clone())
            .run();
    }
    fx = fx.remove("g");
    compare(&mut fx, "blame", &["g"]);
}

/// `-M` and `-C`: blocks moved within a file and copied from other files
/// (changed in the same commit, any file of the commit creating the file,
/// any file), with thresholds.
#[test]
fn blame_moves_and_copies() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    if fx.is_skipped() {
        return;
    }
    fx = fx
        .write("one", "alpha line one\nalpha line two\nalpha line three\n")
        .write("two", "beta first\nbeta second\nbeta third\nbeta fourth\n")
        .write("keep", "kept 1\nkept 2\nkept 3\n")
        .write("m", "head\nab\ncd\nmiddle\ntail end\n")
        .git(["add", "."]);
    fx = commit(fx, "Initial", "initial");
    fx = fx
        .write("m", "head\nmiddle\ntail end\nab\ncd\n")
        .write("two", "beta first\nbeta fourth\n")
        .write(
            "three",
            "new\nbeta second\nbeta third\nalpha line two\nkept 2\n",
        )
        .write(
            "one",
            "alpha line one\nalpha line two\nalpha line three\nmore\n",
        )
        .git(["add", "."]);
    fx = commit(fx, "Second", "second");
    fx = fx
        .write(
            "one",
            "alpha line one\nalpha line two\nalpha line three\nmore\nkept 1\nkept 2\n",
        )
        .git(["mv", "two", "renamed"]);
    fx = commit(fx, "Third", "third");
    let cases: &[&[&str]] = &[
        &["-M", "m"],
        &["-M4", "m"],
        &["-M5", "m"],
        &["-Mx", "-s", "m"],
        &["-M4", "-C", "m"],
        &["-C", "-n", "-f", "three"],
        &["-C", "-C", "-n", "-f", "three"],
        &["-C", "-C", "-C", "-n", "-f", "three"],
        &["-C1", "-C", "-p", "three"],
        &["-C", "-C", "-C1", "--line-porcelain", "three"],
        &["-C", "-C", "-C", "one"],
        &["-C", "-C", "renamed"],
        &["-w", "-C", "-C", "-C1", "three"],
    ];
    for args in cases {
        compare(&mut fx, "blame", args);
    }
}
