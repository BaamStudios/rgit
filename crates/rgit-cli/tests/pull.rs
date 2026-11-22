// SPDX-License-Identifier: MIT
//! Differential tests of `pull` (#167) against the reference git: the
//! choice between fast-forward, merge and the divergent-branches error
//! (`--ff`, `--no-ff`, `--ff-only`, `--[no-]rebase`, `pull.ff`,
//! `pull.rebase`), the messages when nothing was fetched for merging, a
//! fetch that moves the checked-out branch, pulling into an unborn branch,
//! the refusals before fetching, and pulls from a clone's `origin`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use rgit_difftest::Fixture;

/// `c0`; `c1`, `c2` and `c3` each add a file on top of it (tags and
/// branches `b1`..`b3`); `main` is at `c0`.
fn fixture() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("c0.c", "c0\n")
        .git(["add", "c0.c"])
        .git(["commit", "-q", "-m", "c0"])
        .git(["tag", "c0"]);
    for c in ["c1", "c2", "c3"] {
        fx = fx
            .tick()
            .git(["checkout", "-q", "-b", &format!("b{}", &c[1..]), "c0"])
            .write(format!("{c}.c"), format!("{c}\n"))
            .git(["add", "."])
            .git(["commit", "-q", "-m", c])
            .git(["tag", c]);
    }
    fx.git(["checkout", "-q", "main"]).tick()
}

/// `PATH` with the directory of the reference git first, so that a fetch
/// over a local path finds its `git-upload-pack`.
fn server_path(fx: &Fixture) -> OsString {
    let mut dirs: Vec<PathBuf> = fx
        .reference_git()
        .parent()
        .into_iter()
        .map(Path::to_path_buf)
        .collect();
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(dirs).unwrap()
}

