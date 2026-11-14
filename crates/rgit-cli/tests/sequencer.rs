// SPDX-License-Identifier: MIT
//! Differential tests of `cherry-pick` and `revert` (#58): `HEAD`, refs,
//! reflogs, objects, the index with its conflict stages, the work tree,
//! the state files (`CHERRY_PICK_HEAD`, `REVERT_HEAD`, `MERGE_MSG`,
//! `AUTO_MERGE`, `.git/sequencer/*`) and every message, against the
//! reference git. Also `commit`, `status` and `reset` during a pick.

use rgit_difftest::{Comparison, Fixture};

/// A history like git's t3510: `initial` writes foo=a and unrelated;
/// `base` foo=b; `unrelatedpick` changes unrelated; `picked` foo=c;
/// `anotherpick` foo=d; `yetanotherpick` foo=e (on `main`).
/// `conflicting` (from initial) changes unrelated; `merge` merges
/// `side` (adds s) into `initial`; `emptyc` is an empty commit on
/// initial. `w` is checked out at `initial`.
fn fixture() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["config", "advice.detachedHead", "false"])
        .write("unrelated", "unrelated\n")
        .git(["add", "unrelated"]);
    for (tag, file, content) in [
        ("initial", "foo", "a\n"),
        ("base", "foo", "b\n"),
        ("unrelatedpick", "unrelated", "reallyunrelated\n"),
        ("picked", "foo", "c\n"),
        ("anotherpick", "foo", "d\n"),
        ("yetanotherpick", "foo", "e\n"),
    ] {
        fx = fx
            .tick()
            .write(file, content)
            .git(["add", file])
            .git(["commit", "-q", "-m", tag])
            .git(["tag", tag]);
    }
    fx.git(["checkout", "-q", "-b", "c", "initial"])
        .tick()
        .write("unrelated", "conflicting\n")
        .git(["commit", "-q", "-a", "-m", "conflicting"])
        .git(["tag", "conflicting"])
        .git(["checkout", "-q", "-b", "side", "initial"])
        .tick()
        .write("s", "s\n")
        .git(["add", "s"])
        .git(["commit", "-q", "-m", "side"])
        .git(["checkout", "-q", "-b", "m", "initial"])
        .tick()
        .write("t", "t\n")
        .git(["add", "t"])
        .git(["commit", "-q", "-m", "mainline"])
        .tick()
        .git(["merge", "-q", "--no-ff", "-m", "merge", "side"])
        .git(["tag", "merge"])
        .git(["checkout", "-q", "-b", "e", "initial"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "emptyc"])
        .git(["tag", "emptyc"])
        .git(["checkout", "-q", "-b", "w", "initial"])
        .tick()
}

