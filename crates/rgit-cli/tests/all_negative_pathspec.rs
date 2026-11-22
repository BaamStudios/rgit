// SPDX-License-Identifier: MIT
//! Exclude-only pathspecs given in a subdirectory: commands that work on
//! the whole tree without a pathspec (add, commit, stash, the diff family,
//! status) subtract the excludes from the whole tree; the others (ls-files,
//! clean, rm, grep) from the current directory. Differential against git.

use rgit_difftest::{Check, Fixture};

const PATHS: [&str; 4] = ["top", "sub/f", "sub/sub/f", "sub2/f"];

fn fixture() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"]);
    for p in PATHS {
        fx = fx.write(p, "base\n");
    }
    fx = fx
        .git(["add", "."])
        .git(["commit", "-q", "-m", "base"])
        .tick();
    for p in PATHS {
        fx = fx.write(p, "base\nchanged\n");
    }
    fx
}

#[test]
fn exclude_only_pathspec_in_subdirectory() {
    let cases: &[&[&str]] = &[
        &["add", "--", ":!sub/"],
        &["add", "--", ":(exclude)sub"],
        &["commit", "-q", "-m", "c", "--", ":!sub/"],
        &["stash", "push", "-q", "-m", "s", "--", ":!sub/"],
        &["stash", "push", "-q", "-m", "s", "--", ":(exclude)sub/"],
        &["diff", "--name-only", "--", ":!sub/"],
        &["diff-files", "--name-only", "--", ":^sub/"],
        &["diff-index", "--name-only", "HEAD", "--", ":!sub/"],
        &["status", "--porcelain", "--", ":!sub/"],
        &["ls-files", "--", ":!sub/"],
        &["rm", "-q", "-r", "--cached", "--", ":!sub/"],
    ];
    for args in cases {
        let mut fx = fixture();
        fx.compare(*args)
            .in_dir("sub")
            .checks([
                Check::ExitCode,
                Check::Stdout,
                Check::Stderr,
                Check::Index,
                Check::Worktree,
            ])
            .run();
    }
}

#[test]
fn pathspec_from_file_with_edit() {
    let mut fx = fixture().write("list", "top\n");
    fx.compare(["add", "--pathspec-from-file=list", "--edit"])
        .checks(Check::OUTPUT)
        .run();
}
