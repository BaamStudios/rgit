// SPDX-License-Identifier: MIT
//! Differential tests of `rebase` with the merge back end (#154): output,
//! `HEAD`, refs, reflogs, objects, the index with its conflict stages, the
//! work tree and the state files (`.git/rebase-merge/*`, `REBASE_HEAD`,
//! `MERGE_MSG`, `AUTO_MERGE`, `ORIG_HEAD`), against the reference git.
//! Sequences of commands (a stop, then `--continue`) run through a shell
//! alias, so that both sides use their own binary for every step.

use rgit_difftest::{Comparison, Fixture};

/// `main`: A (a) - D (d); `conf`: main + X (b=x, conflicts with B);
/// `upst`: main + a cherry-pick of B; `part`: main + "B+e" (b=b, e: B
/// becomes empty); `topic`: A - B (b) - C (c), checked out;
/// `merged`: topic with a merge of `side` (s) and Y (y).
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["config", "advice.detachedHead", "false"])
        .tick()
        .write("a", "a\n")
        .git(["add", "a"])
        .git(["commit", "-q", "-m", "A"])
        .git(["checkout", "-q", "-b", "topic"])
        .tick()
        .write("b", "b\n")
        .git(["add", "b"])
        .git(["commit", "-q", "-m", "B"])
        .tick()
        .write("c", "c\n")
        .git(["add", "c"])
        .git(["commit", "-q", "-m", "C"])
        .git(["checkout", "-q", "main"])
        .tick()
        .write("d", "d\n")
        .git(["add", "d"])
        .git(["commit", "-q", "-m", "D"])
        .git(["checkout", "-q", "-b", "conf"])
        .tick()
        .write("b", "x\n")
        .git(["add", "b"])
        .git(["commit", "-q", "-m", "X"])
        .git(["checkout", "-q", "-b", "upst", "main"])
        .tick()
        .git(["cherry-pick", "topic~1"])
        .git(["checkout", "-q", "-b", "part", "main"])
        .tick()
        .write("b", "b\n")
        .write("e", "e\n")
        .git(["add", "b", "e"])
        .git(["commit", "-q", "-m", "B+e"])
        .git(["checkout", "-q", "-b", "side", "main~1"])
        .tick()
        .write("s", "s\n")
        .git(["add", "s"])
        .git(["commit", "-q", "-m", "S"])
        .git(["checkout", "-q", "-b", "merged", "topic"])
        .tick()
        .git(["merge", "-q", "--no-ff", "-m", "M", "side"])
        .tick()
        .write("y", "y\n")
        .git(["add", "y"])
        .git(["commit", "-q", "-m", "Y"])
        .git(["checkout", "-q", "topic"])
        .tick()
}

const STATE_FILES: &[&str] = &[
    "AUTO_MERGE",
    "REBASE_HEAD",
    "COMMIT_EDITMSG",
    "rebase-merge/head-name",
    "rebase-merge/onto",
    "rebase-merge/orig-head",
    "rebase-merge/git-rebase-todo",
    "rebase-merge/done",
    "rebase-merge/msgnum",
    "rebase-merge/end",
    "rebase-merge/message",
    "rebase-merge/author-script",
    "rebase-merge/stopped-sha",
    "rebase-merge/patch",
    "rebase-merge/interactive",
    "rebase-merge/quiet",
    "rebase-merge/verbose",
    "rebase-merge/signoff",
    "rebase-merge/strategy",
    "rebase-merge/strategy_opts",
    "rebase-merge/drop_redundant_commits",
    "rebase-merge/keep_redundant_commits",
    "rebase-merge/cdate_is_adate",
    "rebase-merge/ignore_date",
    "rebase-merge/no-reschedule-failed-exec",
];