fn files(c: Comparison<'_>) -> Comparison<'_> {
    c.git_file("AUTO_MERGE")
        .git_file("COMMIT_EDITMSG")
        .git_file("sequencer/todo")
        .git_file("sequencer/head")
        .git_file("sequencer/abort-safety")
        .git_file("sequencer/opts")
}

/// Runs each case in a fresh fixture prepared by `prep`; reports every
/// differing case.
fn cases(prep: impl Fn(Fixture) -> Fixture, cases: &[&[&str]]) {
    let mut failed = Vec::new();
    for args in cases {
        let fx = prep(fixture());
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        if let Err(m) = files(fx.compare(*args)).try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

/// Runs `args` with the reference git as a preparation step whose
/// failure (a conflict) is expected.
fn stop(fx: Fixture, args: &[&str]) -> Fixture {
    if !fx.is_skipped() {
        fx.git_output(args);
    }
    fx
}

#[test]
fn pick_clean() {
    cases(
        |fx| fx,
        &[
            &["cherry-pick", "base"],
            &["cherry-pick", "base", "unrelatedpick"],
            &["cherry-pick", "unrelatedpick", "base"],
            &["cherry-pick", "initial..picked"],
            &["cherry-pick", "unrelatedpick", "initial..base"],
            &["cherry-pick", "--reverse", "initial..unrelatedpick"],
            &["cherry-pick", "-x", "base"],
            &["cherry-pick", "-s", "base"],
            &["cherry-pick", "-x", "-s", "base"],
            &["cherry-pick", "-n", "base"],
            &["cherry-pick", "-n", "base", "unrelatedpick"],
            &["cherry-pick", "-e", "base"],
            &["cherry-pick", "--ff", "base"],
            &["cherry-pick", "--ff", "base", "unrelatedpick"],
            &["cherry-pick", "-m", "1", "base"],
            &["cherry-pick", "-m", "1", "merge"],
            &["cherry-pick", "-m", "2", "merge"],
            &["cherry-pick", "-1", "base"],
            &["cherry-pick", "-2", "unrelatedpick"],
            &["cherry-pick", "--allow-empty", "emptyc"],
            &["cherry-pick", "--keep-redundant-commits", "emptyc"],
            &["cherry-pick", "-Xtheirs", "picked"],
            &["cherry-pick", "--strategy=ort", "base"],
            &["-c", "core.commentChar=%", "cherry-pick", "-x", "base"],
            &["cherry-pick", "--cleanup=verbatim", "-x", "base"],
        ],
    );
}

#[test]
fn pick_empty() {
    cases(
        |fx| fx,
        &[
            &["cherry-pick", "emptyc"],
            &["cherry-pick", "initial"],
            &["cherry-pick", "--empty=drop", "initial"],
            &["cherry-pick", "--empty=keep", "initial"],
            &["cherry-pick", "--empty=stop", "initial"],
            &["cherry-pick", "--empty=drop", "initial", "base"],
            &["cherry-pick", "--empty=keep", "base", "initial"],
        ],
    );
}

#[test]
fn pick_conflict() {
    cases(
        |fx| fx,
        &[
            &["cherry-pick", "picked"],
            &["cherry-pick", "-n", "picked"],
            &["cherry-pick", "-x", "-s", "picked"],
            &["cherry-pick", "--cleanup=scissors", "picked"],
            &["-c", "commit.cleanup=scissors", "cherry-pick", "picked"],
            &["-c", "merge.conflictStyle=diff3", "cherry-pick", "picked"],
            &["-c", "advice.mergeConflict=false", "cherry-pick", "picked"],
            &["cherry-pick", "picked", "anotherpick"],
            &["cherry-pick", "base..anotherpick"],
            &[
                "cherry-pick",
                "-s",
                "--strategy=recursive",
                "-Xpatience",
                "-Xours",
                "--edit",
                "picked",
                "anotherpick",
            ],
            &[
                "cherry-pick",
                "-x",
                "--allow-empty",
                "--allow-empty-message",
                "--keep-redundant-commits",
                "--cleanup=verbatim",
                "-n",
                "picked",
                "anotherpick",
            ],
            &[
                "cherry-pick",
                "--empty=drop",
                "--ff",
                "--allow-empty",
                "picked",
                "anotherpick",
            ],
            &[
                "cherry-pick",
                "-m",
                "1",
                "--cleanup=scissors",
                "picked",
                "anotherpick",
            ],
        ],
    );
    // modify/delete
    cases(
        |fx| {
            fx.git(["rm", "-q", "foo"])
                .git(["commit", "-q", "-m", "rm foo"])
                .tick()
        },
        &[
            &["cherry-pick", "picked"],
            &["cherry-pick", "picked", "anotherpick"],
            &["revert", "base"],
        ],
    );
    cases(
        |fx| fx.env("GIT_CHERRY_PICK_HELP", "do something else"),
        &[
            &["cherry-pick", "picked"],
            &["cherry-pick", "picked", "anotherpick"],
        ],
    );
}

#[test]
fn revert() {
    cases(
        |fx| fx.git(["checkout", "-q", "-B", "w", "anotherpick"]),
        &[
            &["revert", "HEAD"],
            &["revert", "--no-edit", "HEAD"],
            &["revert", "anotherpick", "picked"],
            &["revert", "base..anotherpick"],
            &["revert", "^base", "anotherpick"],
            &["revert", "--reference", "--no-edit", "HEAD"],
            &["-c", "revert.reference=true", "revert", "HEAD"],
            &["revert", "-s", "HEAD"],
            &["revert", "-n", "HEAD"],
            &["revert", "base"],
            &["revert", "-n", "base"],
            &["revert", "initial"],
            &[
                "revert",
                "-s",
                "--strategy=recursive",
                "-Xpatience",
                "-Xours",
                "--no-edit",
                "picked",
                "yetanotherpick",
            ],
        ],
    );
    cases(
        |fx| fx.git(["checkout", "-q", "-B", "w", "merge"]),
        &[
            &["revert", "-m", "1", "HEAD"],
            &["revert", "-m", "2", "HEAD"],
            &["revert", "--reference", "-m", "1", "HEAD"],
            &["revert", "HEAD"],
            &["revert", "-m", "3", "HEAD"],
        ],
    );
}

#[test]
fn revert_subjects() {
    cases(
        |fx| {
            fx.git(["commit", "-q", "--allow-empty", "-m", "Revert \"B\""])
                .tick()
        },
        &[&["revert", "--allow-empty", "HEAD"]],
    );
    for subject in [
        "Revert \"Revert \"B\"\"",
        "Reapply \"B\"",
        "Revert \"B\" extra",
    ] {
        cases(
            |fx| {
                fx.write("x", "x\n")
                    .git(["add", "x"])
                    .git(["commit", "-q", "-m", subject])
                    .tick()
            },
            &[&["revert", "HEAD"]],
        );
    }
}

#[test]
fn root_and_unborn() {
    cases(
        |fx| {
            fx.git(["checkout", "-q", "--orphan", "o"])
                .git(["rm", "-q", "-rf", "--cached", "."])
                .remove("foo")
                .remove("unrelated")
        },
        &[
            &["cherry-pick", "initial"],
            &["cherry-pick", "--ff", "initial"],
            &["cherry-pick", "initial", "base"],
            &["cherry-pick", "--allow-empty", "emptyc"],
        ],
    );
    cases(
        |fx| fx.git(["checkout", "-q", "-B", "w", "base"]),
        &[&["cherry-pick", "initial"], &["revert", "initial"]],
    );
}

#[test]
fn errors() {
    cases(
        |fx| fx,
        &[
            &["cherry-pick"],
            &["revert"],
            &["cherry-pick", "--nonsense"],
            &["cherry-pick", "--reference", "base"],
            &["revert", "--empty=drop", "base"],
            &["cherry-pick", "nosuch"],
            &["cherry-pick", "base..base"],
            &["cherry-pick", "^base"],
            &["cherry-pick", "-2", "--author=no.such.author", "base"],
            &["cherry-pick", "base^{tree}"],
            &["cherry-pick", "merge"],
            &["cherry-pick", "-m", "3", "merge"],
            &["cherry-pick", "-m", "0", "base"],
            &["cherry-pick", "-m", "foo", "base"],
            &["cherry-pick", "-m"],
            &["cherry-pick", "--empty=foo", "base"],
            &["cherry-pick", "-x", "--ff", "base"],
            &["cherry-pick", "-s", "--ff", "base"],
            &["cherry-pick", "--continue", "--abort"],
            &["cherry-pick", "-n", "--abort"],
            &["cherry-pick", "--keep-redundant-commits", "--continue"],
            &["cherry-pick", "--empty=stop", "--skip"],
            &["cherry-pick", "--continue", "base"],
            &["cherry-pick", "--continue"],
            &["cherry-pick", "--skip"],
            &["revert", "--skip"],
            &["cherry-pick", "--abort"],
            &["revert", "--abort"],
            &["cherry-pick", "--quit"],
            &["cherry-pick", "-"],
        ],
    );
    // Local changes.
    cases(
        |fx| fx.write("foo", "dirty\n"),
        &[
            &["cherry-pick", "base"],
            &["revert", "HEAD"],
            &["cherry-pick", "-n", "unrelatedpick"],
        ],
    );
    cases(
        |fx| fx.write("foo", "dirty\n").git(["add", "foo"]),
        &[
            &["cherry-pick", "base"],
            &["revert", "HEAD"],
            &["cherry-pick", "-n", "unrelatedpick"],
            &["cherry-pick", "unrelatedpick", "base", "picked"],
        ],
    );
    cases(
        |fx| fx.write("unrelated", "dirty\n"),
        &[&["cherry-pick", "unrelatedpick", "base", "picked"]],
    );
    // Unmerged entries.
    cases(
        |fx| stop(fx, &["cherry-pick", "picked"]),
        &[
            &["cherry-pick", "base"],
            &["revert", "HEAD"],
            &["cherry-pick", "base..picked"],
        ],
    );
}

#[test]
fn continue_skip_abort_quit() {
    let single = |fx| stop(fx, &["cherry-pick", "picked"]);
    let resolved = |fx: Fixture| {
        stop(fx, &["cherry-pick", "picked"])
            .write("foo", "c\n")
            .git(["add", "foo"])
    };
    let seq = |fx| stop(fx, &["cherry-pick", "base..yetanotherpick"]);
    let seq_resolved = |fx: Fixture| {
        stop(fx, &["cherry-pick", "unrelatedpick..yetanotherpick"])
            .write("foo", "c\n")
            .git(["add", "foo"])
    };
    cases(
        single,
        &[
            &["cherry-pick", "--continue"],
            &["cherry-pick", "--skip"],
            &["cherry-pick", "--abort"],
            &["cherry-pick", "--quit"],
            &["revert", "--skip"],
            &["revert", "--abort"],
            &["commit", "-q"],
            &["commit", "--amend"],
            &["commit", "foo"],
            &["status"],
            &["cherry-pick", "picked..yetanotherpick"],
        ],
    );
    cases(
        resolved,
        &[
            &["cherry-pick", "--continue"],
            &["revert", "--continue"],
            &["commit"],
            &["commit", "-q", "--no-edit"],
            &["commit", "--reset-author", "-m", "x"],
            &["status"],
        ],
    );
    cases(
        seq,
        &[
            &["cherry-pick", "--continue"],
            &["cherry-pick", "--skip"],
            &["cherry-pick", "--abort"],
            &["cherry-pick", "--quit"],
            &["revert", "--continue"],
            &["status"],
            &["cherry-pick", "base"],
            &["cherry-pick", "base..picked"],
        ],
    );
    cases(
        seq_resolved,
        &[
            &["cherry-pick", "--continue"],
            &["commit", "-q"],
            &["status"],
            &["reset", "-q"],
        ],
    );
    // The commit was made by hand, or HEAD moved.
    cases(
        |fx| seq_resolved(fx).git(["commit", "-q"]),
        &[
            &["cherry-pick", "--continue"],
            &["cherry-pick", "--skip"],
            &["cherry-pick", "--abort"],
            &["status"],
        ],
    );
    cases(
        |fx| seq(fx).git(["reset", "-q", "--hard", "base"]),
        &[&["cherry-pick", "--abort"], &["cherry-pick", "--continue"]],
    );
    // Reverts.
    cases(
        |fx| {
            stop(
                fx.git(["checkout", "-q", "-B", "w", "picked"]),
                &["revert", "picked", "base"],
            )
        },
        &[
            &["revert", "--continue"],
            &["revert", "--skip"],
            &["revert", "--abort"],
            &["cherry-pick", "--abort"],
            &["status"],
        ],
    );
    cases(
        |fx| {
            stop(fx, &["revert", "picked"])
                .write("foo", "r\n")
                .git(["add", "foo"])
        },
        &[
            &["revert", "--continue"],
            &["cherry-pick", "--continue"],
            &["commit", "-q"],
        ],
    );
    // A pick that became empty.
    cases(
        |fx| stop(fx, &["cherry-pick", "base..picked"]).git(["checkout", "HEAD", "--", "foo"]),
        &[&["cherry-pick", "--continue"], &["cherry-pick", "--skip"]],
    );
}

#[test]
fn edited_todo() {
    let resolved = |fx: Fixture| {
        stop(fx, &["cherry-pick", "unrelatedpick..yetanotherpick"])
            .write("foo", "c\n")
            .git(["add", "foo"])
            .git(["commit", "-q"])
    };
    let todo =
        |text: &'static str| move |fx: Fixture| resolved(fx).write(".git/sequencer/todo", text);
    cases(
        todo("pickx 1234567 picked\n"),
        &[&["cherry-pick", "--continue"]],
    );
    cases(todo("revert picked\n"), &[&["cherry-pick", "--continue"]]);
    cases(
        todo("pick nosuchcommit\n"),
        &[&["cherry-pick", "--continue"]],
    );
    cases(
        todo("pick \t picked \t\n# comment\n\npick yetanotherpick\n"),
        &[&["cherry-pick", "--continue"]],
    );
    cases(todo("pick picked\n"), &[&["revert", "--continue"]]);
}

#[test]
fn hooks_and_reflog_action() {
    let hooks = |fx: Fixture| {
        let mut fx = fx;
        for h in [
            "pre-commit",
            "prepare-commit-msg",
            "commit-msg",
            "post-commit",
        ] {
            fx = fx.write_executable(
                format!(".git/hooks/{h}"),
                format!("#!/bin/sh\necho \"{h} $*\" >&2\n"),
            );
        }
        fx
    };
    cases(
        hooks,
        &[
            &["cherry-pick", "base"],
            &["cherry-pick", "-e", "base"],
            &["revert", "--no-edit", "HEAD"],
        ],
    );
    cases(
        |fx| hooks(fx).env("GIT_REFLOG_ACTION", "my action"),
        &[&["cherry-pick", "base"], &["cherry-pick", "--ff", "base"]],
    );
    cases(
        |fx| {
            let fx = stop(hooks(fx), &["cherry-pick", "picked"]);
            fx.write("foo", "c\n").git(["add", "foo"])
        },
        &[&["cherry-pick", "--continue"]],
    );
}

/// What one side did in [`git_git_window`].
#[derive(Debug, PartialEq, Eq)]
struct Picked {
    code: Option<i32>,
    head: String,
    stages: String,
    merge_msg: String,
}

/// Cherry-picks each of the last `RGIT_WINDOW` (default 200) non-merge
/// commits of a git.git clone onto its grandparent, with git and with
/// rgit, and compares the new commit or the conflict (exit code, index
/// stages, `MERGE_MSG`). The clone is `RGIT_UPSTREAM_GIT`, else
/// `upstream/git` next to the crates; without one the test does nothing.
#[test]
#[ignore = "slow; needs a git.git clone (RGIT_UPSTREAM_GIT)"]
fn git_git_window() {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    let src = std::env::var_os("RGIT_UPSTREAM_GIT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../upstream/git"));
    if !src.join(".git").exists() {
        return;
    }
    let Some(git) = rgit_difftest::env::reference_git() else {
        return;
    };
    let rgit = PathBuf::from(env!("CARGO_BIN_EXE_rgit"));
    let n: usize = std::env::var("RGIT_WINDOW")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let tmp = tempfile::tempdir().expect("temp dir");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let run = |bin: &Path, dir: &Path, args: &[&str]| {
        let out = Command::new(bin)
            .args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_EDITOR", "true")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .expect("run");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let srcs = src.to_string_lossy().into_owned();
    let count = n.to_string();
    let args = [
        "-C",
        srcs.as_str(),
        "rev-list",
        "--no-merges",
        "-n",
        count.as_str(),
        "HEAD",
    ];
    let (_, list, _) = run(&git, tmp.path(), &args);
    let dirs: Vec<PathBuf> = ["git", "rgit"].iter().map(|d| tmp.path().join(d)).collect();
    for d in &dirs {
        let ds = d.to_string_lossy().into_owned();
        run(
            &git,
            tmp.path(),
            &["clone", "-q", "--shared", "--no-checkout", &srcs, &ds],
        );
        run(&git, d, &["config", "advice.detachedHead", "false"]);
    }
    let mut failures = Vec::new();
    let (mut compared, mut stopped) = (0, 0);
    for c in list.lines() {
        let grand = format!("{c}^^");
        let mut seen = Vec::new();
        for (d, bin) in dirs.iter().zip([&git, &rgit]) {
            if run(&git, d, &["checkout", "-q", "-f", "--detach", &grand]).0 != Some(0) {
                break;
            }
            run(&git, d, &["clean", "-q", "-f", "-d", "-x"]);
            let (code, out, err) = run(bin, d, &["cherry-pick", "--allow-empty", c]);
            let picked = Picked {
                code,
                head: run(&git, d, &["rev-parse", "HEAD"]).1,
                stages: run(&git, d, &["ls-files", "-u"]).1,
                merge_msg: std::fs::read_to_string(d.join(".git/MERGE_MSG")).unwrap_or_default(),
            };
            seen.push((picked, out, err));
            run(&git, d, &["cherry-pick", "--quit"]);
            run(&git, d, &["reset", "-q", "--hard"]);
        }
        if seen.len() == 2 {
            compared += 1;
            if seen[0].0.code != Some(0) {
                stopped += 1;
            }
        }
        if seen.len() == 2 && seen[0].0 != seen[1].0 {
            failures.push(format!(
                "{c}:\n--- git {:?}\n{}{}--- rgit {:?}\n{}{}",
                seen[0].0, seen[0].1, seen[0].2, seen[1].0, seen[1].1, seen[1].2
            ));
        }
    }
    eprintln!("git_git_window: {compared} picks compared, {stopped} stopped with conflicts");
    assert!(compared > 0, "no commit could be picked");
    assert!(
        failures.is_empty(),
        "{} of {n} differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
