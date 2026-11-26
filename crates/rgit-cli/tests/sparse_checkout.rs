// SPDX-License-Identifier: MIT
//! Differential tests of sparse checkouts against the reference git:
//! `rgit sparse-checkout` (cone and non-cone mode), the skip-worktree
//! handling of read-tree, checkout and reset, and the refusals of add, rm
//! and mv outside the sparse checkout. Each step compares output, exit
//! code, index (with the skip-worktree bits), work tree, the configuration
//! and the sparse-checkout file.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .write("folder1/a", "a\n")
        .write("folder2/a", "a\n")
        .write("deep/a", "a\n")
        .write("deep/deeper1/a", "a\n")
        .write("deep/deeper1/deepest/a", "a\n")
        .write("deep/deeper2/a", "a\n");
    // Racily clean index entries would make git see the files as
    // modified; the index is written a second after them.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fx.git(["add", "."])
        .git(["commit", "-q", "-m", "initial"])
        .tick()
}

const FILES: [&str; 3] = ["config.worktree", "info/sparse-checkout", "config"];

/// Runs `args` with both binaries, compares everything (plus the sparse
/// files) and continues from git's result.
#[track_caller]
fn step(fx: &mut Fixture, args: &[&str]) {
    let mut c = fx.compare(args);
    for f in FILES {
        c = c.git_file(f);
    }
    c.run();
}

#[track_caller]
fn step_stdin(fx: &mut Fixture, args: &[&str], stdin: &str) {
    let mut c = fx.compare(args).stdin(stdin.as_bytes().to_vec());
    for f in FILES {
        c = c.git_file(f);
    }
    c.run();
}

#[test]
fn cone_mode() {
    let mut fx = fixture();
    step(&mut fx, &["sparse-checkout", "list"]);
    step(
        &mut fx,
        &["sparse-checkout", "set", "deep/deeper1/deepest/"],
    );
    step(&mut fx, &["sparse-checkout", "list"]);
    step(&mut fx, &["sparse-checkout", "add", "folder1"]);
    step(&mut fx, &["sparse-checkout", "add", "deep"]);
    step(&mut fx, &["sparse-checkout", "set", "a"]);
    step(
        &mut fx,
        &["sparse-checkout", "set", "--skip-checks", "a", "deep"],
    );
    step_stdin(
        &mut fx,
        &["sparse-checkout", "set", "--stdin"],
        "folder2\n\"folder1\"\n",
    );
    step(&mut fx, &["sparse-checkout", "reapply"]);
    step(&mut fx, &["status"]);
    step(&mut fx, &["sparse-checkout", "disable"]);
    step(&mut fx, &["sparse-checkout", "init"]);
    step(
        &mut fx,
        &[
            "sparse-checkout",
            "reapply",
            "--no-cone",
            "--no-sparse-index",
        ],
    );
    step(&mut fx, &["sparse-checkout", "clean"]);
}

#[test]
fn non_cone_mode() {
    let mut fx = fixture();
    step(&mut fx, &["sparse-checkout", "init", "--no-cone"]);
    step(
        &mut fx,
        &["sparse-checkout", "set", "/*", "!/*/", "*folder*"],
    );
    step(&mut fx, &["sparse-checkout", "list"]);
    step(&mut fx, &["sparse-checkout", "add", "a", "deep/a"]);
    step(&mut fx, &["sparse-checkout", "add", "--cone", "x"]);
    fx.compare(["sparse-checkout", "set", "x"])
        .in_dir("deep")
        .run();
    step(&mut fx, &["read-tree", "-mu", "HEAD"]);
    step(&mut fx, &["ls-files", "-t"]);
}

#[test]
fn bad_cone_patterns() {
    for patterns in [
        "/*\n!/*/\n/folder1/**\n",
        "/*\n!/*/\n!/deep/deeper/*/\n",
        "/*\n!/*/\n/\n",
        "/deep/deeper1/a\n",
        "/*\n!b\n",
        "/*\n!/*/\n/foo/\n!/foo/*/\n/foo/\\*/\n",
    ] {
        let mut fx = fixture()
            .git(["config", "core.sparseCheckout", "true"])
            .git(["config", "core.sparseCheckoutCone", "true"])
            .write(".git/info/sparse-checkout", patterns);
        step(&mut fx, &["read-tree", "-mu", "HEAD"]);
        step(&mut fx, &["sparse-checkout", "list"]);
    }
}

