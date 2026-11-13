// SPDX-License-Identifier: MIT
//! Differential tests of `rgit diff`, `diff-files`, `diff-index` and
//! `diff-tree` (pathspecs, stat formats) against the reference git.

use rgit_difftest::{Check, Fixture};

/// Run every command line and report all mismatches at once.
fn compare_all(fx: &mut Fixture, cmds: &[Vec<&str>]) {
    compare_all_in(fx, cmds, None);
}

fn compare_all_in(fx: &mut Fixture, cmds: &[Vec<&str>], dir: Option<&str>) {
    let mut failures = Vec::new();
    for argv in cmds {
        // Rename detection is not implemented: compare with `--no-renames`
        // (it also makes git hash added files when something was deleted).
        let mut argv = argv.clone();
        if let Some(i) = argv.iter().position(|a| *a == "diff") {
            if !argv.contains(&"--no-index") {
                argv.insert(i + 1, "--no-renames");
            }
        }
        let mut c = fx.compare(&argv).checks(Check::OUTPUT);
        if let Some(d) = dir {
            c = c.in_dir(d);
        }
        if let Err(m) = c.try_run() {
            failures.push(format!("{argv:?}\n{}", m.report()));
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn with_formats<'a>(base: &[&'a str], tail: &[&'a str]) -> Vec<Vec<&'a str>> {
    let formats: &[&[&str]] = &[
        &[],
        &["--raw"],
        &["-p"],
        &["--stat"],
        &["--numstat"],
        &["--shortstat"],
        &["--summary"],
        &["--name-only"],
        &["--name-status"],
        &["-z", "--name-status"],
        &["-z", "--raw"],
        &["-z", "--numstat"],
        &["--raw", "-p"],
        &["--stat", "-p"],
        &["--stat", "--raw", "--summary"],
        &["--numstat", "--shortstat"],
        &["--abbrev"],
        &["--abbrev=10", "--raw", "-p"],
        &["--full-index", "-p"],
        &["-R", "--raw", "-p"],
        &["-U1"],
        &["-U0"],
        &["--unified=5"],
        &["--no-prefix"],
        &["--src-prefix=X/", "--dst-prefix=Y/"],
        &["--exit-code"],
        &["--quiet"],
        &["-s"],
        &["-s", "--exit-code"],
        &["-p", "-s"],
        &["-s", "-p"],
        &["--name-only", "--name-status"],
        &["-s", "--name-only"],
    ];
    formats
        .iter()
        .map(|f| {
            let mut v = base.to_vec();
            v.extend_from_slice(f);
            v.extend_from_slice(tail);
            v
        })
        .collect()
}

/// A commit, then staged and unstaged changes of every kind.
fn work_fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a/b/f", "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n")
        .write("a/g", "y\n")
        .write("a.c", "c\n")
        .write("c/\u{e4}\tq", "z\n")
        .write("gone", "bye\n")
        .write("staged-gone", "bye\n")
        .write("mode", "#!/bin/sh\n")
        .write("tl", "target\n")
        .write("bin", "a\0b")
        .write("touched", "same\n")
        .write("space name", "s\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "first"])
        .git(["tag", "v1"])
        .tick()
        .write("a/b/f", "one\n2\nthree\nfour\nfive\nsix\n7\neight\nnine\n")
        .write("a/new file", "n\n")
        .git(["rm", "-q", "staged-gone"])
        .git(["add", "a"])
        .write(
            "a/b/f",
            "one\n2\nthree\nfour\nfive\nsix\n7\neight\nnine\nten\n",
        )
        .write("c/\u{e4}\tq", "zz\n")
        .write_executable("mode", "#!/bin/sh\n")
        .write("bin", "a\0c")
        .remove("gone")
        .remove("tl")
        .symlink("a/g", "tl")
        .write("space name", "s2\n")
        .write("untracked", "u\n")
        .write("ita", "intent\n")
        .git(["add", "-N", "ita"])
}

#[test]
fn diff_files_matches_git() {
    let mut fx = work_fixture();
    let mut cmds = with_formats(&["diff-files"], &[]);
    cmds.extend(with_formats(&["diff-files"], &["--", "a"]));
    cmds.push(vec!["diff-files", "-q"]);
    cmds.push(vec!["diff-files", "-p", "a/b", "mode"]);
    cmds.push(vec!["diff-files", "--bogus"]);
    compare_all(&mut fx, &cmds);
}

#[test]
fn diff_index_matches_git() {
    let mut fx = work_fixture();
    let mut cmds = with_formats(&["diff-index"], &["HEAD"]);
    cmds.extend(with_formats(&["diff-index", "--cached"], &["HEAD"]));
    cmds.extend(with_formats(
        &["diff-index", "--cached"],
        &["v1", "--", "a", "*.c"],
    ));
    cmds.push(vec!["diff-index", "-p", "HEAD", "a/b/f"]);
    cmds.push(vec!["diff-index"]);
    cmds.push(vec!["diff-index", "nope"]);
    compare_all(&mut fx, &cmds);
}

#[test]
fn diff_porcelain_matches_git() {
    let mut fx = work_fixture();
    let mut cmds = with_formats(&["diff"], &[]);
    cmds.extend(with_formats(&["diff", "--cached"], &[]));
    cmds.extend(with_formats(&["diff"], &["HEAD"]));
    cmds.extend(with_formats(&["diff", "--staged"], &["v1"]));
    cmds.extend(with_formats(&["diff"], &["v1", "HEAD"]));
    for extra in [
        vec!["diff", "--", "a"],
        vec!["diff", "a"],
        vec!["diff", "HEAD", "a", "mode"],
        vec!["diff", "HEAD", "--", "nonexistent"],
        vec!["diff", "nonexistent"],
        vec!["diff", "HEAD", "nonexistent"],
        vec!["diff", "mode", "HEAD"],
        vec!["diff", "v1..HEAD"],
        vec!["diff", "v1...HEAD", "--stat"],
        vec!["diff", "..v1"],
        vec!["diff", "--bogus"],
        vec!["diff", "-Ux"],
        vec!["diff", "--stat=x"],
        vec!["diff", "--stat=40"],
        vec!["diff", "--stat=60,10"],
        vec!["diff", "--stat-count=2"],
        vec!["diff", "--stat-graph-width=3"],
        vec!["-c", "core.quotepath=false", "diff", "--stat"],
        vec!["-c", "diff.mnemonicPrefix=true", "diff"],
        vec!["-c", "diff.mnemonicPrefix=true", "diff", "--cached"],
        vec!["-c", "diff.mnemonicPrefix=true", "diff", "HEAD"],
        vec!["-c", "diff.mnemonicPrefix=true", "diff", "-R"],
        vec!["-c", "diff.mnemonicPrefix=true", "diff", "v1", "HEAD"],
        vec!["-c", "diff.noprefix=true", "diff"],
        vec!["-c", "diff.context=1", "diff"],
    ] {
        cmds.push(extra);
    }
    compare_all(&mut fx, &cmds);
    // From a subdirectory: pathspecs are relative, output paths are not.
    compare_all_in(
        &mut fx,
        &[
            vec!["diff", "--stat"],
            vec!["diff", "."],
            vec!["diff", "--cached", "--name-only", "."],
            vec!["diff-files", "--name-only", "b"],
            vec!["diff-index", "--name-only", "HEAD", "b"],
            vec!["diff-tree", "-r", "--name-only", "v1", "HEAD", "b"],
        ],
        Some("a"),
    );
}

#[test]
fn diff_unmerged_matches_git() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("c", "base\n")
        .write("e", "base\n")
        .write("dd", "base\n")
        .write("keep", "k\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "base"])
        .git(["checkout", "-q", "-b", "side"])
        .write("c", "side\n")
        .write("e", "side\n")
        .write("dd", "side\n")
        .git(["commit", "-q", "-a", "-m", "side"])
        .git(["checkout", "-q", "main"])
        .write("c", "main\n")
        .write_executable("e", "main\n")
        .git(["add", "-A"])
        .git(["rm", "-q", "dd"])
        .git(["commit", "-q", "-m", "main"]);
    // The merge stops with conflicts (exit code 1).
    let _ = fx.git_output(["merge", "-q", "side"]);
    let mut cmds = Vec::new();
    for f in [
        &[][..],
        &["--name-status"],
        &["--stat"],
        &["--numstat"],
        &["--raw"],
    ] {
        for base in [
            &["diff-files"][..],
            &["diff-index", "HEAD"],
            &["diff-index", "--cached", "HEAD"],
            &["diff"],
            &["diff", "--cached"],
            &["diff", "HEAD"],
        ] {
            let mut v = base.to_vec();
            v.extend_from_slice(f);
            // `git diff` shows a combined diff here, which is not implemented.
            if v != ["diff"] {
                cmds.push(v);
            }
        }
    }
    cmds.push(vec!["diff-index", "--cached", "-p", "--stat", "HEAD"]);
    cmds.push(vec!["diff-index", "-p", "HEAD"]);
    cmds.push(vec!["diff", "HEAD"]);
    cmds.push(vec!["diff", "--cached"]);
    compare_all(&mut fx, &cmds);
}

#[test]
fn diff_stat_layout_matches_git() {
    fn lines(n: usize, tag: &str) -> String {
        (0..n).map(|i| format!("{tag}{i}\n")).collect()
    }
    let long = "very/long/directory/name/that/goes/on/and/on/forever/and/ever/file.txt";
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("short", lines(200, "a"))
        .write("mid_name_file.txt", lines(5, "b"))
        .write("d", lines(3, "c"))
        .write("big", lines(1000, "z"))
        .write(long, lines(4, "k"))
        .write("binary", "x\0y")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .write("short", format!("{}{}", lines(150, "a"), lines(120, "q")))
        .write("mid_name_file.txt", lines(7, "b"))
        .write("d", format!("{}x\n", lines(3, "c")))
        .write("big", lines(999, "y"))
        .write(long, lines(2, "k"))
        .write("binary", "x\0yzzz");
    let mut cmds = Vec::new();
    for s in [
        "--stat",
        "--stat=60",
        "--stat=100,20",
        "--stat=200",
        "--stat=50,30,2",
        "--stat-graph-width=10",
        "--stat-name-width=8",
        "--numstat",
        "--shortstat",
    ] {
        cmds.push(vec!["diff", s]);
        cmds.push(vec!["diff", s, "--", "short", "d", "mid_name_file.txt"]);
        cmds.push(vec!["diff", s, "--", "big", "binary"]);
    }
    compare_all(&mut fx, &cmds);
}

#[test]
fn diff_tree_pathspecs_match_git() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a/b/f", "1\n")
        .write("a/g", "1\n")
        .write("a/x.c", "1\n")
        .write("top.c", "1\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .git(["tag", "v1"])
        .tick()
        .write("a/b/f", "2\n")
        .write("a/x.c", "2\n")
        .write("top.c", "2\n")
        .git(["commit", "-q", "-a", "-m", "two"]);
    let mut cmds = Vec::new();
    for spec in [
        "a/b/f",
        "a/b",
        "a/b/",
        "*.c",
        "a/*",
        "a",
        "nothing",
        ":(exclude)a",
    ] {
        for opts in [&[][..], &["-r"], &["-t"], &["-p"], &["--stat"]] {
            let mut v = vec!["diff-tree"];
            v.extend_from_slice(opts);
            v.extend_from_slice(&["v1", "HEAD", "--", spec]);
            cmds.push(v);
        }
    }
    compare_all(&mut fx, &cmds);
}

#[test]
fn diff_no_index_matches_git() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .write("p", "a\nb\n")
        .write("q", "a\nc\n")
        .write("d1/x", "x\n")
        .write("d1/only1", "1\n")
        .write("d2/x", "y\n")
        .write("d2/z", "z\n");
    compare_all(
        &mut fx,
        &[
            vec!["diff", "--no-index", "p", "q"],
            vec!["diff", "--no-index", "p", "p"],
            vec!["diff", "--no-index", "--stat", "p", "q"],
            vec!["diff", "--no-index", "--numstat", "p", "q"],
            vec!["diff", "--no-index", "d1", "d2"],
            vec!["diff", "--no-index", "--stat", "d1", "d2"],
            vec!["diff", "--no-index", "--raw", "d1", "d2"],
            vec!["diff", "--no-index", "--name-status", "d1", "d2"],
            vec!["diff", "--no-index", "p", "nope"],
            vec!["diff", "p", "q"],
            vec!["diff", "--no-index", "--quiet", "p", "q"],
        ],
    );
}