fn files(mut c: Comparison<'_>) -> Comparison<'_> {
    for f in STATE_FILES {
        c = c.git_file(*f);
    }
    c
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

/// A shell alias `t` running `script` (`git` is the binary under test).
fn script(script: &str) -> [String; 3] {
    [
        "-c".to_owned(),
        format!("alias.t=!{script}"),
        "t".to_owned(),
    ]
}

fn script_cases(prep: impl Fn(Fixture) -> Fixture, scripts: &[&str]) {
    let all: Vec<[String; 3]> = scripts.iter().map(|s| script(s)).collect();
    let refs: Vec<Vec<&str>> = all
        .iter()
        .map(|a| a.iter().map(String::as_str).collect())
        .collect();
    let slices: Vec<&[&str]> = refs.iter().map(Vec::as_slice).collect();
    cases(prep, &slices);
}

#[test]
fn clean() {
    cases(
        |fx| fx,
        &[
            &["rebase", "main"],
            &["rebase", "-q", "main"],
            &["rebase", "--stat", "main"],
            &["rebase", "-v", "main"],
            &["rebase", "-f", "main"],
            &["rebase", "--no-ff", "main"],
            &["rebase", "--onto", "main", "topic~1"],
            &["rebase", "--onto", "main...topic", "main"],
            &["rebase", "--keep-base", "main"],
            &["rebase", "-f", "--keep-base", "main"],
            &["rebase", "--root"],
            &["rebase", "--root", "--onto", "main"],
            &["rebase", "main~1"],
            &["rebase", "-f", "main~1"],
            &["rebase", "--onto", "topic~2", "topic~1"],
            &["rebase", "main", "topic"],
            &["rebase", "main", "main"],
            &["rebase", "conf~1", "topic~1"],
            &["rebase", "--signoff", "main"],
            &["rebase", "--committer-date-is-author-date", "main"],
            &["rebase", "-s", "ort", "-Xpatience", "main"],
            &["rebase", "-m", "main"],
        ],
    );
}

#[test]
fn from_other_branch() {
    cases(
        |fx| fx.git(["checkout", "-q", "main"]),
        &[
            &["rebase", "main", "topic"],
            &["rebase", "main~1", "topic"],
            &["rebase", "main", "topic~1"],
            &["rebase", "topic"],
        ],
    );
    cases(
        |fx| fx.git(["checkout", "-q", "--detach", "topic"]),
        &[
            &["rebase", "main"],
            &["rebase", "main~1"],
            &["rebase", "-f", "main~1"],
        ],
    );
}

#[test]
fn dropped_and_empty() {
    cases(
        |fx| fx,
        &[
            &["rebase", "upst"],
            &["rebase", "-q", "upst"],
            &["rebase", "--reapply-cherry-picks", "upst"],
            &["-c", "advice.skippedCherryPicks=false", "rebase", "upst"],
            &["rebase", "part"],
            &["rebase", "--empty=drop", "part"],
            &["rebase", "--empty=keep", "part"],
            &["rebase", "--empty=stop", "part"],
            &["rebase", "--empty=ask", "part"],
        ],
    );
    cases(
        |fx| {
            fx.git(["commit", "-q", "--allow-empty", "-m", "EMPTY"])
                .tick()
                .write("f", "f\n")
                .git(["add", "f"])
                .git(["commit", "-q", "-m", "F"])
        },
        &[
            &["rebase", "main"],
            &["rebase", "--no-keep-empty", "main"],
            &["rebase", "--keep-empty", "main"],
        ],
    );
    cases(
        |fx| fx.git(["checkout", "-q", "merged"]),
        &[&["rebase", "main"], &["rebase", "conf~1"]],
    );
}

#[test]
fn conflict_stop() {
    cases(
        |fx| fx,
        &[
            &["rebase", "conf"],
            &["rebase", "-q", "conf"],
            &["rebase", "-v", "conf"],
            &["-c", "advice.mergeConflict=false", "rebase", "conf"],
            &["-c", "merge.conflictStyle=diff3", "rebase", "conf"],
            &["rebase", "--signoff", "-s", "ort", "-Xpatience", "conf"],
            &[
                "rebase",
                "--empty=keep",
                "--committer-date-is-author-date",
                "conf",
            ],
        ],
    );
}

#[test]
fn continue_skip_abort_quit() {
    script_cases(
        |fx| fx,
        &[
            "git rebase conf; echo resolved >b; git add b; git rebase --continue",
            "git rebase conf; git rebase --continue",
            "git rebase conf; echo r >b; git add b; echo z >c; git rebase --continue",
            "git rebase conf; git checkout HEAD -- b; git rebase --continue",
            "git rebase conf; echo r >b; git add b; git commit -q -m mine; git rebase --continue",
            "git rebase conf; git rebase --skip",
            "git rebase conf; git rebase --abort",
            "git rebase conf; git rebase --quit",
            "git rebase conf; git rm -q a; echo precious >a; git rebase --abort",
            "git rebase conf; git rm -q a; echo precious >a; git rebase --skip",
            "git rebase --signoff conf; echo r >b; git add b; git rebase --continue",
            "git rebase --empty=stop part; git rebase --continue",
            "git rebase --empty=stop part; git rebase --skip",
            "git rebase --empty=stop part; git commit -q --allow-empty --no-edit; git rebase --continue",
            "git checkout -q --detach topic; git rebase conf; git rebase --abort",
            "git rebase conf; git status",
            "git rebase conf; echo r >b; git add b; git status",
            "git rebase conf; git status -sb; git status --porcelain=v2 --branch",
            "git rebase conf; echo r >b; git add b; git commit",
            "git rebase conf; git rebase main",
            "git rebase conf; git switch main",
        ],
    );
    // A rebase of the apply back end, started by git: --abort and --quit.
    let apply = |fx: Fixture| {
        if !fx.is_skipped() {
            let _ = fx.git_output(["rebase", "--apply", "conf"]);
        }
        fx
    };
    script_cases(
        apply,
        &[
            "git rebase --abort; ls .git; git status --porcelain",
            "git rebase --quit; ls .git; git status --porcelain",
        ],
    );
}

#[test]
fn errors() {
    cases(
        |fx| fx,
        &[
            &["rebase", "--continue"],
            &["rebase", "--skip"],
            &["rebase", "--abort"],
            &["rebase", "--quit"],
            &["rebase", "nope"],
            &["rebase", "main", "nope"],
            &["rebase", "--onto", "nope", "main"],
            &["rebase", "--empty=foo", "main"],
            &["rebase", "--keep-base", "--onto", "main", "main"],
            &["rebase", "--keep-base", "--root"],
            &["rebase", "--root", "--fork-point"],
            &["rebase", "main", "topic", "extra"],
            &["rebase", "--continue", "main"],
            &["rebase", "-v", "--abort"],
            &["rebase", "--abort", "-q"],
            &["rebase", "--no-verify", "--continue"],
            &["rebase"],
        ],
    );
    // A branch checked out in another worktree.
    cases(
        |fx| fx.git(["worktree", "add", "-q", "wt", "main"]),
        &[&["rebase", "main", "main"], &["rebase", "topic~1", "main"]],
    );
    cases(
        |fx| fx.write("a", "dirty\n"),
        &[&["rebase", "main"], &["rebase", "--autostash", "main"]],
    );
    cases(
        |fx| fx.write("a", "dirty\n").git(["add", "a"]),
        &[
            &["rebase", "main"],
            &["-c", "rebase.autoStash=true", "rebase", "main"],
            &["rebase", "--autostash", "main~1"],
        ],
    );
    cases(|fx| fx.git(["checkout", "-q", "--detach"]), &[&["rebase"]]);
}

#[test]
fn autostash() {
    script_cases(
        |fx| fx.write("a", "dirty\n"),
        &[
            "git rebase --autostash conf; git rebase --abort",
            "git rebase --autostash conf; git rebase --skip",
            "git rebase --autostash conf; git rebase --quit; git stash list",
        ],
    );
    cases(
        |fx| fx.write("d", "local\n"),
        &[&["rebase", "--autostash", "main"]],
    );
}

#[test]
fn upstream_and_fork_point() {
    let tracking = |fx: Fixture| {
        fx.git(["config", "branch.topic.remote", "."]).git([
            "config",
            "branch.topic.merge",
            "refs/heads/main",
        ])
    };
    cases(
        tracking,
        &[
            &["rebase"],
            &["rebase", "--no-fork-point"],
            &["-c", "rebase.forkPoint=false", "rebase"],
            &["rebase", "--keep-base"],
        ],
    );
    // `main` rewound: its old tip is in the reflog and in `topic`.
    cases(
        |fx| {
            tracking(
                fx.git(["checkout", "-q", "-b", "t2", "main"])
                    .tick()
                    .write("u", "u\n")
                    .git(["add", "u"])
                    .git(["commit", "-q", "-m", "U"])
                    .git(["branch", "-f", "main", "t2"])
                    .git(["branch", "-f", "main", "main~1"])
                    .git(["checkout", "-q", "topic"])
                    .git(["reset", "-q", "--hard", "t2"])
                    .tick()
                    .write("v", "v\n")
                    .git(["add", "v"])
                    .git(["commit", "-q", "-m", "V"]),
            )
        },
        &[
            &["rebase"],
            &["rebase", "--fork-point", "main"],
            &["rebase", "main"],
        ],
    );
}

#[test]
fn hooks_and_reflog_action() {
    let hooks = |fx: Fixture| {
        fx.write_executable(
            ".git/hooks/post-rewrite",
            "#!/bin/sh\necho \"post-rewrite $*\" >&2\ncat >&2\n",
        )
        .write_executable(
            ".git/hooks/post-commit",
            "#!/bin/sh\necho post-commit >&2\n",
        )
        .write_executable(
            ".git/hooks/post-checkout",
            "#!/bin/sh\necho \"post-checkout $*\" >&2\n",
        )
    };
    cases(
        hooks,
        &[
            &["rebase", "main"],
            &["rebase", "-f", "main"],
            &["rebase", "--root"],
            &["rebase", "main~1"],
        ],
    );
    script_cases(
        hooks,
        &["git rebase conf; echo r >b; git add b; git rebase --continue"],
    );
    // Up to date: only the branch is checked out.
    cases(
        |fx| hooks(fx.git(["checkout", "-q", "main"])),
        &[&["rebase", "main~1", "topic"]],
    );
    cases(
        |fx| {
            fx.write_executable(
                ".git/hooks/pre-rebase",
                "#!/bin/sh\necho \"pre-rebase $*\" >&2\nexit 1\n",
            )
        },
        &[
            &["rebase", "main"],
            &["rebase", "main", "topic"],
            &["rebase", "--no-verify", "main"],
            &["rebase", "main~1"],
        ],
    );
    cases(
        |fx| fx.env("GIT_REFLOG_ACTION", "custom"),
        &[&["rebase", "main"], &["rebase", "--root"]],
    );
}

#[derive(Debug, PartialEq, Eq)]
struct Rebased {
    code: Option<i32>,
    head: String,
    stages: String,
    done: String,
    todo: String,
    message: String,
}

/// Rebases the topic branches of the last `RGIT_WINDOW` (default 50)
/// merges on the first-parent history of a git.git clone onto the commit
/// they were merged into (`git rebase M^1` on `M^2`), with git and with
/// rgit, and compares the result (`HEAD`) or the stop state (exit code,
/// index stages, `done`, `git-rebase-todo`, `message`). The clone is
/// `RGIT_UPSTREAM_GIT`, else `upstream/git` next to the crates; without one
/// the test does nothing.
#[test]
#[ignore = "slow; needs a git.git clone (RGIT_UPSTREAM_GIT)"]
fn git_git_topics() {
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
        .unwrap_or(50);
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
        "--first-parent",
        "--merges",
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
    let read =
        |d: &Path, f: &str| std::fs::read_to_string(d.join(".git").join(f)).unwrap_or_default();
    let mut failures = Vec::new();
    let (mut compared, mut stopped) = (0, 0);
    for m in list.lines() {
        let (base, topic) = (format!("{m}^1"), format!("{m}^2"));
        let mut seen = Vec::new();
        for (d, bin) in dirs.iter().zip([&git, &rgit]) {
            if run(&git, d, &["checkout", "-q", "-f", "--detach", &topic]).0 != Some(0) {
                break;
            }
            run(&git, d, &["clean", "-q", "-f", "-d", "-x"]);
            let (code, out, err) = run(bin, d, &["rebase", &base]);
            let r = Rebased {
                code,
                head: run(&git, d, &["rev-parse", "HEAD"]).1,
                stages: run(&git, d, &["ls-files", "-u"]).1,
                done: read(d, "rebase-merge/done"),
                todo: read(d, "rebase-merge/git-rebase-todo"),
                message: read(d, "rebase-merge/message"),
            };
            seen.push((r, out, err));
            run(&git, d, &["rebase", "--quit"]);
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
                "{m}:\n--- git {:?}\n{}{}--- rgit {:?}\n{}{}",
                seen[0].0, seen[0].1, seen[0].2, seen[1].0, seen[1].1, seen[1].2
            ));
        }
    }
    eprintln!("git_git_topics: {compared} topics compared, {stopped} stopped with conflicts");
    assert!(compared > 0, "no topic could be rebased");
    assert!(
        failures.is_empty(),
        "{} of {n} differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
