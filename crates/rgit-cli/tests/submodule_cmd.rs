// SPDX-License-Identifier: MIT
//! Differential tests of `rgit submodule` (and `clone --recurse-submodules`)
//! against the reference git. Every case runs in a fresh copy of a
//! temporary fixture (rgit-difftest) with its own `$HOME`; nothing outside
//! it is touched.

use rgit_difftest::{Check, Fixture};

fn checks() -> impl Iterator<Item = Check> {
    Check::OUTPUT
        .into_iter()
        .chain([Check::Worktree, Check::Index, Check::Refs])
}

/// `up` (two commits, a lightweight and an annotated tag), `inner` (one
/// commit) and `mid` (a commit with `inner` as submodule `deep/inner`).
fn upstreams() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["config", "--global", "protocol.file.allow", "always"])
        .git(["init", "-q", "up"])
        .write("up/a", "a\n")
        .git(["-C", "up", "add", "a"])
        .git(["-C", "up", "commit", "-q", "-m", "first"])
        .git(["-C", "up", "tag", "-a", "-m", "v1", "v1"])
        .tick()
        .write("up/b", "b\n")
        .git(["-C", "up", "add", "b"])
        .git(["-C", "up", "commit", "-q", "-m", "second"])
        .git(["-C", "up", "tag", "light"])
        .git(["init", "-q", "inner"])
        .write("inner/i", "i\n")
        .git(["-C", "inner", "add", "i"])
        .git(["-C", "inner", "commit", "-q", "-m", "inner"])
        .git(["init", "-q", "mid"])
        .write("mid/m", "m\n")
        .git(["-C", "mid", "add", "m"])
        .git(["-C", "mid", "commit", "-q", "-m", "mid"])
        .git([
            "-C",
            "mid",
            "submodule",
            "add",
            "-q",
            "../inner",
            "deep/inner",
        ])
        .git(["-C", "mid", "commit", "-q", "-m", "add inner"])
}

/// `super` with the submodules `sub` (from `up`) and `lib/mid` (from
/// `mid`, which has a submodule of its own), committed.
fn fixture() -> Fixture {
    upstreams()
        .git(["init", "-q", "super"])
        .write("super/f", "f\n")
        .git(["-C", "super", "add", "f"])
        .git(["-C", "super", "commit", "-q", "-m", "init"])
        .git(["-C", "super", "submodule", "add", "-q", "../up", "sub"])
        .git(["-C", "super", "submodule", "add", "-q", "../mid", "lib/mid"])
        .git(["-C", "super", "commit", "-q", "-m", "submodules"])
}

/// A clone of `super` without its submodules.
fn cloned() -> Fixture {
    fixture().git(["clone", "-q", "super", "clone"])
}

fn compare(mut fx: Fixture, dir: &str, args: &[&str]) {
    let mut argv = vec!["submodule"];
    argv.extend_from_slice(args);
    fx.compare(&argv).in_dir(dir).checks(checks()).run();
}

#[test]
fn usage() {
    for args in [
        &["-h"][..],
        &["--recursive"],
        &["--"],
        &["no-such"],
        &["add"],
    ] {
        compare(fixture(), "super", args);
    }
}

#[test]
fn status_forms() {
    for args in [
        &[][..],
        &["status"],
        &["--cached"],
        &["status", "--recursive"],
        &["status", "sub"],
        &["status", "no-such"],
        &["--quiet", "status"],
    ] {
        compare(fixture(), "super", args);
    }
    compare(fixture(), "super/lib", &["status"]);
    // A submodule at another commit, an uninitialised one.
    let fx = fixture().git(["-C", "super/sub", "checkout", "-q", "HEAD^"]);
    compare(fx, "super", &["status"]);
    let fx = fixture().git(["-C", "super/sub", "checkout", "-q", "HEAD^"]);
    compare(fx, "super", &["status", "--cached"]);
    compare(cloned(), "clone", &["status", "--recursive"]);
}

#[test]
fn init_and_update() {
    compare(cloned(), "clone", &["init"]);
    compare(cloned(), "clone", &["init", "sub"]);
    compare(cloned(), "clone", &["update"]);
    compare(cloned(), "clone", &["update", "sub"]);
    compare(cloned(), "clone", &["update", "--init"]);
    compare(cloned(), "clone", &["update", "--init", "--recursive"]);
    compare(
        cloned(),
        "clone/lib",
        &["update", "--init", "--recursive", "--", "mid"],
    );
    compare(cloned(), "clone", &["update", "--init", "--quiet"]);
    let fx = cloned().git(["-C", "clone", "config", "submodule.active", "sub"]);
    compare(fx, "clone", &["update"]);
    // Back to the recorded commit, with each strategy.
    for args in [
        &["update"][..],
        &["update", "--force"],
        &["update", "--merge"],
        &["update", "--rebase"],
        &["update", "--checkout"],
    ] {
        let fx = fixture().git(["-C", "super/sub", "checkout", "-q", "HEAD^"]);
        compare(fx, "super", args);
    }
    let fx = fixture().git(["-C", "super", "config", "submodule.sub.update", "none"]);
    compare(fx, "super", &["update"]);
    let fx = fixture()
        .git(["-C", "super/sub", "checkout", "-q", "HEAD^"])
        .git(["-C", "super", "config", "submodule.sub.update", "!echo run"]);
    compare(fx, "super", &["update", "sub"]);
    let fx = fixture().git(["-C", "super", "config", "submodule.sub.update", "bogus"]);
    compare(fx, "super", &["update", "sub"]);
}

