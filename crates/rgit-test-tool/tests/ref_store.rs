// SPDX-License-Identifier: MIT

//! `ref-store` against the reference `test-tool`: output, exit code, the
//! reference files and the reflogs after each call. The repositories are
//! made with the `git` of the reference build (or `git` on `PATH`), since
//! the fixture's own "git" is the reference test-tool.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::Fixture;
use rgit_difftest::Check;

/// Files below `.git` compared after every call.
const GIT_FILES: &[&str] = &[
    "HEAD",
    "FOO",
    "packed-refs",
    "refs/heads/main",
    "refs/heads/new-main",
    "refs/heads/foo",
    "refs/heads/bar",
    "refs/heads/sym",
    "refs/heads/dangling",
    "refs/tags/new-tag",
    "refs/tags/t1",
];

/// The `git` that sets up repositories.
fn setup_git() -> PathBuf {
    common::reference_test_tool()
        .and_then(|tt| Some(tt.parent()?.parent()?.parent()?.join("git")))
        .filter(|g| g.is_file())
        .unwrap_or_else(|| PathBuf::from("git"))
}

/// Runs `git args` in `dir` with a fixed identity and date.
fn git(fx: &Fixture, dir: &Path, args: &[&str]) -> String {
    let out = Command::new(setup_git())
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", fx.home_dir())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1112911993 -0700")
        .env("GIT_COMMITTER_DATE", "1112911993 -0700")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("utf-8")
        .trim()
        .to_string()
}

/// A repository with two commits on `main` and an annotated tag `t1`.
fn repo() -> Option<Fixture> {
    let fx = common::fixture()?;
    let dir = fx.work_dir();
    git(&fx, &dir, &["init", "-q", "-b", "main", "."]);
    git(&fx, &dir, &["commit", "-q", "--allow-empty", "-m", "one"]);
    git(&fx, &dir, &["tag", "-a", "-m", "t1", "t1"]);
    git(&fx, &dir, &["commit", "-q", "--allow-empty", "-m", "two"]);
    Some(fx)
}

/// Compares one call: output, exit code, reference files and reflogs.
fn same(fx: &mut Fixture, args: &[&str]) {
    let mut cmp = fx
        .compare(args)
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::Reflogs,
            Check::GitFiles,
        ])
        .normalize_stderr(rgit_difftest::Normalizer::regex(
            r"^BUG: [a-z/]+\.c:\d+: ",
            "BUG: ",
        ));
    for f in GIT_FILES {
        cmp = cmp.git_file(*f);
    }
    if let Err(m) = cmp.try_run() {
        panic!("ref-store {args:?}:\n{m}");
    }
}

fn rs(fx: &mut Fixture, args: &[&str]) {
    let mut all = vec!["ref-store", "main"];
    all.extend_from_slice(args);
    same(fx, &all);
}

