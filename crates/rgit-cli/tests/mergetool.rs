// SPDX-License-Identifier: MIT
//! Differential tests of `mergetool` and `difftool` against the reference
//! git. Only custom tool commands (`mergetool.<tool>.cmd`,
//! `difftool.<tool>.cmd`, `--extcmd`) are used: no real merge tool, GUI or
//! editor is ever started.

use rgit_difftest::{Fixture, Normalizer};

/// `main` and `side` conflict in `f`, `del` (deleted on `side`) and
/// `del2` (deleted on `main`); the merge of `side` is in progress.
fn conflicted() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("f.txt", "base\n\na\n")
        .write("del", "base\n")
        .write("del2", "base\n")
        .write("sub/s", "base\n")
        .git(["add", "."])
        .tick()
        .git(["commit", "-q", "-m", "base"])
        .git(["checkout", "-q", "-b", "side"])
        .write("f.txt", "base\n\nc\n")
        .write("del2", "side\n")
        .write("sub/s", "side\n")
        .git(["rm", "-q", "del"])
        .git(["add", "."])
        .tick()
        .git(["commit", "-q", "-m", "side"])
        .git(["checkout", "-q", "main"])
        .write("f.txt", "local\n\nb\n")
        .write("del", "main\n")
        .write("sub/s", "main\n")
        .git(["rm", "-q", "del2"])
        .git(["add", "."])
        .tick()
        .git(["commit", "-q", "-m", "main"])
        .git(["config", "merge.tool", "mine"])
        .git([
            "config",
            "mergetool.mine.cmd",
            "echo \"B=$BASE L=$LOCAL R=$REMOTE M=$MERGED\"; cat \"$REMOTE\" >\"$MERGED\"",
        ])
        .git(["config", "mergetool.mine.trustExitCode", "true"])
        .git(["config", "mergetool.untrusted.cmd", "true"])
        .git(["config", "mergetool.fails.cmd", "exit 3"])
        .git(["config", "mergetool.fails.trustExitCode", "true"])
}

fn pids() -> Normalizer {
    Normalizer::regex(r"_(BASE|LOCAL|REMOTE|BACKUP)_[0-9]+", "_${1}_PID")
}

fn run(script: &str, input: &str) {
    let mut fx = conflicted();
    if fx.is_skipped() {
        return;
    }
    let alias = format!("alias.t=!git merge side >/dev/null 2>&1; {script}");
    fx.compare(["-c", alias.as_str(), "t"])
        .stdin(input.as_bytes().to_vec())
        .normalize(pids())
        .normalize_state(pids())
        .run();
}

#[test]
fn mergetool_custom_tools() {
    let cases = [
        ("git mergetool -y f.txt", ""),
        ("git mergetool f.txt", ""),
        ("git mergetool --prompt f.txt", "\n"),
        ("git mergetool --prompt f.txt", ""),
        ("git mergetool -y sub", ""),
        ("cd sub && git mergetool -y ../f.txt", ""),
        ("git mergetool -y -t untrusted f.txt", "n\n"),
        ("git mergetool -y -t untrusted f.txt", "y\n"),
        ("git mergetool -y -t fails f.txt sub/s", "y\n"),
        ("git mergetool -y -t fails f.txt sub/s", "n\n"),
        ("git mergetool -y -t nosuch f.txt", ""),
        ("git -c mergetool.keepBackup=false mergetool -y f.txt", ""),
        ("git -c mergetool.hideResolved=true mergetool -y f.txt", ""),
        ("git mergetool -y nosuch", ""),
        ("git mergetool --bogus", ""),
        ("git mergetool -h", ""),
    ];
    for (script, input) in cases {
        run(script, input);
    }
}

#[test]
fn mergetool_deleted_conflicts() {
    let cases = [
        ("git mergetool del", "d\n"),
        ("git mergetool del", "m\n"),
        ("git mergetool del", "a\n"),
        ("git mergetool del", "x\n"),
        ("git mergetool del2", "m\n"),
        ("git mergetool -y", "d\nn\n"),
        ("git -c mergetool.keepTemporaries=true mergetool del", "a\n"),
    ];
    for (script, input) in cases {
        run(script, input);
    }
}

/// `main` and `branch` differ in `file` and `file2`; `branch` adds `new`.
fn diffs() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("file", "main\n")
        .write("file2", "m2\n")
        .git(["add", "."])
        .tick()
        .git(["commit", "-q", "-m", "main"])
        .git(["checkout", "-q", "-b", "branch"])
        .write("file", "branch\n")
        .write("file2", "br2\n")
        .write("new", "new\n")
        .git(["add", "."])
        .tick()
        .git(["commit", "-q", "-m", "branch"])
        .git(["checkout", "-q", "main"])
        .git([
            "config",
            "difftool.show.cmd",
            "cat \"$LOCAL\" \"$REMOTE\"; echo \"M=$MERGED\"",
        ])
        .git(["config", "difftool.bad.cmd", "exit 3"])
}

#[test]
fn difftool_per_file() {
    let cases = [
        ("git difftool -y -t show branch", ""),
        ("git difftool -t show branch", "\nn\n\n"),
        ("git difftool -t show branch", ""),
        ("git difftool -y -x 'sh -c \"cat \\$1\"' branch", ""),
        ("git difftool -y -t bad branch", ""),
        ("git difftool -y -t bad --trust-exit-code branch", ""),
        ("git difftool -y -t nosuch branch", ""),
        ("git difftool -y -t show --rotate-to=file2 branch", ""),
        ("git difftool -y -t show --skip-to=zz branch", ""),
        ("git difftool -g -t show branch", ""),
        ("git difftool -t show -x cat branch", ""),
        ("git -c difftool.prompt=false difftool -t show branch", ""),
        ("GIT_DIFF_TOOL=show git difftool -y branch", ""),
        ("git difftool -h", ""),
    ];
    for (script, input) in cases {
        let mut fx = diffs();
        if fx.is_skipped() {
            return;
        }
        let alias = format!("alias.t=!{script}");
        fx.compare(["-c", alias.as_str(), "t"])
            .stdin(input.as_bytes().to_vec())
            .normalize(Normalizer::regex(r"/[^ ]*-blob-[^/ ]*/", "/TMP/"))
            .run();
    }
}

#[test]
fn difftool_dir_diff() {
    let cases = [
        "git difftool -d -x ls branch | grep -v :",
        "git difftool -d --no-symlinks -x ls branch | grep -v :",
        "git difftool -d -x ls branch main | grep -v :",
        "git difftool -d main main",
        "git difftool -d -t bad branch",
        "git difftool -d -t bad --trust-exit-code branch",
        "printf '#!/bin/sh\\necho changed >\"$2/file\"\\n' >.git/m && chmod +x .git/m && \
         git difftool -d --no-symlinks -x .git/m branch && cat file",
    ];
    for script in cases {
        let mut fx = diffs();
        if fx.is_skipped() {
            return;
        }
        let alias = format!("alias.t=!{script}");
        fx.compare(["-c", alias.as_str(), "t"]).run();
    }
}
