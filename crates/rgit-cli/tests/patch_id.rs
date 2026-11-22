// SPDX-License-Identifier: MIT
//! Differential tests of `rgit patch-id`, `rgit cherry` and the
//! `--cherry-pick`/`--cherry-mark` walks against the reference git.

use rgit_difftest::{Check, Fixture};

/// `main` and `topic` fork at `base`; `main` has a cherry-pick of the
/// middle topic commit (with different whitespace) and a commit of its
/// own; `topic` changes a file to binary.
fn history() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "1\n2\n3\n")
        .write("bin", "x\0y\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "base"])
        .tick()
        .git(["checkout", "-q", "-b", "topic"])
        .write("b", "one\n")
        .git(["add", "b"])
        .git(["commit", "-q", "-m", "B one"])
        .tick()
        .write("a", "1\n2 two\n3\n")
        .git(["commit", "-q", "-am", "C two\n\nbody"])
        .tick()
        .write("bin", "x\0z\n")
        .git(["commit", "-q", "-am", "D binary"])
        .tick()
        .git(["checkout", "-q", "main"])
        .write("c", "upstream\n")
        .git(["add", "c"])
        .git(["commit", "-q", "-m", "upstream"])
        .tick();
    fx.write("a", "1\n2  two\n3\n")
        .git(["commit", "-q", "-am", "C two, picked"])
        .tick()
}

fn same(fx: &mut Fixture, args: &[&str]) {
    fx.compare(args).checks(Check::OUTPUT).run();
}

#[test]
fn patch_id_of_logs_and_mails() {
    let mut fx = history();
    let log = fx.git_stdout(["log", "-p", "--all"]);
    let binary = fx.git_stdout(["log", "-p", "--binary", "--full-index", "--all"]);
    let mails = fx.git_stdout(["format-patch", "--stdout", "--root", "topic"]);
    let stat = fx.git_stdout(["log", "-p", "--stat", "-M", "--all"]);
    for input in [&log, &binary, &mails, &stat] {
        for opt in [
            None,
            Some("--stable"),
            Some("--unstable"),
            Some("--verbatim"),
        ] {
            let mut args = vec!["patch-id"];
            args.extend(opt);
            fx.compare(&args)
                .stdin(input.clone())
                .checks(Check::OUTPUT)
                .run();
        }
    }
    for config in ["patchid.stable=true", "patchid.verbatim=true"] {
        fx.compare(["-c", config, "patch-id"])
            .stdin(log.clone())
            .checks(Check::OUTPUT)
            .run();
    }
    same(&mut fx, &["patch-id", "--stable", "--unstable"]);
    same(&mut fx, &["patch-id", "--verbatim", "--unstable"]);
}

#[test]
fn patch_id_of_crafted_input() {
    let mut fx = history();
    let hunk = "diff --git a/bar b/bar\nindex bdaf90f..31051f6 100644\n\
                --- a/bar\n+++ b/bar\n@@ -2 +2,2 @@\n b\n+c\n";
    let binary = "diff --git a/bin b/bin\nindex 0000000..1111111 100644\n\
                  Binary files a/bin and b/bin differ\n";
    let commit = "commit 1234567890123456789012345678901234567890\n";
    let inputs = [
        format!("{hunk}{binary}"),
        format!("{binary}{hunk}{hunk}"),
        format!("{commit}{hunk}junk\n{hunk}"),
        format!("{hunk}\\ No newline at end of file\n{hunk}"),
        format!("diff --git a/x b/x\nold mode 100644\nnew mode 100755\n\n{hunk}"),
        "diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -x +1 @@\n-a\n-b\n+c\n".to_owned(),
        "diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1,0 +1,2 @@\n-a\n-b\n+c\n".to_owned(),
        "diff --git a/b b/b\n--- a/b\nfoo\n+++ b/b\n@@ -1,3 +1,3 @@\n-a\nxyz\n+b\n c\n".to_owned(),
        format!("{hunk}ABCDEF7890123456789012345678901234567890 more\n{hunk}"),
    ];
    for input in inputs {
        for opt in ["--unstable", "--stable", "--verbatim"] {
            fx.compare(["patch-id", opt])
                .stdin(input.clone())
                .checks(Check::OUTPUT)
                .run();
        }
    }
}

#[test]
fn cherry_and_cherry_walks() {
    let mut fx = history();
    for args in [
        &["cherry", "main", "topic"][..],
        &["cherry", "-v", "main", "topic"],
        &["cherry", "-v", "main", "topic", "topic~2"],
        &["cherry", "--abbrev", "-v", "main", "topic"],
        &["cherry", "--abbrev=9", "main", "topic"],
        &["cherry", "-v", "topic"],
        &["cherry"],
        &["cherry", "a", "b", "c", "d"],
        &["cherry", "nope"],
        &["rev-list", "--cherry-mark", "main...topic"],
        &["rev-list", "--cherry-mark", "--left-right", "main...topic"],
        &["rev-list", "--cherry-pick", "--left-right", "main...topic"],
        &["rev-list", "--cherry-pick", "topic...main"],
        &["rev-list", "--cherry", "main...topic"],
        &["rev-list", "--cherry-mark", "--count", "main...topic"],
        &[
            "rev-list",
            "--cherry-mark",
            "--left-right",
            "--count",
            "main...topic",
        ],
        &["rev-list", "--cherry", "--count", "main...topic"],
        &[
            "rev-list",
            "--cherry-pick",
            "--left-right",
            "--count",
            "main...topic",
        ],
        &["rev-list", "--cherry-mark", "--boundary", "main...topic"],
        &["rev-list", "--cherry-mark", "-n1", "main...topic"],
        &["rev-list", "--cherry-mark", "--cherry-pick", "main...topic"],
        &["rev-list", "--cherry-pick", "--cherry", "main...topic"],
        &["rev-list", "--cherry", "--cherry-pick", "main...topic"],
        &["rev-list", "--cherry", "--left-only", "main...topic"],
        &["rev-list", "--left-only", "--right-only", "main...topic"],
        &["log", "--cherry-mark", "--oneline", "main...topic"],
        &["log", "--cherry-mark", "--format=%m %s", "main...topic"],
        &["log", "--cherry", "--format=%m %s", "main...topic"],
    ] {
        same(&mut fx, args);
    }
    fx = fx
        .git(["checkout", "-q", "topic"])
        .git(["branch", "-q", "--set-upstream-to=main"]);
    same(&mut fx, &["cherry", "-v"]);
}
