// SPDX-License-Identifier: MIT
//! Differential tests of `rgit merge-file` against the reference git.

use rgit_difftest::{Check, Fixture};

fn lines(words: &str) -> String {
    words.split_whitespace().map(|w| format!("{w}\n")).collect()
}

fn files(fx: Fixture) -> Fixture {
    fx.write("base", lines("a b c d e f g h i j"))
        .write("ours", lines("a B c d e f g H i j k"))
        .write("theirs", lines("a X c d e f g Y i j"))
        .write("same", lines("a B c d e f g h i j"))
        .write("clean", lines("a b c d e f g h i J"))
        .write("crlf-base", "1\r\n2\r\n3")
        .write("crlf-ours", "1\r\n2\r\n4")
        .write("crlf-theirs", "1\r\n2\r\n5\r\n")
        .write("binary", "a\0b\n")
        .write("sub/ours", lines("x y z"))
        .write("sub/base", lines("x b z"))
        .write("sub/theirs", lines("x b Z"))
}

fn fixture() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    files(fx)
}

#[test]
fn merge_file_output() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["-p", "ours", "base", "theirs"],
        &["--stdout", "--diff3", "ours", "base", "theirs"],
        &["-p", "--zdiff3", "ours", "base", "theirs"],
        &["-p", "--diff3", "--no-diff3", "ours", "base", "theirs"],
        &["-p", "--zdiff3", "--diff3", "ours", "base", "theirs"],
        &["-p", "--ours", "ours", "base", "theirs"],
        &["-p", "--theirs", "ours", "base", "theirs"],
        &["-p", "--union", "ours", "base", "theirs"],
        &["-p", "--union", "--no-union", "ours", "base", "theirs"],
        &["-p", "--marker-size=3", "--diff3", "ours", "base", "theirs"],
        &["-p", "--marker-size", "0", "ours", "base", "theirs"],
        &["-p", "-L", "mine", "ours", "base", "theirs"],
        &[
            "-p", "-L", "x", "-L", "", "-L", "z y", "--diff3", "ours", "base", "theirs",
        ],
        &[
            "-p", "-L", "1", "-L", "2", "-L", "3", "-L", "4", "ours", "base", "theirs",
        ],
        &["-p", "--diff-algorithm=minimal", "ours", "base", "theirs"],
        &["-p", "--diff-algorithm", "Myers", "ours", "base", "theirs"],
        &["-p", "--diff-algorithm=bogus", "ours", "base", "theirs"],
        &["-p", "--diff-algorithm=patience", "ours", "base", "theirs"],
        &[
            "-p",
            "--diff3",
            "--diff-algorithm=patience",
            "ours",
            "base",
            "theirs",
        ],
        &["-p", "--diff-algorithm=histogram", "ours", "base", "theirs"],
        &["-p", "ours", "base", "same"],
        &["-p", "ours", "base", "clean"],
        &["-p", "crlf-ours", "crlf-base", "crlf-theirs"],
        &["-p", "--diff3", "crlf-ours", "crlf-base", "crlf-theirs"],
        &["-p", "--union", "crlf-ours", "crlf-base", "crlf-theirs"],
        &["-p", "ours", "binary", "theirs"],
        &["-p", "-q", "ours", "binary", "theirs"],
        &["-p", "binary", "missing", "theirs"],
        &["-p", "ours", "missing", "theirs"],
        &["-p", "ours", "sub", "theirs"],
        &["-p", "ours", "base"],
        &["-h"],
    ];
    for args in cases {
        let mut argv = vec!["merge-file"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
    fx.compare(["merge-file", "-p", "ours", "base", "theirs"])
        .in_dir("sub")
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn merge_file_writes_result() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx.same(["merge-file", "ours", "base", "theirs"]).same([
        "merge-file",
        "-q",
        "same",
        "base",
        "clean",
    ]);
    fx.compare(["merge-file", "ours", "base", "theirs"])
        .in_dir("sub")
        .run();
}

#[test]
fn merge_file_conflict_style_config() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["-c", "merge.conflictStyle=diff3"],
        &["-c", "merge.conflictStyle=zdiff3"],
        &["-c", "merge.conflictStyle=merge"],
        &["-c", "merge.conflictStyle=bogus"],
        &["-c", "merge.conflictStyle"],
    ];
    for c in cases {
        let mut argv = c.to_vec();
        argv.extend(["merge-file", "-p", "ours", "base", "theirs"]);
        fx.compare(&argv).checks(Check::OUTPUT).run();
        argv.insert(argv.len() - 3, "--no-diff3");
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
    let mut fx = fx.git(["config", "merge.conflictStyle", "nope"]);
    fx.compare(["merge-file", "-p", "ours", "base", "theirs"])
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn merge_file_object_id() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx
        .git(["add", "ours", "base", "theirs", "binary"])
        .git(["commit", "-q", "-m", "files"]);
    let cases: &[&[&str]] = &[
        &["-p", "--object-id", ":ours", ":base", ":theirs"],
        &[
            "-p",
            "--object-id",
            "-L",
            "a",
            "HEAD:ours",
            ":base",
            ":theirs",
        ],
        &["--object-id", ":ours", ":base", ":theirs"],
        &["--object-id", ":base", ":base", ":base"],
        &["-p", "--object-id", ":ours", ":nope", ":theirs"],
        &["-p", "--object-id", ":ours", "HEAD", ":theirs"],
        &["-p", "--object-id", ":ours", ":binary", ":theirs"],
        // The empty blob is known without being stored.
        &[
            "-p",
            "--object-id",
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
            ":theirs",
        ],
    ];
    for args in cases {
        let mut argv = vec!["merge-file"];
        argv.extend_from_slice(args);
        fx.compare(&argv).run();
    }
}

#[test]
fn merge_file_object_id_outside_repository() {
    let mut fx = files(Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit")));
    if fx.is_skipped() {
        return;
    }
    fx.compare(["merge-file", "--object-id", "a", "b", "c"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["merge-file", "-p", "ours", "base", "theirs"])
        .checks(Check::OUTPUT)
        .run();
}
