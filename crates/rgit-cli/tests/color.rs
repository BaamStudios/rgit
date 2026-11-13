// SPDX-License-Identifier: MIT
//! Differential tests of coloured output (`--color`, `color.ui`,
//! `color.<cmd>`, `color.<cmd>.<slot>`) of `diff`, `log`, `reflog` and
//! `branch` against the reference git. Output goes to a pipe here, so
//! `auto` means no colour unless `GIT_PAGER_IN_USE` is set.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .env("TERM", "xterm")
        .git(["init", "-q", "-b", "main"])
        .write(
            "m.c",
            "int main(void)\n{\n  a;\n  b;\n  c;\n  d;\n  e;\n  f;\n  g;\n}\nlast",
        )
        .write("gone", "old\n")
        .write("bin", b"x\0y".as_slice())
        .write("w", "one\ntwo\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .git(["branch", "side"])
        .git(["update-ref", "refs/remotes/origin/main", "HEAD"])
        .git([
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ])
        .git(["config", "remote.origin.url", "/nonexistent"])
        .git([
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ])
        .git(["config", "branch.main.remote", "origin"])
        .git(["config", "branch.main.merge", "refs/heads/main"])
        .write(
            "m.c",
            "int main(void)\n{\n  a;\n  b;\n  c;\n  d;\n  e;\n  F; \n\n \t x\n\t y \t\n  g;\n}\nlast2\n",
        )
        .remove("gone")
        .write("newf", "new\n")
        .write("bin", b"x\0z".as_slice())
        .write("w", "one\ntwo\n\n  \n")
        .git(["add", "-A"])
        .git(["commit", "-q", "-m", "two"])
        .write("w", "one\n\t \tx\n  \t\tx \nmid\r\ntwo\n")
}