/// Runs each case in a fresh fixture prepared by `prep` (in `dir`);
/// reports every differing case.
fn cases_in(dir: &str, prep: impl Fn(Fixture) -> Fixture, cases: &[&[&str]]) {
    let mut failed = Vec::new();
    for args in cases {
        let fx = prep(fixture());
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        let path = server_path(&fx);
        let c = fx
            .compare(*args)
            .in_dir(dir)
            .repo(dir)
            .env("PATH", path)
            .git_file("FETCH_HEAD")
            .git_file("MERGE_HEAD")
            .git_file("MERGE_MSG")
            .git_file("SQUASH_MSG")
            .git_file("ORIG_HEAD");
        if let Err(m) = c.try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

fn cases(prep: impl Fn(Fixture) -> Fixture, cases: &[&[&str]]) {
    cases_in("", prep, cases);
}

#[test]
fn fast_forward_possible() {
    cases(
        |fx| fx,
        &[
            &["pull", ".", "c1"],
            &["pull", "-q", ".", "c1"],
            &["pull", "-v", ".", "c1"],
            &["pull", "-q", "-v", ".", "c1"],
            &["pull", "-n", ".", "c1"],
            &["pull", "--ff", ".", "c1"],
            &["pull", "--no-ff", ".", "c1"],
            &["pull", "--ff-only", ".", "c1"],
            &["pull", "--no-rebase", ".", "c1"],
            &["pull", "--no-rebase", "--no-ff", ".", "c1"],
            &["pull", "--rebase", ".", "c1"],
            &["pull", "--rebase", "--no-ff", ".", "c1"],
            &["pull", "--squash", ".", "b1"],
            &["pull", ".", "b1"],
            &["pull", ".", "refs/heads/b1"],
            &["-c", "pull.ff=false", "pull", ".", "c1"],
            &["-c", "pull.ff=only", "pull", ".", "c1"],
            &[
                "-c",
                "pull.ff=true",
                "-c",
                "merge.ff=false",
                "pull",
                ".",
                "c1",
            ],
            &["-c", "merge.ff=false", "pull", ".", "c1"],
            &["-c", "pull.rebase=true", "pull", ".", "c1"],
            &["-c", "branch.main.rebase=true", "pull", ".", "c1"],
            &["pull", "--dry-run", ".", "c1"],
        ],
    );
}

#[test]
fn divergent() {
    cases(
        |fx| fx.git(["reset", "-q", "--hard", "c2"]),
        &[
            &["pull", ".", "c1"],
            &["pull", "-q", ".", "b1"],
            &["--no-advice", "pull", ".", "c1"],
            &["-c", "color.advice=always", "pull", ".", "c1"],
            &["pull", "--no-rebase", ".", "c1"],
            &["pull", "--no-rebase", ".", "b1"],
            &["pull", "--no-rebase", "--log", ".", "b1"],
            &["pull", "--no-rebase", "--no-commit", ".", "b1"],
            &["pull", "--no-rebase", "--squash", ".", "b1"],
            &["pull", "--no-rebase", "-m", "msg", ".", "b1"],
            &["pull", "--ff", ".", "c1"],
            &["pull", "--no-ff", ".", "c1"],
            &["pull", "--ff-only", ".", "c1"],
            &["pull", "--ff-only", "--no-ff", ".", "c1"],
            &["pull", "--rebase", "--ff-only", ".", "c1"],
            &["pull", "--ff-only", "--rebase", ".", "c1"],
            &["pull", "--ff-only", "--no-rebase", ".", "c1"],
            &["-c", "pull.ff=only", "pull", ".", "c1"],
            &["-c", "pull.ff=only", "pull", "--no-rebase", ".", "c1"],
            &[
                "-c",
                "pull.ff=only",
                "-c",
                "pull.rebase=true",
                "pull",
                ".",
                "c1",
            ],
            &[
                "-c",
                "pull.ff=only",
                "-c",
                "pull.rebase=false",
                "pull",
                ".",
                "c1",
            ],
            &["-c", "pull.rebase=true", "pull", "--ff-only", ".", "c1"],
            &["-c", "pull.rebase=false", "pull", ".", "c1"],
            &["-c", "pull.rebase=false", "pull", "--no-ff", ".", "c1"],
            &["-c", "pull.ff=true", "pull", ".", "c1"],
            &["-c", "pull.ff=false", "pull", ".", "c1"],
            &["pull", ".", "c3", "c1"],
            &["-c", "pull.ff=only", "pull", ".", "c1", "c3"],
            &["-c", "pull.rebase=true", "pull", ".", "c1", "c3"],
            &["pull", ".", "c0"],
            &["-c", "pull.ff=only", "pull", ".", "c0"],
            &["pull", "--rebase=foo", ".", "c1"],
            &["-c", "pull.rebase=bad", "pull", ".", "c1"],
            &["-c", "branch.main.rebase=bad", "pull", ".", "c1"],
            &["-c", "pull.ff=bad", "pull", ".", "c1"],
            &["pull", "--no-rebase", "--cleanup", "invalid", ".", "c1"],
        ],
    );
}

#[test]
fn nothing_to_merge() {
    cases(
        |fx| {
            fx.git(["remote", "add", "other", "."])
                .git(["config", "branch.main.remote", "."])
        },
        &[
            &["pull", ".", "refs/nonexisting1/*:refs/nonexisting2/*"],
            &[
                "pull",
                "--rebase",
                ".",
                "refs/nonexisting1/*:refs/nonexisting2/*",
            ],
            &["pull", "other"],
            &["pull", "--rebase", "other"],
            &["pull"],
            &["pull", "--rebase"],
            &["-c", "branch.main.merge=refs/heads/nonexisting", "pull"],
            &["-c", "branch.main.merge=refs/heads/b1", "pull"],
        ],
    );
    cases(
        |fx| fx.git(["checkout", "-q", "--detach"]),
        &[&["pull"], &["pull", "--rebase"], &["pull", ".", "c1"]],
    );
    cases(
        |fx| fx.git(["remote", "add", "only", "."]),
        &[&["pull"], &["pull", "--rebase"]],
    );
    cases(|fx| fx, &[&["pull"], &["pull", "--rebase"]]);
}

#[test]
fn refused_before_fetching() {
    let conflict = |fx: Fixture| {
        let fx = fx
            .git(["reset", "-q", "--hard", "c2"])
            .write("c1.c", "other\n")
            .git(["add", "c1.c"])
            .git(["commit", "-q", "-m", "x"]);
        // The merge stops with a conflict (and fails).
        fx.git_output(["merge", "c1"]);
        fx
    };
    cases(
        conflict,
        &[
            &["pull", ".", "c1"],
            &["-c", "advice.resolveConflict=false", "pull", ".", "c1"],
        ],
    );
    let concluded = move |fx: Fixture| conflict(fx).git(["add", "c1.c"]);
    cases(concluded, &[&["pull", ".", "c1"]]);
    let dirty = |fx: Fixture| fx.write("c0.c", "changed\n");
    cases(
        dirty,
        &[
            &["pull", "--rebase", ".", "c1"],
            &["pull", "--rebase", "--ff-only", ".", "c1"],
            &["pull", ".", "c1"],
        ],
    );
    let both = |fx: Fixture| {
        fx.write("c0.c", "changed\n")
            .write("new", "new\n")
            .git(["add", "new"])
    };
    cases(both, &[&["pull", "--rebase", ".", "c1"]]);
    let staged = |fx: Fixture| fx.write("new", "new\n").git(["add", "new"]);
    cases(staged, &[&["pull", "--rebase", ".", "c1"]]);
}

#[test]
fn fetch_moves_current_branch() {
    cases(
        |fx| {
            fx.git(["branch", "n1", "c1"])
                .git(["checkout", "-q", "b1"])
                .git(["reset", "-q", "--hard", "c0"])
        },
        &[
            &["pull", ".", "n1:b1"],
            &["pull", "--no-rebase", ".", "b3:b1"],
        ],
    );
    cases(
        |fx| {
            fx.git(["branch", "n1", "c1"])
                .git(["checkout", "-q", "b1"])
                .git(["reset", "-q", "--hard", "c0"])
                .write("c1.c", "conflict\n")
        },
        &[&["pull", ".", "n1:b1"]],
    );
}

#[test]
fn into_unborn_branch() {
    let void = |fx: Fixture| fx.git(["init", "-q", "-b", "main", "void"]);
    cases_in(
        "void",
        void,
        &[
            &["pull", ".."],
            &["pull", "..", "main"],
            &["pull", "..", "b1"],
            &["pull", "..", "b1:b1"],
            &["pull", "--rebase", "..", "b1"],
            &["pull", "..", "b1", "b2"],
        ],
    );
    cases_in(
        "void",
        |fx| void(fx).write("void/c1.c", "untracked\n"),
        &[&["pull", "..", "b1"]],
    );
    cases_in(
        "void",
        |fx| {
            void(fx)
                .write("void/c1.c", "staged\n")
                .git(["-C", "void", "add", "c1.c"])
        },
        &[&["pull", "..", "b1"], &["pull", "--rebase", "..", "b1"]],
    );
    cases_in(
        "void",
        |fx| {
            void(fx)
                .write("void/new", "new\n")
                .git(["-C", "void", "add", "new"])
        },
        &[&["pull", "..", "b1"]],
    );
}

#[test]
fn from_origin() {
    let clone = |fx: Fixture| {
        fx.git(["clone", "-q", ".", "cl"])
            .git(["commit", "-q", "--allow-empty", "-m", "upstream"])
            .tick()
    };
    cases_in(
        "cl",
        clone,
        &[
            &["pull"],
            &["pull", "-q"],
            &["pull", "origin"],
            &["pull", "origin", "main"],
            &["pull", "origin", "b1"],
            &["pull", "--no-rebase", "origin", "b1"],
            &["pull", "--rebase"],
            &["pull", "--ff-only"],
        ],
    );
    cases_in(
        "cl",
        |fx| {
            clone(fx)
                .git(["-C", "cl", "commit", "-q", "--allow-empty", "-m", "local"])
                .tick()
        },
        &[
            &["pull"],
            &["pull", "--no-rebase"],
            &["pull", "--no-rebase", "--no-edit"],
            &["pull", "--ff-only"],
            &["-c", "pull.rebase=false", "pull"],
        ],
    );
    cases_in(
        "cl/sub",
        |fx| {
            clone(fx)
                .write("cl/sub/x", "x\n")
                .git(["-C", "cl", "add", "sub/x"])
                .git(["-C", "cl", "commit", "-q", "-m", "sub"])
                .tick()
        },
        &[
            &["pull", "--no-rebase"],
            &["pull", "--no-rebase", "..", "b2"],
        ],
    );
}

/// An annotated tag from FETCH_HEAD is merged with a merge commit (it is
/// not in its natural place under refs/tags/), even with --ff; the merge
/// commit carries the tag message.
#[test]
fn annotated_tag() {
    cases_in(
        "dst",
        |fx| {
            fx.git(["tag", "-a", "-m", "anno c1", "anno1", "c1"])
                .git(["init", "-q", "-b", "main", "dst"])
                .git(["-C", "dst", "pull", "-q", "..", "c0"])
        },
        &[
            &["pull", "--no-edit", "..", "anno1"],
            &["pull", "--ff", "..", "anno1"],
            &["-c", "merge.ff=true", "pull", "..", "anno1"],
            &["-c", "pull.ff=false", "pull", "--no-edit", "..", "anno1"],
            &["pull", "--ff-only", "..", "anno1"],
        ],
    );
}

/// A rebase that is not a fast-forward needs `rebase` (#154): rgit says
/// so instead of rebasing.
#[test]
fn rebase_not_yet_supported() {
    let fx = fixture().git(["reset", "-q", "--hard", "c2"]);
    if fx.is_skipped() {
        return;
    }
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(["pull", "-q", "--rebase", ".", "c1"])
        .current_dir(fx.work_dir())
        .env("HOME", fx.home_dir())
        .env("XDG_CONFIG_HOME", fx.home_dir())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("PATH", server_path(&fx))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(128));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("#154"), "{err}");
}
