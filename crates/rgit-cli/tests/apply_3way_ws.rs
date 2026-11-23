// SPDX-License-Identifier: MIT
//! `git apply --3way` and the whitespace handling of `git apply`
//! (`--whitespace`, `apply.whitespace`, `core.whitespace`,
//! `--ignore-space-change`) against the reference git.

/// Stat checks that survive the copy of the fixture (`--index` compares
/// stat data).
const STAT: [&str; 4] = [
    "-c",
    "core.checkStat=minimal",
    "-c",
    "core.trustCtime=false",
];

/// `args` after [`STAT`].
fn with_stat(args: &[&str]) -> Vec<String> {
    STAT.iter().chain(args).map(|s| s.to_string()).collect()
}

use rgit_difftest::{Check, Fixture};

/// Two branches that change the same files: `P.diff` is the change of
/// `side`, to be applied on `main`.
fn branches() -> Fixture {
    let base = "1\n2\n3\n4\n5\n6\n7\n";
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("one", base)
        .write("two", base)
        .write("three", base)
        .git(["add", "."])
        .git(["commit", "-q", "-m", "init"])
        .git(["branch", "side"])
        .tick()
        .write("one", "1\ntwo\n3\n4\n5\nsix\n7\n")
        .write("two", "1\ntwo\n3\n4\n5\n6\n7\n")
        .git(["commit", "-q", "-a", "-m", "main"])
        .git(["checkout", "-q", "side"])
        .tick()
        .write("one", "1\n2\n3\n4\nfive\n6\n7\n")
        .write("two", "1\n2\n3\n4\nfive\n6\n7\n")
        .write("three", "1\n2\nthree\n4\n5\n6\n7\n")
        .write("four", "new\n")
        .git(["add", "four"])
        .git(["commit", "-q", "-a", "-m", "side"])
        .git(["checkout", "-q", "main"]);
    let patch = fx.git_stdout(["diff", "main...side"]);
    fx.write("P.diff", patch)
}

#[test]
fn three_way_matches_git() {
    for args in [
        &["apply", "--3way", "P.diff"][..],
        &["apply", "--index", "--3way", "P.diff"],
        &["apply", "--cached", "--3way", "P.diff"],
        &["apply", "--3way", "--ours", "P.diff"],
        &["apply", "--3way", "--theirs", "P.diff"],
        &["apply", "--3way", "--union", "P.diff"],
        &[
            "-c",
            "merge.conflictStyle=diff3",
            "apply",
            "--3way",
            "P.diff",
        ],
        &["apply", "--check", "--3way", "P.diff"],
        &["apply", "--3way", "--reject", "P.diff"],
        &["apply", "--ours", "P.diff"],
    ] {
        branches()
            .compare(with_stat(args))
            .skip(Check::Objects)
            .run();
    }
}

/// The branches with `four` added in the index too, and `Q.diff`: the
/// patch with unknown preimage blobs.
fn add_add() -> Fixture {
    let fx = branches();
    let patch = fx.git_stdout(["diff", "main...side"]);
    let text = String::from_utf8(patch).unwrap();
    let broken: String = text
        .lines()
        .map(|l| {
            if l.starts_with("index ") && !l.starts_with("index 0000000") {
                "index 1234567..89abcde 100644".to_string()
            } else {
                l.to_string()
            }
        })
        .map(|l| l + "\n")
        .collect();
    fx.write("Q.diff", broken)
        .write("four", "other\n")
        .git(["add", "four"])
}

#[test]
fn three_way_add_add_and_missing_blobs() {
    for args in [
        &["apply", "--3way", "Q.diff"][..],
        &["apply", "--index", "--3way", "P.diff"],
    ] {
        add_add().compare(with_stat(args)).run();
    }
}

fn whitespace_base() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("f", "a\nb\nc\n")
        .write("h", "h\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
}

