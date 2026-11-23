// SPDX-License-Identifier: MIT
//! Differential tests of the apply back end of `rebase` (#154) against
//! the reference git: `--apply`, `-C<n>`, `--ignore-whitespace`, the
//! fast-forward message, conflicts with `--continue`/`--skip`/`--abort`,
//! the status banner, the options it refuses, and the date and sign-off
//! options with both back ends.

use rgit_difftest::{Comparison, Fixture};

/// `main`: A - B (changes the whitespace of line 2 and line 1 of `f`);
/// `topic` (checked out): A - X (appends to `f`) - Y (changes line 1 of
/// `f` differently, adds `y`).
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["config", "advice.detachedHead", "false"])
        .write("f", "one\ntwo x\nthree\nfour\n")
        .git(["add", "f"])
        .tick()
        .git(["commit", "-q", "-m", "A"])
        .git(["checkout", "-q", "-b", "topic"])
        .write("f", "one\ntwo x\nthree\nfour\nfive\n")
        .git(["commit", "-q", "-am", "X"])
        .write("f", "ONE\ntwo x\nthree\nfour\nfive\n")
        .write("y", "y\n")
        .git(["add", "f", "y"])
        .tick()
        .git(["commit", "-q", "-m", "Y"])
        .git(["checkout", "-q", "main"])
        .write("f", "uno\ntwo  x\nthree\nfour\n")
        .tick()
        .git(["commit", "-q", "-am", "B"])
        .git(["checkout", "-q", "topic"])
        .tick()
}

const STATE_FILES: &[&str] = &[
    "ORIG_HEAD",
    "rebase-apply/head-name",
    "rebase-apply/onto",
    "rebase-apply/orig-head",
    "rebase-apply/rebasing",
    "rebase-apply/next",
    "rebase-apply/last",
    "rebase-apply/original-commit",
    "rebase-apply/rewritten",
];

fn files(mut c: Comparison<'_>) -> Comparison<'_> {
    for f in STATE_FILES {
        c = c.git_file(*f);
    }
    c
}

fn scripts(prep: impl Fn(Fixture) -> Fixture, scripts: &[&str]) {
    let mut failed = Vec::new();
    for s in scripts {
        let fx = prep(fixture());
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        let alias = format!("alias.t=!{s}; echo; git log --format='%s %T %an %ad' --all");
        if let Err(m) = files(fx.compare(["-c", alias.as_str(), "t"])).try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

#[test]
fn apply_back_end() {
    scripts(
        |fx| fx,
        &[
            "git rebase --apply main",
            "git rebase --apply -q main",
            "git rebase --apply -C1 main",
            "git rebase --apply --ignore-whitespace main",
            "git rebase --apply main; git status",
            "git rebase --apply main; git rebase --skip",
            "git rebase --apply main; git rebase --abort",
            "git rebase --apply main; git checkout --theirs f; git add f; git rebase --continue",
            "git rebase --apply --onto main topic~1",
            "git checkout -q main~1 && git rebase --apply main",
            "git checkout -q -b old main~1 && git rebase --apply main",
            "git rebase --apply --signoff --onto main topic~1",
            "git rebase --apply topic",
            "git rebase --apply --autostash --onto main topic~1",
            "echo dirty >>y; git rebase --apply --autostash --onto main topic~1",
        ],
    );
}

#[test]
fn refused_options() {
    scripts(
        |fx| fx,
        &[
            "git rebase --apply --merge main",
            "git rebase --apply -i main",
            "git rebase --apply -x true main",
            "git rebase --apply --rebase-merges main",
            "git rebase --apply --update-refs main",
            "git rebase --apply -s ort main",
            "git rebase --apply --empty=keep main",
            "git rebase --apply --root",
            "git rebase --apply --keep-empty main",
            "git rebase -C4 --keep-empty main",
            "git -c rebase.rebaseMerges=true rebase --apply main",
            "git -c rebase.updateRefs=true rebase --apply main",
            "git -c rebase.updateRefs=true rebase --no-update-refs --apply --onto main topic~1",
            "git rebase -Cx main",
            "git rebase --whitespace=bad main",
        ],
    );
}

#[test]
fn dates_and_signoff() {
    scripts(
        |fx| fx,
        &[
            "git rebase --committer-date-is-author-date HEAD~1",
            "git rebase --apply --committer-date-is-author-date HEAD~1",
            "git rebase --signoff HEAD~1",
            "git rebase --apply --signoff HEAD~1",
            "git rebase -r --committer-date-is-author-date HEAD~1",
            "git rebase --committer-date-is-author-date main; git checkout --theirs f; git add f; git rebase --continue",
        ],
    );
}
