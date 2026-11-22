// SPDX-License-Identifier: MIT
//! Differential tests of submodules in the core commands (#114): `status`
//! and `diff` in a superproject with clean, dirty, untracked and advanced
//! submodules, and `mv`/`rm` of submodules.

use rgit_difftest::{Check, Fixture};

/// A superproject `super` with the submodules `clean`, `dirty` (modified
/// file), `untr` (untracked file), `adv` (new commits) and `both` (all
/// three), plus an untracked nested repository.
const SUBS: [&str; 5] = ["clean", "dirty", "untr", "adv", "both"];

fn superproject() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "up"])
        .write("up/a", "a\n")
        .write("up/b", "b\n")
        .git(["-C", "up", "add", "."])
        .git(["-C", "up", "commit", "-qm", "one"])
        .write("up/a", "a2\n")
        .git(["-C", "up", "commit", "-qam", "two"])
        .write("up/a", "a3\n")
        .git(["-C", "up", "commit", "-qam", "three"])
        .git(["init", "-q", "-b", "main", "super"])
        .write("super/top", "top\n")
        .git(["-C", "super", "add", "top"])
        .git(["-C", "super", "commit", "-qm", "init"]);
    // Without `git submodule add` (the pinned reference git of CI has no
    // scripted commands): clone, describe in .gitmodules, add.
    let mut fx = fx;
    let mut gitmodules = String::new();
    for s in SUBS {
        fx = fx.git(["clone", "-q", "up", &format!("super/{s}")]);
        gitmodules.push_str(&format!(
            "[submodule \"{s}\"]\n\tpath = {s}\n\turl = ../up\n"
        ));
    }
    let mut add = vec![
        "-C",
        "super",
        "-c",
        "advice.addEmbeddedRepo=false",
        "add",
        ".gitmodules",
    ];
    add.extend(SUBS);
    let fx = fx.write("super/.gitmodules", gitmodules).git(add);
    fx.git(["-C", "super/adv", "checkout", "-q", "HEAD~1"])
        .git(["-C", "super/both", "checkout", "-q", "HEAD~2"])
        .git(["-C", "super", "add", "adv", "both"])
        .git(["-C", "super", "commit", "-qm", "subs"])
        .write("super/dirty/a", "a3\nx\n")
        .write("super/untr/new", "u\n")
        .git(["-C", "super/adv", "checkout", "-q", "main"])
        .git(["-C", "super/both", "checkout", "-q", "main"])
        .write("super/both/b", "b\ny\n")
        .write("super/both/zz", "z\n")
        .git(["init", "-q", "super/nested"])
}

fn check(fx: &mut Fixture, args: &[&str]) {
    fx.compare(args).in_dir("super").checks(Check::OUTPUT).run();
}

#[test]
fn status_formats() {
    let mut fx = superproject();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["status"][..],
        &["status", "-s"],
        &["status", "--porcelain"],
        &["status", "--porcelain=v2"],
        &["status", "-uno"],
        &["status", "-s", "-uno"],
        &["status", "-s", "--ignore-submodules"],
        &["status", "-s", "--ignore-submodules=dirty"],
        &["status", "-s", "--ignore-submodules=untracked"],
        &["status", "--porcelain=2", "--ignore-submodules=none"],
        &["-c", "diff.ignoreSubmodules=all", "status", "-s"],
        &[
            "-c",
            "diff.ignoreSubmodules=all",
            "-c",
            "submodule.dirty.ignore=none",
            "status",
            "-s",
        ],
        &["-c", "submodule.untr.ignore=untracked", "status", "-s"],
        &["status", "--ignore-submodules=bogus"],
    ] {
        check(&mut fx, args);
    }
}

#[test]
fn diff_formats() {
    let mut fx = superproject();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["diff"][..],
        &["diff", "--stat"],
        &["diff", "--numstat"],
        &["diff", "--raw"],
        &["diff", "--name-status"],
        &["diff", "HEAD"],
        &["diff", "HEAD~1", "HEAD"],
        &["diff", "--ignore-submodules=none", "--raw"],
        &["diff", "--ignore-submodules=dirty"],
        &["diff", "--ignore-submodules"],
        &["diff", "--submodule=short"],
        &["diff", "--submodule=log"],
        &["diff", "--submodule"],
        &["diff", "--submodule=diff"],
        &["-c", "diff.submodule=log", "diff"],
        &["-c", "diff.submodule=log", "diff-index", "-p", "HEAD"],
        &["diff-files"],
        &["diff-files", "--ignore-submodules=none"],
        &["-c", "submodule.dirty.ignore=all", "diff-files"],
        &["diff-index", "HEAD"],
        &["diff-index", "-p", "--submodule=log", "HEAD"],
        &["diff-tree", "-p", "--submodule=log", "HEAD~1", "HEAD"],
    ] {
        check(&mut fx, args);
    }
}

#[test]
fn mv_and_rm() {
    let mut fx = superproject().write("super/deep/.keep", "");
    if fx.is_skipped() {
        return;
    }
    fx.compare(["mv", "clean", "deep/clean2"])
        .in_dir("super")
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["rm", "dirty"])
        .in_dir("super")
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["rm", "untr"])
        .in_dir("super")
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["rm", "--cached", "adv"])
        .in_dir("super")
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["rm", "-f", "both"])
        .in_dir("super")
        .checks(Check::OUTPUT)
        .run();
}