#[test]
fn deinit() {
    for args in [
        &["deinit"][..],
        &["deinit", "sub"],
        &["deinit", "--all"],
        &["deinit", "-q", "."],
        &["deinit", "--all", "sub"],
    ] {
        compare(fixture(), "super", args);
    }
    let fx = fixture().write("super/sub/a", "changed\n");
    compare(fx, "super", &["deinit", "sub"]);
    let fx = fixture().write("super/sub/a", "changed\n");
    compare(fx, "super", &["deinit", "-f", "sub"]);
    compare(cloned(), "clone", &["deinit", "sub"]);
}

#[test]
fn foreach() {
    compare(
        fixture(),
        "super",
        &["foreach", "echo $name $sm_path $displaypath $sha1"],
    );
    compare(
        fixture(),
        "super/lib",
        &["foreach", "--recursive", "echo $displaypath $path"],
    );
    compare(fixture(), "super", &["foreach", "-q", "pwd"]);
    compare(fixture(), "super", &["foreach", "echo", "a b"]);
    compare(fixture(), "super", &["foreach", "false"]);
    compare(fixture(), "super", &["foreach", "--", "--not-an-option"]);
}

#[test]
fn sync_set_url_set_branch() {
    let fx = fixture().git(["-C", "super", "config", "submodule.sub.url", "elsewhere"]);
    compare(fx, "super", &["sync"]);
    compare(fixture(), "super", &["sync", "--recursive"]);
    let fx = fixture().git([
        "-C",
        "super",
        "config",
        "remote.origin.url",
        "/some/where/super",
    ]);
    compare(fx, "super", &["sync", "sub"]);
    compare(fixture(), "super", &["set-url", "sub", "../other"]);
    compare(fixture(), "super", &["set-url", "sub"]);
    compare(fixture(), "super", &["set-branch", "-b", "topic", "sub"]);
    compare(fixture(), "super", &["set-branch", "--default", "sub"]);
    compare(fixture(), "super", &["set-branch", "sub"]);
    compare(fixture(), "super", &["set-branch", "-b", "x", "-d", "sub"]);
    let fx = fixture().write("super/.gitmodules", "[submodule \"sub\"]\n\tpath = sub\n");
    compare(fx, "super", &["set-branch", "-b", "topic", "sub"]);
}

#[test]
fn add() {
    for args in [
        &["add", "../up"][..],
        &["add", "-q", "../up", "dir/./x//"],
        &["add", "-b", "main", "../up", "br"],
        &["add", "--name", "other", "../up", "named"],
        &["add", "../up", "sub"],
        &["add", "../up", "f"],
        &["add", "--force", "../up", "sub"],
    ] {
        compare(fixture(), "super", args);
    }
    compare(fixture(), "super/lib", &["add", "../../up", "x"]);
    // An existing repository at the path.
    let fx = fixture()
        .git(["init", "-q", "super/own"])
        .write("super/own/o", "o\n")
        .git(["-C", "super/own", "add", "o"])
        .git(["-C", "super/own", "commit", "-q", "-m", "own"]);
    compare(fx, "super", &["add", "./own"]);
    let fx = fixture().git(["init", "-q", "super/empty"]);
    compare(fx, "super", &["add", "../up", "empty"]);
    let fx = fixture().write("super/.gitignore", "ign\n");
    compare(fx, "super", &["add", "../up", "ign"]);
}

#[test]
fn summary() {
    let fx = fixture().git(["-C", "super/sub", "checkout", "-q", "HEAD^"]);
    compare(fx, "super", &["summary"]);
    let fx = fixture()
        .git(["-C", "super/sub", "checkout", "-q", "HEAD^"])
        .git(["-C", "super", "add", "sub"]);
    compare(fx, "super", &["summary", "--cached"]);
    let fx = fixture().git(["-C", "super/sub", "checkout", "-q", "HEAD^"]);
    compare(fx, "super", &["summary", "--files"]);
    compare(fixture(), "super", &["summary", "HEAD^"]);
    compare(fixture(), "super", &["summary", "-n", "1", "HEAD^"]);
    compare(fixture(), "super", &["summary", "--cached", "--files"]);
}

#[test]
fn absorbgitdirs() {
    let fx = fixture()
        .git(["init", "-q", "super/emb"])
        .write("super/emb/e", "e\n")
        .git(["-C", "super/emb", "add", "e"])
        .git(["-C", "super/emb", "commit", "-q", "-m", "e"])
        .git(["-C", "super", "submodule", "add", "-q", "./emb"]);
    compare(fx, "super", &["absorbgitdirs"]);
    compare(fixture(), "super", &["absorbgitdirs"]);
}

/// The issue's acceptance check: a recursive clone has every nested
/// submodule initialised at the recorded commit.
#[test]
fn clone_recurse_submodules() {
    for args in [
        &["clone", "--recurse-submodules", "super", "c"][..],
        &["clone", "-q", "--recursive", "super", "c"],
        &["clone", "--recurse-submodules=lib", "super", "c"],
        &[
            "clone",
            "--recurse-submodules=.",
            "--recurse-submodules=:(exclude)sub",
            "super",
            "c",
        ],
    ] {
        // The work trees of the clones (submodules included) must match.
        let mut fx = fixture();
        fx.compare(args).checks(checks()).run();
        fx.compare(["submodule", "status", "--recursive"])
            .in_dir("c")
            .checks(Check::OUTPUT)
            .run();
    }
}

#[test]
fn helper_get_default_remote() {
    let mut fx = fixture();
    fx.compare(["submodule--helper", "get-default-remote", "sub"])
        .in_dir("super")
        .checks(Check::OUTPUT)
        .run();
    let mut fx = fixture();
    fx.compare(["submodule--helper", "get-default-remote", "f"])
        .in_dir("super")
        .checks(Check::OUTPUT)
        .run();
}
