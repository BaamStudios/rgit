// SPDX-License-Identifier: MIT
//! Differential tests of `check-ignore` against the reference git, on a
//! layout like the one t0008-ignores.sh builds (nested `.gitignore` files,
//! `info/exclude`, `core.excludesFile`, tracked files, a symbolic link and a
//! submodule).

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a/b/ignored-dir/.gitignore", "seven\n")
        .write("a/submodule/a", "a\n")
        .write("b/.keep", "")
        .git(["-C", "a/submodule", "init", "-q"])
        .git(["-C", "a/submodule", "add", "a"])
        .git(["-C", "a/submodule", "commit", "-q", "-m", "sub"])
        .git(["add", "a/submodule"])
        .write(".gitignore", "one\nignored-*\ntop-level-dir/\n")
        .write("not-ignored", "")
        .write("ignored-and-untracked", "")
        .write("ignored-but-in-index", "")
        .write("a/not-ignored", "")
        .write("a/ignored-and-untracked", "")
        .write("a/ignored-but-in-index", "")
        .git([
            "add",
            "-f",
            "ignored-but-in-index",
            "a/ignored-but-in-index",
        ])
        .write("a/.gitignore", "two*\n*three\n")
        .write(
            "a/b/.gitignore",
            "four\nfive\n# a comment counts for line numbers\nsix\nignored-dir/\n\
             # and so does a blank line:\n\n!on*\n!two\n",
        )
        .write("global-excludes", "globalone\n!globaltwo\nglobalthree\n")
        .write(".git/info/exclude", "per-repo\n");
    #[cfg(unix)]
    let fx = fx.symlink("b", "a/symlink");
    fx
}

fn same(fx: &mut Fixture, dir: &str, args: &[&str]) {
    let mut full = vec!["check-ignore"];
    full.extend_from_slice(args);
    fx.compare(&full).in_dir(dir).checks(Check::OUTPUT).run();
}

fn same_stdin(fx: &mut Fixture, dir: &str, args: &[&str], input: &[u8]) {
    let mut full = vec!["check-ignore"];
    full.extend_from_slice(args);
    fx.compare(&full)
        .in_dir(dir)
        .stdin(input.to_vec())
        .checks(Check::OUTPUT)
        .run();
}

const MODES: [&[&str]; 8] = [
    &[],
    &["-q"],
    &["-v"],
    &["-v", "-n"],
    &["--no-index"],
    &["--no-index", "-q"],
    &["--no-index", "-v"],
    &["--no-index", "-v", "-n"],
];

const PATHS: &[&str] = &[
    ".",
    "non-existent",
    "one",
    "not-ignored",
    "ignored-but-in-index",
    "ignored-and-untracked",
    "a/non-existent",
    "a/one",
    "a/not-ignored",
    "a/ignored-but-in-index",
    "a/ignored-and-untracked",
    "a/3-three",
    "a/b/one",
    "a/b/ignored-dir",
    "a/b/ignored-dir/seven",
    "top-level-dir",
    "top-level-dir/",
    "a/submodule",
];

#[test]
fn single_paths_in_all_modes() {
    let mut fx = fixture();
    for mode in MODES {
        for p in PATHS {
            let mut args = mode.to_vec();
            args.push(p);
            same(&mut fx, ".", &args);
        }
    }
}

#[test]
fn several_paths_and_subdirectories() {
    let mut fx = fixture();
    for mode in MODES.iter().filter(|m| !m.contains(&"-q")) {
        let with = |paths: &[&'static str]| {
            let mut v = mode.to_vec();
            v.extend_from_slice(paths);
            v
        };
        same(
            &mut fx,
            ".",
            &with(&[
                "non-existent",
                "one",
                "not-ignored",
                "ignored-but-in-index",
                "ignored-and-untracked",
                "a/3-three",
                "a/three-not-this-one",
            ]),
        );
        same(
            &mut fx,
            "a",
            &with(&["3-three", "three-not-this-one", "../one", ".."]),
        );
        same(
            &mut fx,
            "a/b/ignored-dir",
            &with(&["foo", "twoooo", "../one", "seven", "../../one"]),
        );
        same(
            &mut fx,
            ".",
            &with(&[
                "globalone",
                "per-repo",
                "a/globalthree",
                "a/per-repo",
                "a/globaltwo",
            ]),
        );
    }
}