fn whitespace() -> Fixture {
    let lines: String = (1..=7).map(|i| format!("x{i} \n")).collect();
    let changed = whitespace_base()
        .write("f", "a \nb\n \tc\nd\t\n\n")
        .write("h", "h\n  \tx \n        y\n\tz\n")
        .write("g", lines)
        .git(["add", "-N", "g"]);
    let p = changed.git_stdout(["diff", "--", "f"]);
    let h = changed.git_stdout(["diff", "--", "h"]);
    let g = changed.git_stdout(["diff", "--", "g"]);
    whitespace_base().write("P", p).write("H", h).write("G", g)
}

#[test]
fn whitespace_actions_match_git() {
    let cases: &[&[&str]] = &[
        &["apply", "P"],
        &["apply", "--whitespace=warn", "P", "G"],
        &["apply", "--whitespace=nowarn", "P"],
        &["apply", "--whitespace=error", "P", "G"],
        &["apply", "--whitespace=error-all", "P", "G"],
        &["apply", "--whitespace=fix", "P", "G"],
        &["apply", "--check", "P"],
        &["-c", "apply.whitespace=error", "apply", "G"],
        &["-c", "apply.whitespace=bogus", "apply", "G"],
        &[
            "-c",
            "core.whitespace=indent-with-non-tab",
            "apply",
            "--whitespace=fix",
            "H",
        ],
        &[
            "-c",
            "core.whitespace=tab-in-indent,tabwidth=4",
            "apply",
            "--whitespace=fix",
            "H",
        ],
    ];
    for args in cases {
        whitespace().compare(with_stat(args)).run();
    }
}

#[test]
fn ignore_space_change_matches_git() {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("f", "a\n  b\nc\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .write("f", "a\n b\nc\nd\n");
    let w = fx.git_stdout(["diff", "--", "f"]);
    let mut fx = fx.write("W", w).write("f", "a\n\tb\nc\n");
    fx.compare(with_stat(&["apply", "W"])).run();
    fx.compare(with_stat(&["apply", "--ignore-space-change", "W"]))
        .run();
    fx.compare(with_stat(&[
        "-c",
        "apply.ignoreWhitespace=change",
        "apply",
        "W",
    ]))
    .run();
    fx.compare(with_stat(&[
        "-c",
        "apply.ignoreWhitespace=bogus",
        "apply",
        "W",
    ]))
    .run();
}

/// A submodule at two commits and a file without a final line feed;
/// `mod.patch` moves the submodule, `eof.patch` changes the last line.
fn gitlinks() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .git(["init", "-q", "sub"])
        .write("sub/a", "a\n")
        .git(["-C", "sub", "add", "a"])
        .git(["-C", "sub", "commit", "-q", "-m", "s1"])
        .tick()
        .write("sub/a", "b\n")
        .git(["-C", "sub", "commit", "-q", "-a", "-m", "s2"]);
    let s1 = String::from_utf8(fx.git_stdout(["-C", "sub", "rev-parse", "HEAD~1"])).unwrap();
    let s2 = String::from_utf8(fx.git_stdout(["-C", "sub", "rev-parse", "HEAD"])).unwrap();
    let add = format!("160000,{},sub", s1.trim());
    let fx = fx
        .git(["update-index", "--add", "--cacheinfo", &add])
        .write("n", "a\nb\nc")
        .git(["add", "n"])
        .git(["commit", "-q", "-m", "one"]);
    let moved = format!("160000,{},sub", s2.trim());
    let fx = fx.git(["update-index", "--cacheinfo", &moved]);
    let m = fx.git_stdout(["diff", "--cached"]);
    let fx = fx.git(["reset", "-q"]).write("n", "a\nb\nd");
    let e = fx.git_stdout(["diff", "--", "n"]);
    let without_marker: Vec<u8> = String::from_utf8(e.clone())
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('\\'))
        .flat_map(|l| format!("{l}\n").into_bytes())
        .collect();
    fx.git(["checkout", "-q", "n"])
        .write("mod.patch", m)
        .write("eof.patch", e)
        .write("e2.patch", without_marker)
}

