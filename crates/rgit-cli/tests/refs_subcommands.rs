// SPDX-License-Identifier: MIT
//! `git refs` (list, exists, optimize, create, update, delete, rename) and
//! `pack-refs --auto` against the reference git: output, references and
//! reflogs.

use rgit_difftest::{Check, Fixture};

fn id(fx: &Fixture, rev: &str) -> String {
    String::from_utf8(fx.git_stdout(["rev-parse", rev]))
        .unwrap()
        .trim()
        .to_string()
}

#[test]
fn refs_subcommands() {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .git(["commit", "-q", "--allow-empty", "-m", "c1"])
        .git(["tag", "t1"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "c2"])
        .git(["tag", "-a", "-m", "annotated", "t2"])
        .git(["branch", "side", "HEAD~1"])
        .git(["symbolic-ref", "refs/heads/symref", "refs/heads/target"]);
    if fx.is_skipped() {
        return;
    }
    let a = id(&fx, "t1");
    let b = id(&fx, "HEAD");
    let zero = "0".repeat(a.len());
    let mut fx = fx;
    let cases: Vec<Vec<&str>> = vec![
        vec!["refs", "-h"],
        vec!["refs"],
        vec!["refs", "bogus"],
        vec!["refs", "--bogus"],
        vec!["refs", "list"],
        vec![
            "refs",
            "list",
            "--format=%(refname) %(objecttype)",
            "refs/tags",
        ],
        vec!["refs", "list", "--sort=bogus", "-h"],
        vec!["refs", "exists", "refs/heads/main"],
        vec!["refs", "exists", "main"],
        vec!["refs", "exists", "refs/heads/symref"],
        vec!["refs", "exists"],
        vec!["refs", "exists", "a", "b"],
        vec!["refs", "optimize", "--all"],
        vec!["refs", "optimize", "-h"],
        vec!["refs", "create"],
        vec!["refs", "create", "-h"],
        vec!["refs", "create", "refs/heads/foo", &a],
        vec!["refs", "create", "refs/heads/foo", "t1"],
        vec!["refs", "create", "refs/heads/main", &a],
        vec!["refs", "create", "refs/heads/foo", &zero],
        vec!["refs", "create", "refs/heads/foo", "invalid-oid"],
        vec!["refs", "create", "--create-reflog", "refs/foo", &a],
        vec!["refs", "create", "--message=why", "refs/heads/foo", &a],
        vec!["refs", "create", "--message=", "refs/heads/foo", &a],
        vec!["refs", "create", "refs/heads/symref", &a],
        vec!["refs", "create", "--no-deref", "refs/heads/symref", &a],
        vec!["refs", "update", "refs/heads/main", &a],
        vec!["refs", "update", "refs/heads/main", &a, &b],
        vec!["refs", "update", "refs/heads/main", &a, &a],
        vec!["refs", "update", "refs/heads/new", &a, &zero],
        vec!["refs", "update", "refs/heads/main", &a, &zero],
        vec!["refs", "update", "refs/heads/side", &zero],
        vec!["refs", "update", "refs/heads/nope", &a, &b],
        vec!["refs", "update", "refs/heads/main", "nope"],
        vec!["refs", "update", "refs/heads/main"],
        vec!["refs", "delete", "refs/heads/side"],
        vec!["refs", "delete", "refs/heads/side", &a],
        vec!["refs", "delete", "refs/heads/side", &b],
        vec!["refs", "delete", "refs/heads/side", &zero],
        vec!["refs", "delete", "refs/heads/nope"],
        vec!["refs", "delete", "--no-deref", "refs/heads/symref"],
        vec!["refs", "delete"],
        vec!["refs", "rename", "refs/heads/side", "refs/heads/other"],
        vec![
            "refs",
            "rename",
            "--message=m",
            "refs/heads/main",
            "refs/heads/x/y",
        ],
        vec!["refs", "rename", "refs/heads/side", "refs/heads/main"],
        vec!["refs", "rename", "refs/heads/nope", "refs/heads/other"],
        vec!["refs", "rename", "refs/heads/side", "bad..name"],
        vec!["refs", "rename", "refs/heads/side", "HEAD"],
        vec!["refs", "rename", "refs/tags/t1", "refs/tags/t3"],
        vec!["refs", "rename", "refs/heads/side"],
    ];
    for args in &cases {
        fx.compare(args)
            .checks([
                Check::ExitCode,
                Check::Stdout,
                Check::Stderr,
                Check::Refs,
                Check::Reflogs,
            ])
            .run();
    }
}

/// `pack-refs --auto` packs only with enough loose references for the
/// size of `packed-refs`; per-worktree references stay loose and an
/// inconsistent tag gets no peeled value.
#[test]
fn pack_refs_auto_and_exclusions() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .git(["commit", "-q", "--allow-empty", "-m", "c1"])
        .git(["update-ref", "refs/bisect/local", "HEAD"])
        .git(["update-ref", "refs/worktree/foo", "HEAD"]);
    if fx.is_skipped() {
        return;
    }
    for n in 0..14 {
        let name = format!("refs/heads/loose-{n}");
        fx = fx.git(["update-ref", name.as_str(), "HEAD"]);
    }
    let checks = [
        Check::ExitCode,
        Check::Stdout,
        Check::Stderr,
        Check::Refs,
        Check::GitFiles,
    ];
    fx.compare(["pack-refs", "--auto", "--all"])
        .checks(checks)
        .git_file("packed-refs")
        .run();
    fx.compare(["pack-refs", "--all"])
        .checks(checks)
        .git_file("packed-refs")
        .run();
    let mut fx = fx.git(["update-ref", "refs/heads/loose-14", "HEAD"]);
    fx.compare(["pack-refs", "--auto", "--all"])
        .checks(checks)
        .git_file("packed-refs")
        .run();
    let mut fx = fx.git(["pack-refs", "--all"]);
    for n in 0..20 {
        let name = format!("refs/heads/more-{n}");
        fx = fx.git(["update-ref", name.as_str(), "HEAD"]);
    }
    fx.compare(["refs", "optimize", "--auto", "--all"])
        .checks(checks)
        .git_file("packed-refs")
        .run();
}