#[test]
fn usage_errors() {
    let mut fx = fixture();
    for args in [
        &[][..],
        &["--"],
        &[""],
        &["--stdin", "foo"],
        &["--stdin", "-z", "foo"],
        &["-z"],
        &["-z", "foo"],
        &["-q", "one", "two"],
        &["--quiet", "-v", "foo"],
        &["-n", "foo"],
        &["--bogus"],
        &["-h"],
        &[":(icase)one"],
        &[":!one"],
        &["/outside"],
        &["a/submodule/one"],
    ] {
        same(&mut fx, ".", args);
        same(&mut fx, "a", args);
    }
    same(&mut fx, ".git", &["foo"]);
    same(&mut fx, ".git", &[]);
}

#[cfg(unix)]
#[test]
fn symbolic_links() {
    let mut fx = fixture();
    same(&mut fx, ".", &["-v", "-n", "a/symlink"]);
    same(&mut fx, ".", &["a/symlink/foo"]);
    same(&mut fx, ".", &["--no-index", "a/symlink/foo"]);
    same(&mut fx, "a", &["symlink/foo"]);
    let mut fx = fx
        .write("ignore", "*\n")
        .remove(".gitignore")
        .remove(".git/info/exclude")
        .symlink("ignore", ".gitignore")
        .symlink("../ignore", "subdir/.gitignore");
    same(&mut fx, ".", &["subdir/file"]);
    let mut fx = fx.remove(".gitignore").remove("subdir/.gitignore");
    let abs = fx.work_dir().join("ignore");
    let cfg = format!("core.excludesFile={}", abs.display());
    fx.compare(["-c", &cfg, "check-ignore", "-v", "file"])
        .checks(Check::OUTPUT)
        .run();
    let mut fx = fx.symlink("../../ignore", ".git/info/exclude");
    same(&mut fx, ".", &["-v", "file"]);
}

const STDIN: &str = "one\nnot-ignored\na/one\na/not-ignored\na/b/on\na/b/one\na/b/one one\n\
\"a/b/one two\"\n\"a/b/one\\\"three\"\na/b/not-ignored\na/b/two\na/b/twooo\nglobaltwo\n\
a/globaltwo\na/b/globaltwo\nb/globaltwo\n";

#[test]
fn stdin_and_global_excludes() {
    let mut fx = fixture().git(["config", "core.excludesfile", "global-excludes"]);
    let nul: Vec<u8> = STDIN
        .lines()
        .flat_map(|l| {
            let l = l.trim_matches('"').replacen('\\', "", 1);
            let mut v = l.into_bytes();
            v.push(0);
            v
        })
        .collect();
    for mode in [&[][..], &["-q"], &["-v"], &["-v", "-n"]] {
        for dir in [".", "a"] {
            let mut args = vec!["--stdin"];
            args.extend_from_slice(mode);
            same_stdin(&mut fx, dir, &args, STDIN.as_bytes());
            args.push("-z");
            same_stdin(&mut fx, dir, &args, &nul);
        }
        let mut args = mode.to_vec();
        args.extend_from_slice(&["globalone", "per-repo", "globalthree", "not-ignored"]);
        if !mode.contains(&"-q") {
            same(&mut fx, ".", &args);
        }
    }
    same_stdin(&mut fx, ".", &["--stdin"], b"");
    same_stdin(
        &mut fx,
        ".",
        &["--stdin", "-v", "-n"],
        b"one\n\"bad\\x\"\ntwo\n",
    );
    same_stdin(&mut fx, ".", &["--stdin", "-v", "-n"], b"one\n\ntwo\n");
    same_stdin(&mut fx, ".", &["--stdin", "-v", "-n"], b"one\r\ntwo");
}

