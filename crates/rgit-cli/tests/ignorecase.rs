// SPDX-License-Identifier: MIT
//! Differential tests of `core.ignoreCase` (#183) against the reference
//! git: index lookups that ignore case in `add`, `status`, `ls-files -o`
//! and `clean`, renames that change only the case, and switching between
//! trees whose paths differ only in case.
//!
//! git honours `core.ignoreCase=true` on every file system, so most cases
//! run with it on a case-sensitive file system too, where two spellings of
//! a name are two files. Those cases skip themselves where the file system
//! folds case; the others compare what git does on the platform at hand.

use rgit_difftest::{Check, Fixture};

const STATE: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Index,
    Check::Worktree,
];

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .git(["config", "core.ignorecase", "true"])
}

/// Whether `a` and `A` are two files in the fixture's work directory.
fn case_sensitive(fx: &Fixture) -> bool {
    let dir = fx.work_dir();
    let probe = dir.join("CaseProbe");
    std::fs::write(&probe, "").expect("write probe");
    let folded = dir.join("caseprobe").exists();
    std::fs::remove_file(&probe).expect("remove probe");
    !folded
}

/// `Dir/a` and `file` are tracked; `dir/b`, `dir/A`, `FILE` and `new` exist
/// next to them under other spellings.
fn two_spellings() -> Option<Fixture> {
    let fx = fixture();
    if fx.is_skipped() || !case_sensitive(&fx) {
        return None;
    }
    Some(
        fx.write("Dir/a", "a\n")
            .write("file", "file\n")
            .git(["add", "."])
            .git(["commit", "-q", "-m", "one"])
            .tick()
            .write("dir/b", "b\n")
            .write("dir/A", "other a\n")
            .write("FILE", "other file\n")
            .write("new", "new\n"),
    )
}

#[test]
fn add_uses_the_spelling_of_the_index() {
    let cases: &[&[&str]] = &[
        &["add", "dir/b"],
        &["add", "-v", "dir"],
        &["add", "FILE"],
        &["add", "dir/A"],
        &["add", "."],
        &["add", "-A"],
        &["add", "-n", "."],
        &["add", "-u", "FILE"],
        &["add", "dir/b", "new", "Dir"],
    ];
    for args in cases {
        let Some(mut fx) = two_spellings() else {
            return;
        };
        fx.compare(*args).checks(STATE).run();
    }
}

#[test]
fn new_directories_keep_one_spelling() {
    let Some(fx) = two_spellings() else { return };
    let mut fx = fx
        .write("Sub/Deep/x", "x\n")
        .git(["add", "Sub"])
        .write("sub/deep/y", "y\n")
        .write("sub/other/z", "z\n")
        .write("dir/more/w", "w\n");
    fx.compare(["add", "sub", "dir/more"]).checks(STATE).run();
}

#[test]
fn untracked_files_are_looked_up_without_case() {
    let cases: &[&[&str]] = &[
        &["status", "--porcelain"],
        &["status", "--porcelain", "-uall"],
        &["status", "--porcelain", "-uall", "--ignored"],
        &["status", "--porcelain", "dir"],
        &["status"],
        &["ls-files", "-o"],
        &["ls-files", "-o", "--directory"],
        &["ls-files", "-o", "dir"],
        &["ls-files", "FILE"],
        &["ls-files", "--error-unmatch", "FILE"],
        &["clean", "-n"],
        &["clean", "-n", "-d"],
        &["clean", "-f", "-d"],
    ];
    for args in cases {
        let Some(mut fx) = two_spellings() else {
            return;
        };
        fx.compare(*args).checks(STATE).run();
    }
}

#[test]
fn without_the_setting_spellings_are_distinct() {
    for args in [
        &["add", "."][..],
        &["status", "--porcelain", "-uall"],
        &["ls-files", "-o"],
    ] {
        let Some(fx) = two_spellings() else { return };
        let mut fx = fx.git(["config", "core.ignorecase", "false"]);
        fx.compare(args).checks(STATE).run();
    }
}