#[test]
fn dirty_and_unmerged_paths_stay() {
    let mut fx = fixture().write("folder1/a", "dirty\n");
    step(&mut fx, &["sparse-checkout", "set", "deep"]);
    step(&mut fx, &["sparse-checkout", "reapply"]);
    let mut fx = fx.git(["checkout", "--", "folder1/a"]);
    step(&mut fx, &["sparse-checkout", "reapply"]);
    step(&mut fx, &["sparse-checkout", "disable"]);
}

#[test]
fn checkout_and_reset() {
    let fx = fixture()
        .git(["checkout", "-q", "-b", "other"])
        .write("folder1/a", "changed\n")
        .write("folder2/new", "new\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "other"])
        .tick()
        .git(["checkout", "-q", "main"]);
    let mut fx = fx.git(["sparse-checkout", "set", "folder2"]);
    step(&mut fx, &["checkout", "-q", "other"]);
    step(&mut fx, &["reset", "-q", "--hard", "main"]);
    step(&mut fx, &["reset", "-q", "other"]);
    step(&mut fx, &["checkout", "-q", "-f", "main"]);
    let mut fx = fx.write("folder1/a", "present\n");
    step(&mut fx, &["ls-files", "-t"]);
    step(&mut fx, &["checkout", "-q", "other"]);
}

#[test]
fn add_rm_mv_outside() {
    let mut fx = fixture().git(["sparse-checkout", "set", "folder1"]);
    step(&mut fx, &["add", "deep/a"]);
    step(&mut fx, &["add", "--sparse", "deep/a"]);
    step(&mut fx, &["rm", "folder2/a"]);
    step(&mut fx, &["rm", "--ignore-unmatch", "folder2/a"]);
    step(&mut fx, &["mv", "folder2/a", "folder1/b"]);
    step(&mut fx, &["mv", "folder1/a", "folder2/c"]);
    step(&mut fx, &["mv", "--sparse", "folder1/a", "folder2/c"]);
    step(&mut fx, &["rm", "--sparse", "folder2/a"]);
}

#[test]
fn check_rules() {
    let mut fx = fixture().write("rules", "folder1\n\"deep/deeper1\"\n");
    let paths = "a\nfolder1/a\nfolder2/a\ndeep/a\ndeep/deeper1/x\n\"folder\\\" 1/a\"\n";
    for args in [
        &["sparse-checkout", "check-rules", "--rules-file", "rules"][..],
        &[
            "sparse-checkout",
            "check-rules",
            "--no-cone",
            "--rules-file",
            "rules",
        ],
    ] {
        fx.compare(args)
            .stdin(paths.as_bytes().to_vec())
            .checks(Check::OUTPUT)
            .run();
    }
    fx.compare([
        "sparse-checkout",
        "check-rules",
        "-z",
        "--rules-file",
        "rules",
    ])
    .stdin(b"a\0folder1/a\0deep/b\0".to_vec())
    .checks(Check::OUTPUT)
    .run();
}

#[test]
fn outside_a_repository_and_bare() {
    let mut fx = fixture().git(["clone", "-q", "--bare", ".", "bare.git"]);
    for sub in ["list", "set", "add", "init", "reapply", "disable"] {
        fx.compare(["sparse-checkout", sub])
            .in_dir("bare.git")
            .checks(Check::OUTPUT)
            .run();
    }
    for args in [
        &["sparse-checkout", "-h"][..],
        &["sparse-checkout", "init", "-h"],
        &["sparse-checkout", "set", "-h"],
        &["sparse-checkout", "check-rules", "-h"],
        &["sparse-checkout", "nope"],
        &["sparse-checkout", "add", "--sikp-checks", "x"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}
