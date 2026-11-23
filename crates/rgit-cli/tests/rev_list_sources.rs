// SPDX-License-Identifier: MIT
//! Differential tests of the `rev-list` sources and output options
//! (`--reflog`, `--indexed-objects`, `--exclude-hidden`, `--unpacked`,
//! `--disk-usage`, `--maximal-only`, ...) against the reference git.

use rgit_difftest::{Check, Fixture};

fn commit(fx: Fixture, name: &str) -> Fixture {
    fx.write(name, name)
        .git(["add", name])
        .git(["commit", "-q", "-m", name])
        .tick()
}

/// A merge of a side branch, a rewritten commit (only in the reflog),
/// packed and loose objects, a hidden ref and a staged change.
fn history() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    fx = commit(fx, "a");
    fx = commit(fx, "b");
    fx = fx.git(["checkout", "-q", "-b", "side"]);
    fx = commit(fx, "s1");
    fx = commit(fx, "s2");
    fx = fx.git(["checkout", "-q", "main"]);
    fx = commit(fx, "m1");
    fx = fx
        .git(["merge", "-q", "--no-ff", "-m", "merge", "side"])
        .tick();
    fx = commit(fx, "m2");
    fx = fx.git(["reset", "-q", "--hard", "HEAD~1"]);
    fx = commit(fx, "m3");
    fx = fx.git(["repack", "-q", "-a", "-d"]);
    fx = commit(fx, "loose");
    fx.git(["update-ref", "refs/hidden/x", "HEAD~2"])
        .git(["update-ref", "refs/tags/t", "HEAD~3"])
        .write("a", "changed")
        .git(["add", "a"])
}

#[test]
fn sources_and_output_options() {
    let mut fx = history();
    for args in [
        &["rev-list", "--reflog"][..],
        &["rev-list", "HEAD", "--not", "--reflog"],
        &["rev-list", "--all", "--single-worktree"],
        &["rev-list", "--ignore-missing", "nonexist", "HEAD~1"],
        &[
            "rev-list",
            "--ignore-missing",
            "0000000000000000000000000000000000000000",
        ],
        &["rev-list", "--objects", "--indexed-objects"],
        &[
            "rev-list",
            "-1",
            "--objects",
            "HEAD",
            "--not",
            "--indexed-objects",
        ],
        &["rev-list", "--objects", "--unpacked", "HEAD"],
        &["rev-list", "--unpacked", "--all"],
        &["rev-list", "--disk-usage", "HEAD"],
        &["rev-list", "--disk-usage", "--objects", "HEAD"],
        &["rev-list", "--objects", "HEAD", "--disk-usage=human"],
        &["rev-list", "--objects", "HEAD", "--disk-usage=typo"],
        &["rev-list", "--maximal-only", "HEAD~1", "HEAD~2", "side"],
        &["rev-list", "--maximal-only", "--boundary", "HEAD~1..HEAD"],
        &["rev-list", "--exclude-first-parent-only", "main", "^side~1"],
        &["rev-list", "--exclude-hidden=fetch", "--all"],
        &[
            "-c",
            "transfer.hideRefs=refs/hidden/",
            "rev-list",
            "--exclude-hidden=receive",
            "--all",
        ],
        &[
            "-c",
            "uploadpack.hideRefs=refs/",
            "-c",
            "uploadpack.hideRefs=!refs/tags",
            "rev-list",
            "--exclude-hidden=uploadpack",
            "--all",
        ],
        &[
            "-c",
            "transfer.hideRefs=refs/hidden",
            "rev-list",
            "--all",
            "--exclude-hidden=fetch",
            "--not",
            "--all",
        ],
        &["rev-list", "--exclude-hidden=bogus", "--all"],
        &[
            "rev-list",
            "--exclude-hidden=fetch",
            "--exclude-hidden=fetch",
            "--all",
        ],
        &["rev-list", "--exclude-hidden=fetch", "--branches"],
        &["rev-list", "--alternate-refs"],
        &["rev-list", "-z", "HEAD~2..HEAD"],
        &["rev-list", "-z", "--parents", "--boundary", "HEAD~2..HEAD"],
        &["rev-list", "-z", "--objects", "HEAD~1..HEAD"],
        &["rev-list", "-z", "--left-right", "HEAD...side"],
        &["rev-list", "-z", "--oneline", "HEAD"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}