#[test]
fn excludes_file_default_and_tilde() {
    let mut fx = fixture()
        .write("xdg/git/ignore", "xdgpat\n")
        .write("home-ignore", "homepat\n");
    let xdg = fx.work_dir().join("xdg");
    fx.compare(["check-ignore", "-v", "xdgpat", "homepat"])
        .env("XDG_CONFIG_HOME", &xdg)
        .checks(Check::OUTPUT)
        .run();
    if fx.is_skipped() {
        return;
    }
    let home = fx.home_dir().join(".config/git");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("ignore"), "homepat\n").unwrap();
    same(&mut fx, ".", &["-v", "xdgpat", "homepat"]);
    fx.compare([
        "-c",
        "core.excludesFile=~/.config/git/ignore",
        "check-ignore",
        "-v",
        "homepat",
    ])
    .checks(Check::OUTPUT)
    .run();
    let mut fx = fx.git(["config", "core.excludesFile", "global-excludes"]);
    same(&mut fx, "a", &["-v", "-n", "globalone", "../globalone"]);
}

#[test]
fn pattern_syntax() {
    let ignore = b"\xef\xbb\xbfbom\r\ncrlf\r\n\\#hash\n#c\n\\!bang\nsp  \nesc\\ \\ \n!\n/\n/x/\n\
mid/dle\n**/deep\n\\\n  lead\ntab\t\nbs\\\\\nx[ab\nback\\\n";
    let mut fx = fixture().write(".gitignore", ignore).write("q/.keep", "");
    same(
        &mut fx,
        ".",
        &[
            "-v",
            "-n",
            "bom",
            "crlf",
            "#hash",
            "#c",
            "!bang",
            "sp",
            "esc  ",
            "x/",
            "x/y",
            "q/mid/dle",
            "mid/dle",
            "q/deep",
            "q/  lead",
            "  lead",
            "tab",
            "tab\t",
            "bs\\",
            "x[ab",
            "back",
            "back\\",
        ],
    );
    same(
        &mut fx,
        "q",
        &["-v", "-n", "bom", "../crlf", "deep", ":/crlf", ":(top)crlf"],
    );

    let mut fx = fx
        .write(".gitignore", "!a\nb\nc/\n!d/\ne/\n!e/\n")
        .write("e/.keep", "");
    same(
        &mut fx,
        ".",
        &[
            "-v", "-n", "a/x", "a", "b/", "b/x", "c", "c/", "c/x", "d/x", "e", "e/x", "x/e/y",
        ],
    );
    fx.compare([
        "-c",
        "core.ignorecase=true",
        "check-ignore",
        "-v",
        "-n",
        "B",
        "C/",
        "E/x",
    ])
    .checks(Check::OUTPUT)
    .run();

    let mut fx = fx
        .write(".gitignore", "data/**\n!data/**/\n!data/**/*.txt\n")
        .write("data/data1/x", "");
    same(
        &mut fx,
        ".",
        &[
            "-v",
            "-n",
            "file",
            "data/file",
            "data/data1/file1",
            "data/data1/file1.txt",
            "data/data2/file2.txt",
        ],
    );
    let mut fx = fx.write(".gitignore", "foo**/bar\n");
    same(&mut fx, ".", &["foobar", "foo/bar"]);
    let mut fx = fx
        .write("a/.gitignore", "/git/\n")
        .write("a/git/foo", "")
        .write("a/git-foo/bar", "");
    same(
        &mut fx,
        ".",
        &["a/git", "a/git/foo", "a/git-foo", "a/git-foo/bar"],
    );
    let mut fx = fx.write("a/.gitignore", "git/\n");
    same(
        &mut fx,
        ".",
        &["a/git", "a/git/foo", "a/git-foo", "a/git-foo/bar"],
    );
}

#[test]
fn quoting() {
    let mut fx = fixture().write(".gitignore", "*\n");
    let names = ["x\u{e9}", "a\x7fb", "a\tb", "a\"b", "a\\b"];
    let mut args = vec!["-v", "-n"];
    args.extend_from_slice(&names);
    same(&mut fx, ".", &args);
    let mut full = vec!["-c", "core.quotepath=false", "check-ignore"];
    full.extend_from_slice(&args);
    fx.compare(&full).checks(Check::OUTPUT).run();
    let mut fx = fx.write("d\u{e9}/.gitignore", "zz\n");
    same(&mut fx, ".", &["-v", "d\u{e9}/zz"]);
}
