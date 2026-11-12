// SPDX-License-Identifier: MIT
//! Differential tests of repository setup through the binary:
//! `rev-parse` location options and `symbolic-ref HEAD` with
//! `GIT_TRACE_SETUP` (as t1510 uses it).

use rgit_difftest::{Check, Fixture};

const LOCATION: [&str; 8] = [
    "--git-dir",
    "--absolute-git-dir",
    "--show-prefix",
    "--show-cdup",
    "--is-bare-repository",
    "--is-inside-work-tree",
    "--is-inside-git-dir",
    "--show-toplevel",
];

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "repo"])
        .git(["init", "-q", "--bare", "bare.git"])
        .write("repo/sub/sub/f", "")
        .write("repo/wt/sub/f", "")
        .write("repo/.git/wt/sub/f", "")
        .write("out/f", "")
}

fn same(fx: &mut Fixture, dir: &str, env: &[(&str, &str)], args: &[&str]) {
    let mut c = fx.compare(args).in_dir(dir).checks(Check::OUTPUT);
    for (k, v) in env {
        c = c.env(k, v);
    }
    c.run();
}

#[test]
fn rev_parse_locations() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let dirs = [
        "repo",
        "repo/sub/sub",
        "repo/.git",
        "repo/.git/wt/sub",
        "bare.git",
        "bare.git/refs",
        "out",
    ];
    for dir in dirs {
        for opt in LOCATION {
            same(&mut fx, dir, &[], &["rev-parse", opt]);
        }
        same(&mut fx, dir, &[], &["rev-parse"]);
        same(
            &mut fx,
            dir,
            &[],
            &["rev-parse", "--show-prefix", "--git-dir", "--show-cdup"],
        );
    }
    let abs = fx.work_dir().join("repo");
    let wt = abs.join("wt");
    let (abs, wt) = (abs.to_str().unwrap(), wt.to_str().unwrap());
    let git_dir = format!("{abs}/.git");
    for env in [
        vec![("GIT_DIR", git_dir.as_str())],
        vec![("GIT_DIR", git_dir.as_str()), ("GIT_WORK_TREE", abs)],
        vec![("GIT_DIR", git_dir.as_str()), ("GIT_WORK_TREE", wt)],
    ] {
        for dir in ["repo", "repo/sub/sub", "repo/wt/sub"] {
            for opt in LOCATION {
                same(&mut fx, dir, &env, &["rev-parse", opt]);
            }
        }
    }
    same(
        &mut fx,
        "repo/sub",
        &[],
        &[
            "--git-dir=../.git",
            "rev-parse",
            "--git-dir",
            "--show-toplevel",
        ],
    );
    same(
        &mut fx,
        "repo/sub",
        &[],
        &[
            "--work-tree=..",
            "--git-dir=../.git",
            "rev-parse",
            "--show-prefix",
        ],
    );
    same(
        &mut fx,
        "repo/.git",
        &[],
        &["--bare", "rev-parse", "--is-bare-repository", "--git-dir"],
    );
    same(
        &mut fx,
        ".",
        &[],
        &["-C", "repo/sub/sub", "rev-parse", "--show-prefix"],
    );
}

#[test]
fn symbolic_ref_trace() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let trace = [("GIT_TRACE_SETUP", "2"), ("GIT_TRACE_BARE", "1")];
    for dir in [
        "repo",
        "repo/sub/sub",
        "repo/.git",
        "repo/.git/wt/sub",
        "bare.git",
        "out",
    ] {
        same(&mut fx, dir, &trace, &["symbolic-ref", "HEAD"]);
        same(&mut fx, dir, &trace, &["symbolic-ref", "--short", "HEAD"]);
    }
    let mut fx = fx
        .git(["-C", "repo", "symbolic-ref", "refs/heads/a", "refs/heads/b"])
        .git(["-C", "repo", "symbolic-ref", "HEAD", "refs/heads/a"]);
    for args in [
        &["symbolic-ref", "HEAD"][..],
        &["symbolic-ref", "--no-recurse", "HEAD"],
        &["symbolic-ref", "refs/heads/a"],
        &["symbolic-ref", "--short", "refs/heads/a"],
        &["symbolic-ref", "nope"],
        &["symbolic-ref", "-q", "nope"],
        &["symbolic-ref"],
        &["symbolic-ref", "-h"],
    ] {
        same(&mut fx, "repo", &[], args);
    }
}

#[test]
fn setup_errors() {
    let mut fx = fixture()
        .write("gf/.git", "garbage\n")
        .git(["init", "-q", "v99"])
        .git([
            "config",
            "--file",
            "v99/.git/config",
            "core.repositoryformatversion",
            "99",
        ]);
    if fx.is_skipped() {
        return;
    }
    same(&mut fx, "gf", &[], &["rev-parse", "--git-dir"]);
    same(&mut fx, "v99", &[], &["rev-parse", "--git-dir"]);
    same(&mut fx, "v99", &[], &["symbolic-ref", "HEAD"]);
    same(
        &mut fx,
        "repo",
        &[("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")],
        &["rev-parse", "--git-dir"],
    );
    same(
        &mut fx,
        "repo",
        &[("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")],
        &["-c", "safe.directory=*", "rev-parse", "--git-dir"],
    );
    same(
        &mut fx,
        "bare.git",
        &[],
        &[
            "-c",
            "safe.bareRepository=explicit",
            "rev-parse",
            "--git-dir",
        ],
    );
    same(
        &mut fx,
        "repo",
        &[("GIT_DIR", "nope")],
        &["rev-parse", "--git-dir"],
    );
}
