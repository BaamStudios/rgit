// SPDX-License-Identifier: MIT
//! Differential tests of `rgit add` against the reference git: output,
//! exit code, the resulting index and the object database.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("b", "b\n")
        .write("d/c", "c\n")
        .write(".gitignore", "*.log\nbuild/\n")
        .symlink("a", "lnk")
        .git(["add", "a", "b", "d/c", ".gitignore", "lnk"])
        .git(["commit", "-q", "-m", "init"])
        .tick()
        .write("a", "a2\n")
        .write("d/c", "c2\n")
        .write("d/new", "n\n")
        .write("e/f/g", "g\n")
        .write("top", "top\n")
        .write("x.log", "log\n")
        .write("build/out", "o\n")
        .remove("b")
}

const STATE: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Index,
    Check::Objects,
];

fn each(dir: &str, cases: &[&[&str]]) {
    for args in cases {
        let mut fx = fixture();
        let mut argv = vec!["add"];
        argv.extend_from_slice(args);
        fx.compare(&argv).in_dir(dir).checks(STATE).run();
    }
}

#[test]
fn paths() {
    each(
        "",
        &[
            &["a"],
            &["-v", "a", "d"],
            &["-v", "."],
            &["-n", "."],
            &["-v", "-A"],
            &["-v", "-u"],
            &["-v", "b"],
            &["-v", "e"],
            &["-v", "*.log"],
            &["-v", "x.log"],
            &["-f", "-v", "x.log"],
            &["nosuch"],
            &["-v", "d/*"],
            &[],
            &["-A", "-u"],
        ],
    );
}

#[test]
fn subdirectory() {
    each(
        "d",
        &[&["-v", "."], &["-v", "new"], &["-v", "-u"], &["-v", "-A"]],
    );
}

#[test]
fn intent_to_add() {
    each(
        "",
        &[
            &["-N", "top"],
            &["-N", "-v", "top", "d"],
            &["-N", "-n", "."],
            &["-N", "-v", "-A"],
            &["-N", "-v", "-u"],
            &["-N", "-v", "a"],
            &["-N", "x.log"],
            &["-N", "-f", "x.log"],
            &["-N", "-v", "b"],
            &["-N", "--chmod=+x", "-v", "top", "a"],
        ],
    );
    // An existing intent-to-add entry: skipped by -N, filled by add / -u.
    for args in [&["-N", "-v", "top"][..], &["-v", "top"], &["-v", "-u"]] {
        let mut fx = fixture().git(["add", "-N", "top"]);
        let mut argv = vec!["add"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(STATE).run();
    }
}

#[test]
fn chmod() {
    each(
        "",
        &[
            &["--chmod=+x", "-v", "a", "top"],
            &["--chmod=+x", ".gitignore"],
            &["--chmod=-x", "a"],
            &["--chmod=+x", "--chmod=-x", "a"],
            &["--no-chmod", "a"],
            &["--chmod=+x", "-n", "a", "top"],
            &["--chmod=+x", "-u"],
            &["--chmod=+x", "-A"],
            &["--chmod=+x", "."],
            &["--chmod=+x", "lnk"],
            &["--chmod=-x", "-n", "lnk", "a"],
            &["--chmod=+x"],
            &["--chmod=x", "a"],
            &["--chmod=x"],
            &["--chmod=+x", "nosuch"],
            &["--chmod=+x", "-f", "x.log"],
        ],
    );
    let mut fx = fixture().write_executable("a", "a3\n");
    fx.compare(["add", "--chmod=-x", "a"]).checks(STATE).run();
}

#[test]
fn ignored_and_missing() {
    each(
        "",
        &[
            &["-n", "build/out"],
            &["-n", "build"],
            &["-n", "build/nosuch"],
            &["-n", "build/*"],
            &["-n", "-v", "x.log", "build/out", "top"],
            &["-n", "x.lo?"],
            &["e/f"],
            &["-u", "top"],
            &["-u", "x.log", "nosuch"],
            &["--ignore-missing", "a"],
            &[
                "--ignore-missing",
                "-n",
                "a",
                "nosuch",
                "x.log",
                "build/q",
                "y.log",
            ],
            &["--ignore-missing", "-n", "-v", "y.log", "y.log"],
            &["--ignore-missing", "-n", "build/*"],
            &["--ignore-missing", "-n", "y.lo?"],
            &["--ignore-missing", "-n", "nosuch"],
            &["--ignore-missing", "-n"],
            &["--ignore-missing", "-n", "-f", "y.log"],
            &["--ignore-missing", "-n", "-N", "y.log", "top"],
        ],
    );
    each(
        "d",
        &[&["-n", "../x.log"], &["--ignore-missing", "-n", "../y.log"]],
    );
}

#[test]
fn pathspec_from_file() {
    let cases: &[(&[u8], &[&str])] = &[
        (b"top\nd/new\n", &["-v", "--pathspec-from-file=list"]),
        (
            b"top\0d/new\0",
            &["-v", "--pathspec-from-file=list", "--pathspec-file-nul"],
        ),
        (b"\"top\"\r\nd/new", &["-v", "--pathspec-from-file", "list"]),
        (b"\"top\n", &["--pathspec-from-file=list"]),
        (b"top\n", &["--pathspec-from-file=list", "a"]),
        (b"", &["--pathspec-from-file=list"]),
        (b"", &["-u", "--pathspec-from-file=list"]),
        (b"\n", &["--pathspec-from-file=list"]),
        (b"nosuch\n", &["--pathspec-from-file=list"]),
        (b"top\n", &["--pathspec-from-file=nosuch"]),
        (b"top\n", &["--pathspec-file-nul", "a"]),
    ];
    for (list, args) in cases {
        let mut fx = fixture().write("list", list);
        let mut argv = vec!["add"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(STATE).run();
    }
    let mut fx = fixture();
    fx.compare(["add", "-v", "--pathspec-from-file=-"])
        .stdin(&b"top\nd/new\n"[..])
        .checks(STATE)
        .run();
    let mut fx = fixture().write("d/list", "new\n");
    fx.compare(["add", "-v", "--pathspec-from-file=list"])
        .in_dir("d")
        .checks(STATE)
        .run();
}

#[test]
fn ignore_errors_option() {
    // Unreadable files cannot be copied into the fixture; t3700 covers
    // them. Here: the option and the configuration parse.
    each(
        "",
        &[
            &["--ignore-errors", "-v", "top"],
            &["--no-ignore-errors", "-v", "top"],
        ],
    );
    let mut fx = fixture().git(["config", "add.ignoreErrors", "true"]);
    fx.compare(["add", "-v", "top"]).checks(STATE).run();
}