#[track_caller]
fn all_same(fx: &mut Fixture, cases: &[&[&str]]) {
    let mut failures = Vec::new();
    for args in cases {
        let c = fx
            .compare(*args)
            .checks([Check::ExitCode, Check::Stdout, Check::Stderr]);
        if let Err(m) = c.try_run() {
            failures.push(m.to_string());
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn diff() {
    let mut fx = fixture();
    all_same(
        &mut fx,
        &[
            &["diff", "--color", "HEAD^", "HEAD"],
            &["diff", "--color=always"],
            &["diff", "--color", "-R", "HEAD^", "HEAD"],
            &["diff", "--color", "--stat", "HEAD^", "HEAD"],
            &["diff", "--color", "--stat", "-p", "HEAD^", "HEAD"],
            &[
                "diff",
                "--color",
                "--numstat",
                "--name-status",
                "HEAD^",
                "HEAD",
            ],
            &["diff", "--color=auto", "HEAD^", "HEAD"],
            &["diff", "--color", "--no-color", "HEAD^", "HEAD"],
            &["diff", "--color=never", "HEAD^", "HEAD"],
            &["diff", "--color=bogus"],
            &["-c", "color.ui=always", "diff", "HEAD^", "HEAD"],
            &["-c", "color.diff=always", "diff", "HEAD^", "HEAD"],
            &["-c", "color.ui=always", "-c", "color.diff=auto", "diff"],
            &["-c", "color.diff=never", "diff", "--color", "HEAD^", "HEAD"],
            &["-c", "color.ui=bogus", "diff"],
            &["-c", "color.diff=bogus", "diff"],
            &[
                "-c",
                "color.diff.func=blue",
                "-c",
                "color.diff.context=yellow",
                "-c",
                "color.diff.meta=reverse",
                "-c",
                "color.diff.whitespace=ul magenta",
                "-c",
                "color.diff.frag=#ff0000 bold",
                "diff",
                "--color",
                "HEAD^",
                "HEAD",
            ],
            &["-c", "color.diff.plain=yellow", "diff", "--color"],
            &["-c", "color.diff.new=bogus", "diff", "--color"],
            // Plumbing ignores `color.diff` and `color.ui`.
            &["-c", "color.ui=always", "diff-files", "-p"],
            &["diff-files", "-p", "--color"],
            &["diff-index", "-p", "--color", "HEAD"],
            &["diff-tree", "-p", "--color", "HEAD^", "HEAD"],
        ],
    );
}

#[test]
fn log_and_reflog() {
    let mut fx = fixture();
    all_same(
        &mut fx,
        &[
            &["log", "--color", "--no-decorate"],
            &["log", "--color", "--no-decorate", "--oneline"],
            &["log", "--color", "--no-decorate", "--format=raw", "-1"],
            &["log", "--color", "--no-decorate", "--format=fuller", "-1"],
            &["log", "--color", "--no-decorate", "--parents", "-1"],
            &["log", "--color", "--no-decorate", "--oneline", "--parents"],
            &[
                "-c",
                "color.diff.commit=blue bold",
                "log",
                "--color",
                "--oneline",
                "--no-decorate",
            ],
            &[
                "-c",
                "color.diff.commit=bogus",
                "log",
                "--color",
                "--oneline",
                "--no-decorate",
            ],
            &[
                "-c",
                "color.ui=always",
                "log",
                "--no-color",
                "--oneline",
                "--no-decorate",
            ],
            &["log", "--color=bogus", "--oneline"],
            &["rev-list", "--color", "HEAD"],
            &["reflog", "--color"],
            &["-c", "color.ui=always", "reflog"],
            &["-c", "color.diff=never", "-c", "color.ui=always", "reflog"],
        ],
    );
}

#[test]
fn branch() {
    let mut fx = fixture();
    all_same(
        &mut fx,
        &[
            &["branch", "--color"],
            &["branch", "--color", "-a"],
            &["branch", "--color", "-r"],
            &["branch", "--color", "-v"],
            &["branch", "--color", "-vv"],
            &["branch", "--color", "-a", "-vv"],
            &["branch", "--color=never"],
            &["branch", "--no-color"],
            &["branch", "--color=bogus"],
            &["-c", "color.branch=always", "branch"],
            &[
                "-c",
                "color.ui=always",
                "-c",
                "color.branch=false",
                "branch",
            ],
            &[
                "-c",
                "color.branch.current=blue",
                "-c",
                "color.branch.local=red",
                "-c",
                "color.branch.remote=bold",
                "-c",
                "color.branch.upstream=yellow",
                "branch",
                "--color",
                "-avv",
            ],
            &["-c", "color.branch.current=bogus", "branch", "--color"],
            &["-c", "color.branch.plain=bogus", "branch", "--color"],
        ],
    );
    fx = fx.git(["checkout", "-q", "--detach", "HEAD"]);
    all_same(
        &mut fx,
        &[&["branch", "--color"], &["branch", "--color", "-v"]],
    );
}

/// Environment variables and arguments.
type EnvCase<'a> = (&'a [(&'a str, &'a str)], &'a [&'a str]);

/// Output for a pager (`GIT_PAGER_IN_USE`) is coloured in `auto` mode
/// unless `color.pager` is false or the terminal is dumb.
#[test]
fn pager_in_use() {
    let mut fx = fixture();
    let cases: &[EnvCase<'_>] = &[
        (
            &[("GIT_PAGER_IN_USE", "true")],
            &["log", "--oneline", "--no-decorate"],
        ),
        (&[("GIT_PAGER_IN_USE", "1")], &["diff"]),
        (&[("GIT_PAGER_IN_USE", "false")], &["diff"]),
        (&[("GIT_PAGER_IN_USE", "true"), ("TERM", "dumb")], &["diff"]),
        (
            &[("GIT_PAGER_IN_USE", "true")],
            &["-c", "color.pager=false", "branch"],
        ),
        (
            &[("GIT_PAGER_IN_USE", "true")],
            &["config", "--get-colorbool", "color.foo"],
        ),
        (
            &[("GIT_PAGER_IN_USE", "true")],
            &["config", "--get-colorbool", "color.foo", "false"],
        ),
        (
            &[("GIT_PAGER_IN_USE", "true")],
            &[
                "-c",
                "color.pager=no",
                "config",
                "--get-colorbool",
                "color.foo",
                "false",
            ],
        ),
        (&[], &["config", "--get-colorbool", "color.foo", "true"]),
        (
            &[("TERM", "dumb")],
            &["config", "--get-colorbool", "color.foo", "true"],
        ),
    ];
    let mut failures = Vec::new();
    for (env, args) in cases {
        let mut c = fx
            .compare(*args)
            .checks([Check::ExitCode, Check::Stdout, Check::Stderr]);
        for (k, v) in *env {
            c = c.env(k, v);
        }
        if let Err(m) = c.try_run() {
            failures.push(format!("{env:?}\n{m}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `--no-pager` and `pager.<cmd>=false` set `GIT_PAGER=cat` for the
/// command itself (`git var`) and its children.
#[test]
fn no_pager_exports_cat() {
    let mut fx = fixture().write_executable(
        "tools/git-envdump",
        "#!/bin/sh\necho \"GIT_PAGER=$GIT_PAGER\"\n",
    );
    let exec_path = format!("--exec-path={}", fx.work_dir().join("tools").display());
    all_same(
        &mut fx,
        &[
            &["--no-pager", "var", "GIT_PAGER"],
            &["-c", "pager.var=false", "var", "GIT_PAGER"],
            &["-c", "pager.var=true", "var", "GIT_PAGER"],
            &["-c", "core.pager=x", "var", "GIT_PAGER"],
        ],
    );
    let e = exec_path.as_str();
    all_same(
        &mut fx,
        &[
            &[e, "--no-pager", "envdump"],
            &[e, "-c", "pager.envdump=false", "envdump"],
            &[e, "envdump"],
        ],
    );
}
