// SPDX-License-Identifier: MIT
//! `branch` list filters (`--contains`, `--merged`, `--points-at`),
//! `--sort`, `--format`, `-i`, `--omit-empty`, `--create-reflog` and
//! `--edit-description` against the reference git.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .git(["commit", "-q", "--allow-empty", "-m", "c1"])
        .git(["branch", "side"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "c2"])
        .git(["branch", "next"])
        .git(["checkout", "-q", "side"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "s1"])
        .git(["checkout", "-q", "main"])
        .git(["branch", "Zed", "side"])
        .git(["branch", "alpha", "HEAD~1"])
        .git(["tag", "-a", "-m", "t", "v1", "side"])
        .git(["update-ref", "refs/remotes/origin/main", "main"])
        .git(["update-ref", "refs/remotes/origin/side", "side"])
}

#[test]
fn list_filters_sort_format() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["branch", "--contains", "side"],
        &["branch", "--contains"],
        &["branch", "--no-contains", "side"],
        &["branch", "--merged"],
        &["branch", "--merged", "side"],
        &["branch", "--no-merged"],
        &["branch", "--no-merged", "main", "main"],
        &["branch", "--contains", "main", "-a"],
        &["branch", "-r", "--contains", "main"],
        &["branch", "--points-at", "side"],
        &["branch", "--points-at", "v1"],
        &["branch", "--sort=-refname"],
        &["branch", "--sort=committerdate", "-v"],
        &["branch", "-i", "--sort=refname"],
        &["-c", "branch.sort=-refname", "branch"],
        &["-c", "branch.sort=-refname", "branch", "--no-sort"],
        &["branch", "--format=%(refname:short) %(objectname:short)"],
        &["branch", "--format=%(refname)", "-a"],
        &[
            "branch",
            "--format=%(HEAD)%(refname:short)",
            "--sort=-refname",
        ],
        &[
            "branch",
            "--omit-empty",
            "--format=%(if:equals=side)%(refname:short)%(then)x%(end)",
        ],
        &["branch", "--contains", "main^{tree}"],
        &["branch", "--contains", "nope"],
        &["branch", "--merged", "nope"],
        &["branch", "--list", "--contains", "side", "s*"],
        &["branch", "-l", "-i", "z*"],
    ];
    for args in cases {
        fx.compare(*args).checks(Check::OUTPUT).run();
    }
    let mut fx = fx.git(["checkout", "-q", "--detach"]);
    for args in [
        &["branch", "--format=%(refname) %(HEAD)"][..],
        &["branch", "--contains", "main"][..],
        &["branch", "--points-at", "HEAD"][..],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}

#[test]
fn create_reflog_and_description() {
    let fx = fixture()
        .write_executable(
            "editor.sh",
            "#!/bin/sh\nprintf 'my desc\\n\\n# c\\nline2\\n' >\"$1\"\n",
        )
        .write_executable("empty.sh", "#!/bin/sh\n: >\"$1\"\n");
    if fx.is_skipped() {
        return;
    }
    let editor = fx.work_dir().join("editor.sh");
    let empty = fx.work_dir().join("empty.sh");
    let mut fx = fx.git(["config", "branch.side.description", "old text"]);
    let checks = [
        Check::ExitCode,
        Check::Stdout,
        Check::Stderr,
        Check::Refs,
        Check::Reflogs,
        Check::GitFiles,
    ];
    for args in [
        &["branch", "--create-reflog", "newb"][..],
        &[
            "-c",
            "core.logAllRefUpdates=false",
            "branch",
            "--create-reflog",
            "newb",
        ][..],
        &["-c", "core.logAllRefUpdates=false", "branch", "newb"][..],
    ] {
        fx.compare(args).checks(checks).run();
    }
    for args in [
        &["branch", "--edit-description"][..],
        &["branch", "--edit-description", "side"][..],
        &["branch", "--edit-description", "nope"][..],
        &["branch", "--edit-description", "a", "b"][..],
        &["-c", "core.commentChar=;", "branch", "--edit-description"][..],
    ] {
        fx.compare(args)
            .env("GIT_EDITOR", &editor)
            .checks(checks)
            .git_file("config")
            .git_file("EDIT_DESCRIPTION")
            .run();
    }
    fx.compare(["branch", "--edit-description", "side"])
        .env("GIT_EDITOR", &empty)
        .checks(checks)
        .git_file("config")
        .run();
    fx.compare(["branch", "--edit-description", "side"])
        .env("GIT_EDITOR", "false")
        .checks(checks)
        .git_file("config")
        .run();
}

/// In a repository configured as bare, the branch `HEAD` names is not
/// checked out and can be deleted.
#[test]
fn delete_head_branch_in_bare_repository() {
    let mut fx = fixture().git(["config", "--bool", "core.bare", "true"]);
    if fx.is_skipped() {
        return;
    }
    fx.compare(["branch", "-D", "main"])
        .checks([Check::ExitCode, Check::Stdout, Check::Stderr, Check::Refs])
        .run();
}
