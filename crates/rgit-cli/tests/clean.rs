// SPDX-License-Identifier: MIT
//! Differential tests of `rgit clean` against the reference git. Every
//! case runs in a fresh copy of a temporary fixture (rgit-difftest), and the
//! resulting work tree is compared, so nothing outside the fixture is ever
//! touched.

use rgit_difftest::{Check, Fixture};

/// Tracked and untracked files and directories, ignored files and
/// directories, nested repositories (valid, empty, bare, broken), a
/// `.git` file, symbolic links and names that need quoting.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("src/p1.c", "1\n")
        .write("Makefile", "m\n")
        .write("t/k", "k\n")
        .write(".gitignore", "build\n*.o\nig/\nig2/\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "init"])
        .write("a.out", "\n")
        .write("src/p3.c", "\n")
        .write("docs/manual.txt", "\n")
        .write("docs/sub/x", "\n")
        .write("docs/y.o", "\n")
        .write("obj.o", "\n")
        .write("build/lib.so", "\n")
        .write("untr/deep/z", "\n")
        .write("allig/a.o", "\n")
        .write("allig/s/b.o", "\n")
        .write("mix2/a.o", "\n")
        .write("mix2/e/.keep.o", "\n")
        .write("m3/build/x", "\n")
        .write("m3/f", "\n")
        .write("t/x.o", "\n")
        .write("t/ig/q", "\n")
        .write("t/u/f", "\n")
        .write("ig/f", "\n")
        .write("sp ace", "\n")
        .write("q\"x", "\n")
        .write("u/a", "\n")
        .write("u/inner/f", "\n")
        .git(["init", "-q", "u/inner"])
        .git(["init", "-q", "nest"])
        .write("nest/f", "\n")
        .git(["-C", "nest", "add", "f"])
        .git(["-C", "nest", "commit", "-q", "-m", "n"])
        .write("almost/.git/HEAD", "garbage\n")
        .write("almost/.git/objects/.keep", "")
        .write("almost/.git/refs/.keep", "")
        .write("almost/f", "\n")
        .write("gl/.git", "gitdir: nowhere\n")
        .write("gl/x", "\n")
        .git(["init", "-q", "--bare", "bare/b.git"])
        .symlink("src", "lnk")
        .symlink("nowhere", "dangling")
}

/// Runs `clean <args>` in `dir` with both binaries; compares output, exit
/// code and the resulting work tree and index.
fn each(dir: &str, cases: &[&[&str]]) {
    for args in cases {
        let mut fx = fixture();
        let mut argv = vec!["clean"];
        argv.extend_from_slice(args);
        fx.compare(&argv)
            .in_dir(dir)
            .checks(
                Check::OUTPUT
                    .into_iter()
                    .chain([Check::Worktree, Check::Index]),
            )
            .run();
    }
}

#[test]
fn dry_run_and_force() {
    each(
        ".",
        &[
            &["-n"],
            &["-nd"],
            &["-nx"],
            &["-ndx"],
            &["-nX"],
            &["-ndX"],
            &["-ndff"],
            &["-ndq"],
            &["-f"],
            &["-fd"],
            &["-fdx"],
            &["-fdX"],
            &["-ffd"],
            &["-ffdx"],
            &["-fdq"],
        ],
    );
}

#[test]
fn pathspecs_and_excludes() {
    each(
        ".",
        &[
            &["-n", "docs"],
            &["-n", "docs/"],
            &["-n", "nest"],
            &["-nd", "u"],
            &["-fd", "u"],
            &["-n", "u/inner/f"],
            &["-nX", "build"],
            &["-n", "untr/deep"],
            &["-n", "*z"],
            &["-nd", "*z"],
            &["-nd", ":!u"],
            &["-nd", "-e", "docs"],
            &["-ndx", "-e", "src"],
            &["-ndX", "-e", "src"],
            &["-fd", "almost", "gl", "bare"],
            &["-n", "src/"],
            &["-nd", ".git"],
            &["-nd", "/nonexistent"],
        ],
    );
}

#[test]
fn from_subdirectory() {
    each(
        "src",
        &[
            &["-nd"],
            &["-nd", ".."],
            &["-fd", "../u"],
            &["-f"],
            &["-n", "../t"],
        ],
    );
}

#[test]
fn require_force_and_errors() {
    each(
        ".",
        &[
            &[],
            &["-d"],
            &["-x", "-X"],
            &["-n", "-x", "-X"],
            &["-f", "-x", "-X"],
            &["--bogus"],
            &["-h"],
        ],
    );
    for value in ["false", "true", "maybe"] {
        let mut fx = fixture();
        let key = format!("clean.requireForce={value}");
        fx.compare(["-c", key.as_str(), "clean"])
            .checks(Check::OUTPUT.into_iter().chain([Check::Worktree]))
            .run();
    }
}

#[test]
fn skip_worktree_gitignore() {
    let mut fx = fixture()
        .git(["update-index", "--skip-worktree", ".gitignore"])
        .remove(".gitignore");
    fx.compare(["clean", "-n"]).checks(Check::OUTPUT).run();
    fx.compare(["clean", "-nX"]).checks(Check::OUTPUT).run();
}

#[test]
fn interactive() {
    for (input, args) in [
        ("c\n", &["-i"][..]),
        ("cl\n", &["-id"]),
        ("quit\n", &["-id"]),
        ("", &["-id"]),
        ("h\n?\nx\n\n5\n", &["-id"]),
        ("f\n*.c src/\n\nc\n", &["-id"]),
        ("f\n* !*.out\n\nc\n", &["-id"]),
        ("f\n  bogus  zz\n*\n", &["-id"]),
        ("s\n1 3-4 7-\n\nc\n", &["-id"]),
        ("s\n?\n*\n-2\n\nc\n", &["-id"]),
        ("s\nsp q zz 99 2-1\n\nc\n", &["-id"]),
        ("s\n\n", &["-id"]),
        ("a\ny\nn\nyes\nY\n", &["-id"]),
        ("a\nn\nn\nn\nn\nn\nn\nn\nn\nn\nn\nn\nn\nn\nn\nn\n", &["-id"]),
        ("c\n", &["-idn"]),
    ] {
        let mut fx = fixture();
        let mut argv = vec!["clean"];
        argv.extend_from_slice(args);
        fx.compare(&argv)
            .stdin(input)
            .env("COLUMNS", "80")
            .checks(Check::OUTPUT.into_iter().chain([Check::Worktree]))
            .run();
    }
    let mut fx = fixture();
    fx.compare(["clean", "-id", ".."])
        .in_dir("src")
        .stdin("s\n../a ../d\n\nc\n")
        .env("COLUMNS", "60")
        .checks(Check::OUTPUT.into_iter().chain([Check::Worktree]))
        .run();
}
