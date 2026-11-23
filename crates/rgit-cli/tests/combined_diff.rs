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