#[test]
fn gitlinks_and_inaccurate_eof() {
    for args in [
        &["apply", "mod.patch"][..],
        &["apply", "--index", "mod.patch"],
        &["apply", "--cached", "mod.patch"],
        &["apply", "--check", "mod.patch"],
        &["apply", "-R", "--cached", "mod.patch"],
        &["apply", "eof.patch"],
        &["apply", "--inaccurate-eof", "e2.patch"],
        &["apply", "e2.patch"],
    ] {
        gitlinks().compare(with_stat(args)).run();
    }
}

/// A committed `f` and `e`, `P` (a change of `f`, with whitespace errors
/// in context and added lines) and `E` (a line added after a blank line
/// at the end of `e`).
fn ws_fix_patches() -> Fixture {
    let base = || {
        Fixture::new()
            .rgit(env!("CARGO_BIN_EXE_rgit"))
            .git(["init", "-q"])
            .write("f", "a \nb\nc \nd\n")
            .write("e", "a\n\n")
            .git(["add", "."])
            .git(["commit", "-q", "-m", "one"])
    };
    let changed = base().write("f", "a \nB \nc \nd\n").write("e", "a\n\nb\n");
    let p = changed.git_stdout(["diff", "--", "f"]);
    let e = changed.git_stdout(["diff", "--", "e"]);
    base().write("P", p).write("E", e)
}

/// `--whitespace=fix` matching: a preimage that differs from the file only
/// in whitespace the fix changes, blank context lines missing at the end
/// of the file, and context lines with whitespace errors.
#[test]
fn whitespace_fix_matching_matches_git() {
    let cases: &[(&str, &[&str])] = &[
        ("a \nb\nc \nd\n", &["apply", "--whitespace=fix", "P"]),
        ("a\nb\nc\nd\n", &["apply", "--whitespace=fix", "P"]),
        ("a\nb\nc \nd\n", &["apply", "--whitespace=fix", "P"]),
        (
            "a\nb\nc\nd\n",
            &["apply", "--check", "--whitespace=fix", "P"],
        ),
        ("a\nb\nc\nd\n", &["apply", "P"]),
        ("a\nB\nc\nd\n", &["apply", "-R", "--whitespace=fix", "P"]),
        ("x \nb \nc\nd\n", &["apply", "-C1", "--whitespace=fix", "P"]),
    ];
    for (content, args) in cases {
        ws_fix_patches()
            .write("f", content)
            .compare(with_stat(args))
            .run();
    }
    for (content, args) in [
        ("a\n", &["apply", "E"][..]),
        ("a\n", &["apply", "--whitespace=fix", "E"]),
        (
            "a\n",
            &["apply", "--ignore-space-change", "--whitespace=fix", "E"],
        ),
        // Blank lines beyond the end need a non-blank line in the file.
        ("\n", &["apply", "--whitespace=fix", "E"]),
    ] {
        ws_fix_patches()
            .write("e", content)
            .compare(with_stat(args))
            .run();
    }
}

/// The `incomplete-line` rule: an added line without a final line feed.
#[test]
fn incomplete_line_matches_git() {
    let base = || {
        Fixture::new()
            .rgit(env!("CARGO_BIN_EXE_rgit"))
            .git(["init", "-q"])
            .write("f", "1\n2\n3\n")
            .git(["add", "."])
            .git(["commit", "-q", "-m", "one"])
            .git(["config", "core.whitespace", "incomplete-line"])
    };
    let p = base().write("f", "1\n2\n4").git_stdout(["diff", "--", "f"]);
    for (content, args) in [
        ("1\n2\n3\n", &["apply", "--whitespace=error", "P"][..]),
        (
            "1\n2\n3\n",
            &["apply", "--whitespace=error", "--check", "P"],
        ),
        ("1\n2\n3\n", &["apply", "--whitespace=fix", "P"]),
        ("1\n2\n4", &["apply", "-R", "--whitespace=error", "P"]),
        ("1\n2\n4", &["apply", "-R", "--whitespace=fix", "P"]),
    ] {
        base()
            .write("P", &p)
            .write("f", content)
            .compare(with_stat(args))
            .run();
    }
}
