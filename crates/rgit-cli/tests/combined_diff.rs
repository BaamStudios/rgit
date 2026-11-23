// SPDX-License-Identifier: MIT
//! Differential tests of combined diffs (#148): `show`/`log` with `-c` and
//! `--cc` (patch and raw), and `diff-tree` for one commit (`-m`, `-c`,
//! `--cc`, `--root`, `--no-commit-id`), against the reference git.

use rgit_difftest::Fixture;

/// `main` and `side` change `f` in different places and `g` on both
/// sides; the merge resolves `g` anew, adds a line to `f`, takes `k` from
/// `side` and adds `new`.
fn fixture() -> Fixture {
    let mut f = String::new();
    for i in 1..=30 {
        f.push_str(&format!("line {i}\n"));
    }
    let side_f = f.replace("line 2\n", "side 2\n");
    let main_f = f.replace("line 25\n", "main 25\n");
    let merged_f = f
        .replace("line 2\n", "side 2\n")
        .replace("line 25\n", "main 25\n")
        .replace("line 14\n", "merge 14\n")
        + "tail\n";
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("f", &f)
        .write("g", "g\n")
        .write("k", "k\n")
        .write("del", "gone\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "base"])
        .git(["checkout", "-q", "-b", "side"])
        .tick()
        .write("f", &side_f)
        .write("g", "gs\n")
        .write("k", "ks\n")
        .git(["commit", "-q", "-a", "-m", "side"])
        .git(["checkout", "-q", "main"])
        .tick()
        .write("f", &main_f)
        .write("g", "gm\n")
        .git(["commit", "-q", "-a", "-m", "main"])
        .tick()
        .git(["merge", "-q", "-s", "ours", "--no-commit", "side"])
        .write("f", &merged_f)
        .write("g", "gx\n")
        .write("k", "ks\n")
        .write("new", "new\n")
        .git(["rm", "-q", "del"])
        .git(["add", "."])
        .git(["commit", "-q", "-m", "merge"])
}

fn cases(cases: &[&[&str]]) {
    let mut failed = Vec::new();
    for args in cases {
        let mut fx = fixture();
        if fx.is_skipped() {
            return;
        }
        if let Err(m) = fx.compare(*args).output_only().try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

#[test]
fn show_and_log() {
    cases(&[
        &["show", "HEAD"],
        &["show", "-c", "HEAD"],
        &["show", "--cc", "HEAD"],
        &["show", "-c", "--raw", "HEAD"],
        &["show", "--cc", "--stat", "HEAD"],
        &["show", "--format=%s", "HEAD", "--", "f"],
        &["log", "--cc", "-1"],
        &["log", "-c", "--raw", "--format=%h", "-2"],
        &["log", "--diff-merges=combined", "-p", "-1"],
        &["log", "--diff-merges=dense-combined", "-p", "-1"],
    ]);
}

#[test]
fn diff_tree_one_commit() {
    cases(&[
        &["diff-tree", "HEAD"],
        &["diff-tree", "-m", "HEAD"],
        &["diff-tree", "-c", "HEAD"],
        &["diff-tree", "-c", "-p", "HEAD"],
        &["diff-tree", "--cc", "HEAD"],
        &["diff-tree", "--cc", "--no-commit-id", "HEAD"],
        &["diff-tree", "--cc", "--stat", "HEAD"],
        &["diff-tree", "HEAD^1"],
        &["diff-tree", "-p", "HEAD^1"],
        &["diff-tree", "-r", "--name-only", "HEAD^1"],
        &["diff-tree", "-s", "HEAD^1"],
        &["diff-tree", "HEAD~2"],
        &["diff-tree", "--root", "HEAD~2"],
        &["diff-tree", "HEAD^1", "f"],
        &["diff-tree", "-c", "HEAD", "--", "g"],
    ]);
}

#[test]
fn separate_diffs_per_parent() {
    cases(&[
        &["log", "-m", "-p", "-1"],
        &["log", "-m", "--stat", "--oneline", "-3"],
        &["log", "-m", "--raw", "--format=%h", "-2"],
        &["log", "-m", "-p", "-1", "--format=format:%h"],
        &["log", "-m", "-p", "-1", "--parents", "--decorate"],
        &["log", "-m", "--graph", "-p", "--all"],
        &["log", "-m", "--graph", "--oneline", "--stat", "--all"],
        &["log", "-m", "-p", "--first-parent", "-2"],
        &["log", "-m", "--format=%s"],
        &["log", "-m", "-p", "-1", "--color=always", "--decorate"],
        &["show", "-m", "HEAD", "--format=%s"],
        &["log", "--diff-merges=on", "--oneline", "-3"],
        &["log", "--diff-merges=separate", "--oneline", "-3"],
        &["log", "--diff-merges=c", "--oneline", "-3"],
        &["log", "--dd", "--oneline", "-3"],
        &["-c", "log.diffMerges=first-parent", "log", "-m", "-p", "-2"],
        &[
            "-c",
            "log.diffMerges=combined",
            "log",
            "--diff-merges=on",
            "-1",
        ],
        &["-c", "log.diffMerges=wrong", "log", "-1"],
        &["log", "--diff-merges=wrong", "-1"],
    ]);
}

#[test]
fn combined_with_statistics() {
    cases(&[
        &["log", "-1", "-c", "--stat", "--raw", "--format=%s"],
        &["log", "-1", "-c", "--raw", "--numstat", "--format=%s"],
        &["log", "-1", "--cc", "--stat", "-p", "--raw"],
        &["log", "-1", "--cc", "--summary", "-p", "--format=%s"],
        &["log", "-1", "--cc", "--shortstat", "-p", "--oneline"],
        &["log", "-1", "--cc", "--patch-with-stat", "--summary"],
        &["log", "-2", "-c", "--raw", "--abbrev=4", "--format=%h"],
        &["log", "-2", "--format=", "-p"],
        &["log", "-2", "--format=", "--cc", "--stat"],
    ]);
}

#[test]
fn combined_names_and_paths() {
    cases(&[
        &["log", "-1", "-c", "--name-only", "--format=%s"],
        &["log", "-1", "--cc", "--name-status"],
        &["log", "-1", "-c", "--name-status", "-z", "--format=%s"],
        &["log", "-1", "-c", "--raw", "--combined-all-paths", "-z"],
        &["log", "-1", "--cc", "--combined-all-paths", "--name-only"],
        &["show", "-c", "--combined-all-paths", "-p", "--raw"],
        &["show", "--cc", "-w"],
        &["show", "-c", "-b"],
        &["diff-tree", "-c", "--combined-all-paths", "HEAD"],
        &["diff-tree", "-c", "--name-status", "HEAD"],
        &["diff-tree", "-z", "-c", "HEAD"],
        &["diff-tree", "-z", "HEAD^"],
    ]);
}

/// A merge that renames a file differently on each side and in the
/// result.
#[test]
fn combined_renames() {
    let mut failed = Vec::new();
    for args in [
        &["diff-tree", "-c", "-M", "--combined-all-paths", "HEAD"][..],
        &["diff-tree", "--cc", "-M", "--combined-all-paths", "HEAD"],
        &[
            "diff-tree",
            "-c",
            "-M",
            "--combined-all-paths",
            "--name-status",
            "HEAD",
        ],
        &["show", "-c", "--raw", "--format="],
        &["show", "--cc", "--combined-all-paths", "--format="],
        &["show", "-c", "--stat", "--format="],
        &["log", "-1", "-c", "--name-only", "--combined-all-paths"],
    ] {
        let lines: String = (1..=10).map(|i| format!("{i}\n")).collect();
        let mut fx = Fixture::new()
            .rgit(env!("CARGO_BIN_EXE_rgit"))
            .git(["init", "-q", "-b", "main"])
            .write("base", "x\n")
            .git(["add", "."])
            .git(["commit", "-q", "-m", "init"])
            .git(["branch", "side2"])
            .git(["checkout", "-q", "-b", "side1"])
            .tick()
            .write("name-side1", &lines)
            .git(["add", "."])
            .git(["commit", "-q", "-m", "with"])
            .git(["checkout", "-q", "side2"])
            .tick()
            .write("name-side2", lines.replace("10\n", "ten\n"))
            .git(["add", "."])
            .git(["commit", "-q", "-m", "iam"])
            .git(["checkout", "-q", "side1"])
            .tick()
            .git(["merge", "-q", "--no-commit", "side2"])
            .git(["rm", "-q", "name-side1"])
            .write("name-side2", &(lines.replace("10\n", "ten\n") + "eleven\n"))
            .git(["mv", "name-side2", "name-merged"])
            .git(["add", "."])
            .git(["commit", "-q", "-m", "merged"]);
        if fx.is_skipped() {
            return;
        }
        if let Err(m) = fx.compare(args).output_only().try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

/// A merge stopped with conflicts: `added` (add/add), `f` (content),
/// `d` (deleted by us), `m` merged cleanly, `s` changed in the work tree.
fn conflicted() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("f", "1\n2\n3\n4\n5\n6\n7\n8\n9\n")
        .write("s", "same\n")
        .write("d", "base\n")
        .write("m", "a\nb\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "base"])
        .git(["checkout", "-q", "-b", "side"])
        .tick()
        .write("f", "1\n2\nthree\n4\n5\n6\n7\n8\nnine\n")
        .write("d", "side\n")
        .write("m", "a\nb\nside\n")
        .write("added", "sa\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "side"])
        .git(["checkout", "-q", "main"])
        .tick()
        .write("f", "1\n2\ndrei\n4\n5\n6\n7\n8\n9\n")
        .git(["rm", "-q", "d"])
        .write("m", "main\na\nb\n")
        .write("added", "ma\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "main"]);
    if fx.is_skipped() {
        return fx;
    }
    let _ = fx.git_output(["merge", "-q", "side"]);
    fx.write("s", "same\nextra\n")
}

#[test]
fn conflicted_work_tree() {
    let mut failed = Vec::new();
    for args in [
        &["diff"][..],
        &["diff", "-c"],
        &["diff", "--cc", "--raw"],
        &["diff", "--raw"],
        &["diff", "--stat"],
        &["diff", "-c", "--stat"],
        &["diff", "-c", "--name-status"],
        &["diff", "--patch-with-raw"],
        &["diff", "--base"],
        &["diff", "--ours"],
        &["diff", "--theirs"],
        &["diff", "-0"],
        &["diff", "--", "f"],
        &["diff", "--cc", "--", "d"],
        &["diff-files"],
        &["diff-files", "-p"],
        &["diff-files", "-c"],
        &["diff-files", "--cc", "--raw"],
        &["diff-files", "-1"],
        &["diff-files", "-3", "-p"],
        &["diff-files", "-0"],
    ] {
        let mut fx = conflicted();
        if fx.is_skipped() {
            return;
        }
        if let Err(m) = fx.compare(args).output_only().try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

#[test]
fn diff_of_merge_and_parents() {
    cases(&[
        &["diff", "HEAD^!"],
        &["diff", "HEAD^1^!"],
        &["diff", "-c", "HEAD", "HEAD^", "HEAD^2"],
        &["diff", "--cc", "--raw", "HEAD", "HEAD^", "HEAD^2"],
        &["diff", "-c", "--name-only", "HEAD", "HEAD^", "HEAD^2"],
        &["diff", "HEAD^!", "--", "f"],
    ]);
}

#[test]
fn remerge_diff() {
    let mut failed = Vec::new();
    for args in [
        &["show", "--remerge-diff"][..],
        &["show", "--oneline", "--remerge-diff", "--stat"],
        &["log", "--remerge-diff", "--oneline"],
        &["log", "--diff-merges=remerge", "-1", "--format=%s"],
        &[
            "diff-tree",
            "-r",
            "-p",
            "--remerge-diff",
            "--no-commit-id",
            "HEAD",
        ],
    ] {
        let fx = conflicted();
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx
            .write("f", "1\n2\nresolved\n4\n5\n6\n7\n8\nnine\n")
            .write("added", "both\n")
            .git(["add", "f", "added", "s", "d"])
            .git(["commit", "-q", "-m", "resolved"]);
        if let Err(m) = fx.compare(args).output_only().try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}