/// The calls of t1405, in its order.
#[test]
fn main_store_sequence() {
    let Some(mut fx) = repo() else { return };
    let dir = fx.work_dir();
    rs(
        &mut fx,
        &["create-symref", "FOO", "refs/heads/main", "nothing"],
    );
    git(
        &fx,
        &dir,
        &["tag", "-a", "-m", "new-tag", "new-tag", "HEAD"],
    );
    rs(
        &mut fx,
        &[
            "delete-refs",
            "REF_NO_DEREF",
            "nothing",
            "FOO",
            "refs/tags/new-tag",
        ],
    );
    rs(
        &mut fx,
        &["rename-ref", "refs/heads/main", "refs/heads/new-main"],
    );
    git(
        &fx,
        &dir,
        &["commit", "-q", "--allow-empty", "-m", "recreate"],
    );
    rs(&mut fx, &["for-each-ref", "refs/heads/"]);
    rs(&mut fx, &["for-each-ref", "refs/"]);
    rs(&mut fx, &["for-each-ref", ""]);
    rs(&mut fx, &["resolve-ref", "refs/heads/new-main", "0"]);
    rs(&mut fx, &["resolve-ref", "HEAD", "0"]);
    rs(&mut fx, &["verify-ref", "refs/heads/new-main"]);
    rs(&mut fx, &["for-each-reflog"]);
    rs(&mut fx, &["for-each-reflog-ent", "HEAD"]);
    rs(&mut fx, &["for-each-reflog-ent-reverse", "HEAD"]);
    rs(&mut fx, &["reflog-exists", "HEAD"]);
    rs(&mut fx, &["delete-reflog", "HEAD"]);
    rs(&mut fx, &["reflog-exists", "HEAD"]);
    rs(&mut fx, &["create-reflog", "HEAD"]);
    rs(&mut fx, &["reflog-exists", "HEAD"]);
    git(&fx, &dir, &["checkout", "-q", "-b", "foo"]);
    let foo = git(&fx, &dir, &["rev-parse", "foo"]);
    git(&fx, &dir, &["checkout", "-q", "--detach"]);
    git(&fx, &dir, &["commit", "-q", "--allow-empty", "-m", "bar"]);
    git(&fx, &dir, &["checkout", "-q", "-b", "bar"]);
    let bar = git(&fx, &dir, &["rev-parse", "bar"]);
    rs(
        &mut fx,
        &["update-ref", "updating", "refs/heads/foo", &bar, &foo, "0"],
    );
    git(&fx, &dir, &["checkout", "-q", "--detach"]);
    rs(&mut fx, &["delete-ref", "msg", "refs/heads/foo", &bar, "0"]);
}

#[test]
fn more_calls() {
    let Some(mut fx) = repo() else { return };
    let dir = fx.work_dir();
    let head = git(&fx, &dir, &["rev-parse", "HEAD"]);
    let first = git(&fx, &dir, &["rev-parse", "HEAD~1"]);
    rs(
        &mut fx,
        &["create-symref", "refs/heads/sym", "refs/heads/main", "m"],
    );
    rs(
        &mut fx,
        &["create-symref", "refs/heads/dangling", "refs/heads/no"],
    );
    rs(&mut fx, &["for-each-ref", "refs/heads/"]);
    rs(&mut fx, &["resolve-ref", "refs/heads/sym", "0"]);
    rs(&mut fx, &["resolve-ref", "refs/heads/dangling", "0"]);
    rs(&mut fx, &["resolve-ref", "refs/heads/nope", "0"]);
    rs(&mut fx, &["resolve-ref", "refs/heads/a..b", "0"]);
    rs(&mut fx, &["resolve-ref", "", "0"]);
    git(&fx, &dir, &["pack-refs", "--all"]);
    rs(&mut fx, &["for-each-ref", "refs/"]);
    rs(&mut fx, &["resolve-ref", "refs/heads/main", "0"]);
    rs(&mut fx, &["resolve-ref", "refs/heads/sym", "0"]);
    rs(
        &mut fx,
        &[
            "rename-ref",
            "refs/heads/main",
            "refs/heads/new-main",
            "moved",
        ],
    );
    rs(&mut fx, &["rename-ref", "refs/heads/nope", "refs/heads/x"]);
    rs(&mut fx, &["for-each-reflog"]);
    rs(&mut fx, &["for-each-reflog-ent", "refs/heads/new-main"]);
    rs(&mut fx, &["for-each-reflog-ent", "refs/heads/nope"]);
    rs(&mut fx, &["create-reflog", "refs/tags/t1"]);
    rs(&mut fx, &["delete-reflog", "refs/tags/t1"]);
    rs(&mut fx, &["delete-reflog", "refs/tags/none"]);
    rs(
        &mut fx,
        &["update-ref", "up", "refs/heads/foo", &head, &first, "0"],
    );
    rs(
        &mut fx,
        &[
            "update-ref",
            "",
            "refs/tags/new-tag",
            &head,
            &"0".repeat(40),
            "REF_FORCE_CREATE_REFLOG",
        ],
    );
    rs(
        &mut fx,
        &["update-ref", "up", "refs/heads/foo", "zz", &first, "0"],
    );
    rs(
        &mut fx,
        &["update-ref", "up", "refs/heads/foo", &head, "zz", "0"],
    );
    rs(
        &mut fx,
        &["delete-ref", "m", "refs/heads/foo", &"1".repeat(40), "0"],
    );
    rs(&mut fx, &["delete-refs", "0", "m", "refs/heads/nope"]);
    rs(
        &mut fx,
        &["create-symref", "HEAD", "refs/heads/invalid.lock"],
    );
}