#[test]
fn mv_changes_only_the_case() {
    let cases: &[&[&str]] = &[
        &["mv", "camelcase", "CamelCase"],
        &["mv", "-v", "camelcase", "CAMELCASE"],
        &["mv", "-n", "camelcase", "CamelCase"],
        &["mv", "camelcase", "other"],
        &["mv", "-f", "camelcase", "OTHER"],
        &["mv", "d/f", "d/F"],
    ];
    for args in cases {
        let fx = fixture();
        if fx.is_skipped() {
            return;
        }
        let both = case_sensitive(&fx);
        let mut fx = fx
            .write("camelcase", "c\n")
            .write("other", "o\n")
            .write("d/f", "f\n")
            .git(["add", "."])
            .git(["commit", "-q", "-m", "one"])
            .tick();
        if both {
            // The destination exists as a file of its own.
            fx = fx.write("CamelCase", "second\n").write("d/F", "second\n");
        }
        fx.compare(*args).checks(STATE).run();
    }
}

/// `main` has `camelcase` and `Gitweb`; `topic` renamed the first to
/// `CamelCase`, `tree` has `gitweb/subdir/file` instead of the file `Gitweb`.
fn renamed() -> Fixture {
    fixture()
        .git(["symbolic-ref", "HEAD", "refs/heads/main"])
        .write("camelcase", "c\n")
        .write("Gitweb", "g\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "initial"])
        .git(["tag", "initial"])
        .tick()
        .git(["checkout", "-q", "-b", "topic"])
        .git(["mv", "camelcase", "tmp"])
        .git(["mv", "tmp", "CamelCase"])
        .git(["commit", "-q", "-m", "rename"])
        .tick()
        .git(["checkout", "-q", "-b", "tree", "initial"])
        .git(["rm", "-q", "Gitweb"])
        .write("gitweb/subdir/file", "f\n")
        .git(["add", "gitweb"])
        .git(["commit", "-q", "-m", "directory"])
        .tick()
        .git(["checkout", "-q", "-f", "main"])
}

#[test]
fn switching_between_spellings() {
    let cases: &[&[&[&str]]] = &[
        &[&["checkout", "topic"], &["checkout", "main"]],
        &[&["merge", "topic"]],
        &[&["checkout", "tree"], &["checkout", "main"]],
        &[&["checkout", "tree"], &["checkout", "topic"]],
        &[&["reset", "--hard", "topic"], &["status", "--porcelain"]],
        &[&["read-tree", "-m", "-u", "main", "topic"]],
        &[&["cherry-pick", "topic"]],
    ];
    for steps in cases {
        let mut fx = renamed();
        if fx.is_skipped() {
            return;
        }
        for args in *steps {
            fx.compare(*args).checks(STATE).run();
        }
    }
}

#[test]
fn files_in_the_way_under_another_spelling() {
    // Untracked files whose names differ from tracked or incoming paths
    // only in case; on a file system that folds case they are those paths.
    let cases: &[&[&str]] = &[
        &["checkout", "topic"],
        &["merge", "topic"],
        &["checkout", "tree"],
        &["reset", "--hard", "tree"],
        &["read-tree", "-m", "-u", "main", "tree"],
    ];
    for args in cases {
        let fx = renamed();
        if fx.is_skipped() || !case_sensitive(&fx) {
            return;
        }
        let mut fx = fx
            .write("CamelCase", "in the way\n")
            .write("GITWEB", "in the way\n")
            .write("Unrelated", "u\n");
        fx.compare(*args).checks(STATE).run();
    }
}

#[test]
fn clone_reports_colliding_paths() {
    // Where the file system folds case, `A`/`a` and `DIR/X`/`dir/x` end up
    // in one file each and `clone` lists them; elsewhere nothing collides.
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "src"])
        .git(["-C", "src", "config", "core.ignorecase", "false"])
        .write("src/a", "a\n")
        .write("src/zzz", "z\n")
        .git(["-C", "src", "add", "a", "zzz"]);
    if fx.is_skipped() {
        return;
    }
    let blob = String::from_utf8(fx.git_stdout(["-C", "src", "rev-parse", ":a"])).unwrap();
    let blob = blob.trim();
    let mut fx = fx;
    for path in ["A", "dir/x", "DIR/X", "Dir/y"] {
        let info = format!("100644,{blob},{path}");
        fx = fx.git(["-C", "src", "update-index", "--add", "--cacheinfo", &info]);
    }
    let mut fx = fx.git(["-C", "src", "commit", "-q", "-m", "colliding paths"]);
    fx.compare(["clone", "src", "c"])
        .repo("c")
        .checks([Check::ExitCode, Check::Stderr, Check::Worktree])
        .run();
    fx.compare(["-C", "c", "ls-files", "-s"]).run();
}
