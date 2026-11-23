// SPDX-License-Identifier: MIT
//! Differential tests of `log` limited by its diff (`-S`, `-G`,
//! `--find-object`, `--diff-filter`) and of `--merge`, against the
//! reference git.

use rgit_difftest::{Check, Fixture};

fn compare_all(fx: &mut Fixture, cases: &[&[&str]]) {
    let mut failures = Vec::new();
    for argv in cases {
        if let Err(m) = fx.compare(*argv).checks(Check::OUTPUT).try_run() {
            failures.push(format!("{argv:?}\n{}", m.report()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn log_limited_by_diff() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("file0", "1\n2\n3\n")
        .write("dir/sub", "A\nB\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "initial"])
        .tick()
        .write("file0", "1\n2\n3\nF\n")
        .git(["commit", "-q", "-a", "-m", "add F"])
        .tick()
        .git(["checkout", "-q", "-b", "side", "HEAD~1"])
        .write("dir/sub", "A\nB\nF G\n")
        .write("file3", "x\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "side"])
        .tick()
        .git(["checkout", "-q", "main"])
        .git(["merge", "-q", "--no-edit", "side"])
        .tick()
        .write("file0", "1\n2\n3\n")
        .git(["rm", "-q", "dir/sub"])
        .git(["commit", "-q", "-a", "-m", "drop F"]);
    compare_all(
        &mut fx,
        &[
            &["log", "--oneline", "-SF"],
            &["log", "--oneline", "-S", "F"],
            &["log", "-SF", "-p"],
            &["log", "-SF", "--stat", "--max-count=1"],
            &["log", "--oneline", "-GF"],
            &["log", "-GF", "-p", "--pickaxe-all"],
            &["log", "--oneline", "-S", "F.", "--pickaxe-regex"],
            &["log", "--oneline", "-SF", "--", "dir"],
            &["log", "--oneline", "-SF", "--first-parent"],
            &["log", "--oneline", "-SA"],
            &["-c", "log.showRoot=false", "log", "--oneline", "-SA"],
            &["log", "--oneline", "--diff-filter=A"],
            &["log", "--oneline", "--diff-filter=D", "--name-status"],
            &["log", "--oneline", "--find-object=HEAD~1:file0"],
            &["log", "-S"],
        ],
    );
}

#[test]
fn log_merge_shows_the_conflicting_commits() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("conflicting", "base\n")
        .write("other", "o\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "base"])
        .git(["branch", "hello"])
        .git(["branch", "goodbye"])
        .tick()
        .git(["switch", "-q", "hello"])
        .write("conflicting", "hello\n")
        .git(["commit", "-q", "-a", "-m", "hello"])
        .tick()
        .git(["switch", "-q", "goodbye"])
        .write("conflicting", "goodbye\n")
        .write("other", "x\n")
        .git(["commit", "-q", "-a", "-m", "goodbye"])
        .git(["switch", "-q", "hello"]);
    // Without a merge in progress.
    compare_all(&mut fx, &[&["log", "--merge"]]);
    let _ = fx.git_output(["merge", "goodbye"]);
    compare_all(
        &mut fx,
        &[
            &["log", "--merge", "--oneline"],
            &["log", "--merge", "--oneline", "--", "other"],
            &["rev-list", "--merge", "HEAD"],
        ],
    );
}

/// Merges under a diff limit follow the diff format for merges: none
/// without one, any parent with `-m` (only those get an entry), the first
/// parent, combined diffs always, the remerge diff; `show` is limited too.
#[test]
fn log_limited_merges_and_show() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("f", "base\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "base"])
        .tick()
        .git(["switch", "-q", "-c", "side"])
        .write("g", "foo\n")
        .git(["add", "g"])
        .git(["commit", "-q", "-m", "side adds foo"])
        .tick()
        .git(["switch", "-q", "main"])
        .write("h", "bar\n")
        .git(["add", "h"])
        .git(["commit", "-q", "-m", "main adds bar"])
        .tick()
        .git(["merge", "-q", "--no-edit", "side", "-m", "merge"]);
    compare_all(
        &mut fx,
        &[
            &["log", "--format=%s", "-S", "foo"],
            &["log", "--format=%s", "-S", "foo", "-m"],
            &["log", "--oneline", "-S", "foo", "-m"],
            &["log", "--format=%s", "-S", "foo", "-m", "-p"],
            &["log", "--format=%s", "-S", "foo", "--first-parent"],
            &["log", "--format=%s", "-S", "nothing", "-c"],
            &["log", "--format=%s", "-S", "foo", "--cc"],
            &["log", "--format=%s", "-S", "foo", "--remerge-diff"],
            &[
                "log",
                "--format=%s",
                "-S",
                "foo",
                "--diff-merges=first-parent",
            ],
            &[
                "log",
                "--format=%s",
                "--diff-filter=A",
                "--diff-merges=first-parent",
            ],
            &["log", "--format=%s", "-S", "bar", "-m", "--raw"],
            &["log", "--format=%s", "-S", "nothing", "--cc", "--stat"],
            &["log", "--format=%s", "-S", "foo", "--cc", "--stat"],
            &["log", "--format=%s", "--cc", "--stat"],
            &["log", "--format=%s", "--summary"],
            &["log", "--format=%s", "-S", "FOO", "-i"],
            &["log", "--format=%s", "-G", "FO+", "--regexp-ignore-case"],
            &["show", "--format=%s", "-S", "foo", "HEAD"],
            &["show", "--format=%s", "-S", "nothing", "HEAD~1"],
            &["show", "--format=%s", "-S", "bar", "HEAD~1"],
            &[
                "diff-tree",
                "-c",
                "--name-status",
                "--find-object=HEAD:g",
                "HEAD",
            ],
        ],
    );
}