#[test]
fn usage_errors() {
    let Some(mut fx) = repo() else { return };
    same(&mut fx, &["ref-store"]);
    same(&mut fx, &["ref-store", "main"]);
    same(&mut fx, &["ref-store", "main", "bogus"]);
    same(&mut fx, &["ref-store", "xx", "bogus"]);
    same(&mut fx, &["ref-store", "worktree:nope", "for-each-reflog"]);
    for f in [
        "create-symref",
        "rename-ref",
        "for-each-ref",
        "resolve-ref",
        "verify-ref",
        "for-each-reflog-ent",
        "reflog-exists",
        "create-reflog",
        "delete-reflog",
        "update-ref",
        "delete-ref",
        "delete-refs",
    ] {
        rs(&mut fx, &[f]);
    }
    rs(&mut fx, &["create-symref", "X"]);
    rs(&mut fx, &["rename-ref", "refs/heads/main"]);
    rs(&mut fx, &["resolve-ref", "HEAD"]);
    rs(&mut fx, &["resolve-ref", "HEAD", "1"]);
    rs(&mut fx, &["resolve-ref", "HEAD", "REF_NO_DEREF"]);
    rs(&mut fx, &["update-ref", "m", "refs/heads/x"]);
    rs(&mut fx, &["update-ref", "m", "refs/heads/x", "a"]);
    rs(&mut fx, &["update-ref", "m", "refs/heads/x", "a", "b"]);
    rs(&mut fx, &["delete-ref", "m", "refs/heads/x"]);
    rs(&mut fx, &["delete-ref", "m", "refs/heads/x", "a"]);
    rs(&mut fx, &["delete-refs", "x", "m", "refs/heads/x"]);
    rs(
        &mut fx,
        &["delete-refs", "REF_NO_DEREF,", "m", "refs/heads/x"],
    );
    rs(&mut fx, &["for-each-ref", "refs/heads"]);
}

/// The worktree stores of t0600.
#[test]
fn worktree_stores() {
    let Some(mut fx) = common::fixture() else {
        return;
    };
    let dir = fx.work_dir();
    git(&fx, &dir, &["init", "-q", "-b", "main", "."]);
    git(&fx, &dir, &["commit", "-q", "--allow-empty", "-m", "first"]);
    git(&fx, &dir, &["worktree", "add", "-q", "-b", "wt-main", "wt"]);
    git(
        &fx,
        &dir.join("wt"),
        &["commit", "-q", "--allow-empty", "-m", "second"],
    );
    let zero = format!("{}\n", "0".repeat(40));
    for f in [
        ".git/logs/PSEUDO_MAIN_HEAD",
        ".git/logs/refs/bisect/random",
        ".git/worktrees/wt/logs/PSEUDO_WT_HEAD",
        ".git/worktrees/wt/logs/refs/bisect/wt-random",
    ] {
        fx = fx.write(f, &zero);
    }
    for store in ["worktree:wt", "worktree:main", "main"] {
        same(&mut fx, &["ref-store", store, "for-each-reflog"]);
        same(
            &mut fx,
            &["ref-store", store, "for-each-reflog-ent", "HEAD"],
        );
        same(&mut fx, &["ref-store", store, "resolve-ref", "HEAD", "0"]);
        same(
            &mut fx,
            &["ref-store", store, "for-each-ref", "refs/heads/"],
        );
        same(
            &mut fx,
            &["ref-store", store, "reflog-exists", "refs/bisect/wt-random"],
        );
    }
    same(
        &mut fx,
        &["-C", "wt", "ref-store", "main", "for-each-reflog"],
    );
    same(
        &mut fx,
        &["-C", "wt", "ref-store", "main", "resolve-ref", "HEAD", "0"],
    );
}
