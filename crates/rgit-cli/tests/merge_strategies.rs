// SPDX-License-Identifier: MIT
//! Differential tests of the merge strategies (#153): `resolve` with the
//! trivial in-index merge, custom `git-merge-<name>` programs, trying
//! several strategies, `subtree` and `-X subtree=<path>`, and the
//! whitespace options `-X ignore-*`, against the reference git.

use rgit_difftest::{Check, Comparison, Fixture, Normalizer};

/// `main` and `side` change `f` in different places (side also adds `h`);
/// `conf` changes the line `main` changed; `addonly` only adds `a`;
/// `dirt` adds a file `d`.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("f", "1\n2\n3\n4\n5\n")
        .write("g", "g\n")
        .write("del", "del\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "init"])
        .git(["branch", "side"])
        .git(["branch", "conf"])
        .git(["branch", "addonly"])
        .tick()
        .write("f", "1\n2\n3\n4\n5x\n")
        .git(["rm", "-q", "del"])
        .git(["commit", "-q", "-a", "-m", "main1"])
        .git(["checkout", "-q", "side"])
        .tick()
        .write("f", "1x\n2\n3\n4\n5\n")
        .write("h", "h\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "side1"])
        .git(["checkout", "-q", "conf"])
        .tick()
        .write("f", "1\n2\n3\n4\n5c\n")
        .write("h", "conf h\n")
        .write("g", "conf g\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "conf1"])
        .git(["checkout", "-q", "addonly"])
        .tick()
        .write("a", "a\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "addonly1"])
        .git(["checkout", "-q", "main"])
        .tick()
}

/// The conflict marker labels of `resolve` are random temporary names.
/// Objects are not compared: git saves the state of a work tree with
/// local changes with `stash create` before a strategy other than `ort`
/// runs, and the copied fixture's index lacks stat data, so git sees
/// changes there and stores objects nobody refers to.
fn labels(c: Comparison<'_>) -> Comparison<'_> {
    let n = || Normalizer::regex(r"\.merge_file_[A-Za-z0-9]{6}", ".merge_file_X");
    c.normalize(n())
        .normalize_state(n())
        .git_file("MERGE_MODE")
        .skip(Check::Objects)
}

fn cases(prep: impl Fn(Fixture) -> Fixture, cases: &[&[&str]]) {
    cases_skipping(prep, &[], cases);
}

fn cases_skipping(prep: impl Fn(Fixture) -> Fixture, skip: &[Check], cases: &[&[&str]]) {
    let mut failed = Vec::new();
    for args in cases {
        let fx = prep(fixture());
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        let mut c = labels(fx.compare(*args));
        for s in skip {
            c = c.skip(*s);
        }
        if let Err(m) = c.try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

#[test]
fn resolve_strategy() {
    cases(
        |fx| fx,
        &[
            &["merge", "-s", "resolve", "addonly"],
            &["merge", "-s", "resolve", "side"],
            &["merge", "-s", "resolve", "conf"],
            &["merge", "-s", "resolve", "--no-commit", "side"],
            &["merge", "-s", "resolve", "--squash", "side"],
            &["merge", "-s", "resolve", "-Xours", "side"],
            &["merge", "--strategy=resolve", "-m", "msg", "side"],
            &["-c", "pull.twohead=resolve", "merge", "side"],
            &["merge", "-s", "octopus", "side"],
        ],
    );
}

#[test]
fn resolve_with_local_changes() {
    // git saves and restores the local changes around the attempt, which
    // leaves the dirty file's index entry without stat data.
    cases_skipping(
        |fx| fx.write("f", "dirty\n"),
        &[Check::Index],
        &[
            &["merge", "-s", "resolve", "side"],
            &["merge", "-s", "resolve", "addonly"],
        ],
    );
    cases(
        |fx| fx.write("n", "n\n").git(["add", "n"]),
        &[
            &["merge", "-s", "resolve", "side"],
            &["merge", "-s", "resolve", "-s", "ort", "side"],
        ],
    );
}

#[test]
fn several_strategies() {
    cases(
        |fx| fx,
        &[
            &["merge", "-s", "ours", "-s", "resolve", "side"],
            &["merge", "-s", "resolve", "-s", "ort", "side"],
            &["merge", "-s", "ort", "-s", "resolve", "conf"],
            &["merge", "-s", "resolve", "-s", "recursive", "conf"],
            &["-c", "pull.twohead=ort resolve", "merge", "side"],
            &["merge", "-s", "nosuch", "side"],
            &["merge", "-s", "index", "side"],
        ],
    );
}

#[test]
fn custom_strategy() {
    let script = "#!/bin/sh\n\
                  echo \"args: $*\" >&2\n\
                  echo \"action: $GIT_REFLOG_ACTION\" >&2\n\
                  env | grep '^GITHEAD_' | sort >&2\n\
                  eval git read-tree --reset -u \\$$#\n\
                  exit ${STRATEGY_EXIT:-0}\n";
    let prep = |fx: Fixture| {
        if fx.is_skipped() {
            return fx;
        }
        let bin = fx.home_dir().join("strategy-bin");
        std::fs::create_dir_all(&bin).unwrap();
        rgit_difftest::write_executable(bin.join("git-merge-theirs"), script);
        let mut path = vec![bin];
        if let Some(p) = std::env::var_os("PATH") {
            path.extend(std::env::split_paths(&p));
        }
        fx.env("PATH", std::env::join_paths(path).unwrap())
    };
    cases(
        prep,
        &[
            &["merge", "-s", "theirs", "side"],
            &["merge", "-s", "theirs", "-Xfoo", "-Xbar=1", "addonly"],
            &["merge", "-s", "theirs", "side", "addonly"],
            &["merge", "-s", "nosuch", "side"],
        ],
    );
    cases(
        |fx| prep(fx).env("STRATEGY_EXIT", "1"),
        &[&["merge", "-s", "theirs", "side"]],
    );
    cases(
        |fx| prep(fx).env("STRATEGY_EXIT", "2"),
        &[&["merge", "-s", "theirs", "side"]],
    );
}

/// `sub` is an unrelated history merged into `main` under `dir/` (as
/// t6409 does), then changed; `copy` also has a copy of it in `other/`.
fn subtree_fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("top", "top\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "main"])
        .git(["checkout", "-q", "--orphan", "sub"])
        .git(["rm", "-q", "-r", "-f", "."])
        .write("s1", "1\n2\n3\n")
        .write("s2", "s2\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "sub1"])
        .git(["checkout", "-q", "main"])
        .git([
            "merge",
            "-q",
            "-s",
            "ours",
            "--no-commit",
            "--allow-unrelated-histories",
            "sub",
        ])
        .git(["read-tree", "--prefix=dir/", "-u", "sub"])
        .git(["commit", "-q", "-m", "merge sub"])
        .tick()
        .write("dir/s1", "1\n2\n3x\n")
        .git(["commit", "-q", "-a", "-m", "main change"])
        .git(["checkout", "-q", "sub"])
        .tick()
        .write("s1", "1x\n2\n3\n")
        .write("s3", "s3\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "sub2"])
        .git(["checkout", "-q", "-b", "copy", "main"])
        .git(["read-tree", "--prefix=other/", "-u", "sub~1"])
        .git(["commit", "-q", "-m", "copy sub"])
        .git(["checkout", "-q", "main"])
        .tick()
}

#[test]
fn subtree() {
    let mut failed = Vec::new();
    for args in [
        &["merge", "-s", "subtree", "sub"][..],
        &["merge", "-Xsubtree=dir", "sub"],
        &["merge", "-Xsubtree=dir/", "sub"],
        &["merge", "-s", "subtree", "-Xsubtree=dir", "sub"],
        &["merge", "-Xsubtree=other", "sub"],
    ] {
        let mut fx = subtree_fixture();
        if args.contains(&"-Xsubtree=other") {
            fx = fx.git(["checkout", "-q", "copy"]);
        }
        if fx.is_skipped() {
            return;
        }
        if let Err(m) = fx.compare(args).git_file("AUTO_MERGE").try_run() {
            failed.push(m.to_string());
        }
    }
    // Merging the project into the subproject strips the prefix.
    let mut fx = subtree_fixture().git(["checkout", "-q", "sub"]);
    if let Err(m) = fx
        .compare(["merge", "-Xsubtree=dir", "main"])
        .skip(Check::Worktree)
        .try_run()
    {
        failed.push(m.to_string());
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

#[test]
fn whitespace_options() {
    let prep = |fx: Fixture| {
        fx.git(["checkout", "-q", "-b", "ws", "side~1"])
            .write("f", "1\n2 \n3\n4\n5\n")
            .write("w", "a  b\nc\n")
            .git(["add", "."])
            .git(["commit", "-q", "-m", "ws1"])
            .git(["checkout", "-q", "-b", "ws2", "HEAD~1"])
            .write("f", "1\n2\n3\n4\n5\n")
            .write("w", "a b\nc d\n")
            .git(["add", "."])
            .git(["commit", "-q", "-m", "ws2"])
            .git(["checkout", "-q", "ws"])
    };
    cases(
        prep,
        &[
            &["merge", "ws2"],
            &["merge", "-Xignore-space-change", "ws2"],
            &["merge", "-Xignore-all-space", "ws2"],
            &["merge", "-Xignore-space-at-eol", "ws2"],
            &["merge", "-Xignore-cr-at-eol", "ws2"],
            &[
                "merge-tree",
                "--write-tree",
                "-Xignore-all-space",
                "ws",
                "ws2",
            ],
        ],
    );
}
