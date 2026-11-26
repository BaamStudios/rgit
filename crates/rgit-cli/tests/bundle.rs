// SPDX-License-Identifier: MIT
//! `rgit bundle` against the reference git: headers byte for byte, the
//! messages of `verify`, `list-heads` and `unbundle`, and bundles that
//! travel both ways (an rgit bundle verifies and unbundles with git, a git
//! bundle with rgit). Packs may differ in their delta choice; the objects
//! are compared instead.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

/// main: c0 -> c1 -> c2 -> c3, side branching at c1, an annotated tag at
/// c2 and a commit with a multi-line subject.
fn repo() -> Fixture {
    let mut fx = fixture().git(["init", "-q"]);
    for c in 0..4 {
        fx = fx
            .write("a.txt", format!("line {c}\n").repeat(20 + c))
            .write(format!("d/f{c}.txt"), format!("{c}\n"))
            .git(["add", "-A"])
            .git(["commit", "-q", "-m", &format!("c{c}\nmore\n\nbody")])
            .tick();
        if c == 1 {
            fx = fx.git(["branch", "side"]);
        }
        if c == 2 {
            fx = fx.git(["tag", "-a", "-m", "v1", "v1"]).tick();
        }
    }
    fx.git(["checkout", "-q", "side"])
        .write("side.txt", "side\n")
        .git(["add", "side.txt"])
        .git(["commit", "-q", "-m", "side"])
        .tick()
        .git(["checkout", "-q", "main"])
}

/// The header of a bundle (up to and including the empty line).
fn header(bundle: &[u8]) -> &[u8] {
    let end = bundle
        .windows(2)
        .position(|w| w == b"\n\n")
        .map_or(bundle.len(), |p| p + 2);
    &bundle[..end]
}

/// Create a bundle with `args` on both sides (written to standard
/// output); the headers must be equal. Returns rgit's bundle.
fn create(fx: &mut Fixture, args: &[&str]) -> Vec<u8> {
    let mut argv = vec!["bundle", "create", "-q", "-"];
    argv.extend_from_slice(args);
    let Some(outcome) = fx
        .compare(&argv)
        .checks([Check::ExitCode, Check::Stderr])
        .run()
    else {
        return Vec::new();
    };
    assert_eq!(
        String::from_utf8_lossy(header(&outcome.rgit.stdout)),
        String::from_utf8_lossy(header(&outcome.git.stdout)),
        "bundle create {args:?}"
    );
    outcome.rgit.stdout
}

#[test]
fn headers_match() {
    let mut fx = repo();
    for args in [
        &["--all"][..],
        &["main"],
        &["main~2..main", "side"],
        &["-2", "main", "side"],
        &["v1"],
        &["main^!"],
        &["--all", "--not", "side"],
        &["--version=3", "main"],
    ] {
        create(&mut fx, args);
    }
}

#[test]
fn errors_match() {
    let mut fx = repo();
    for args in [
        &["bundle", "create", "x.bdl", "main..main"][..],
        &["bundle", "create", "--version=4", "x.bdl", "main"],
        &["bundle", "create"],
        &["bundle", "verify"],
        &["bundle", "list-heads"],
        &["bundle", "unbundle"],
        &["bundle", "verify", "missing.bdl"],
        &["bundle"],
    ] {
        fx = fx.same(args);
    }
}

#[test]
fn rgit_bundle_works_with_git() {
    let mut fx = repo();
    let incremental = create(&mut fx, &["main~2..main", "v1"]);
    let full = create(&mut fx, &["--all"]);
    if full.is_empty() {
        return; // no reference git
    }
    fx = fx
        .write("inc.bdl", &incremental)
        .write("full.bdl", &full)
        .git(["init", "-q", "--bare", "copy.git"]);
    let out = fx.git_output(["--git-dir=copy.git", "bundle", "verify", "full.bdl"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = fx.git_output(["--git-dir=copy.git", "bundle", "unbundle", "full.bdl"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Everything git's own bundle of the same refs brings is there.
    fx = fx
        .git(["bundle", "create", "theirs.bdl", "--all"])
        .git(["init", "-q", "--bare", "theirs.git"])
        .git(["--git-dir=theirs.git", "bundle", "unbundle", "theirs.bdl"]);
    let objects = |fx: &Fixture, dir: &str| {
        fx.git_stdout([
            &format!("--git-dir={dir}"),
            "cat-file",
            "--batch-all-objects",
            "--batch-check",
        ])
    };
    assert_eq!(objects(&fx, "copy.git"), objects(&fx, "theirs.git"));
    // The incremental bundle applies on top.
    let out = fx.git_output(["--git-dir=copy.git", "bundle", "verify", "inc.bdl"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn git_bundle_works_with_rgit() {
    let fx = repo()
        .git(["bundle", "create", "full.bdl", "--all"])
        .git(["bundle", "create", "inc.bdl", "main~2..main"])
        .git(["init", "-q", "empty"]);
    let fx = fx
        .same(["bundle", "verify", "full.bdl"])
        .same(["bundle", "verify", "-q", "inc.bdl"])
        .same(["bundle", "list-heads", "full.bdl"])
        .same([
            "bundle",
            "list-heads",
            "full.bdl",
            "refs/heads/main",
            "HEAD",
        ])
        .same(["ls-remote", "full.bdl"]);
    let mut fx = fx;
    // Missing prerequisites in an empty repository.
    let _ = fx
        .compare(["-C", "empty", "bundle", "verify", "../inc.bdl"])
        .checks([Check::ExitCode, Check::Stdout, Check::Stderr])
        .run();
    let _ = fx
        .compare(["-C", "empty", "bundle", "unbundle", "../full.bdl"])
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::Refs,
            Check::Objects,
        ])
        .run();
}

#[test]
fn clone_and_fetch_from_bundle() {
    let fx = repo().git(["bundle", "create", "full.bdl", "--all"]).git([
        "bundle",
        "create",
        "inc.bdl",
        "side~1..side",
    ]);
    let mut fx = fx;
    let _ = fx
        .compare(["clone", "-q", "full.bdl", "copy"])
        .checks([Check::ExitCode, Check::Stdout, Check::Stderr])
        .run();
    let fx = fx.git(["clone", "-q", "--bare", "full.bdl", "bare.git"]);
    let mut fx = fx;
    let _ = fx
        .compare([
            "--git-dir=bare.git",
            "fetch",
            "-q",
            "inc.bdl",
            "+refs/*:refs/*",
        ])
        .checks([Check::ExitCode, Check::Stdout, Check::Stderr])
        .run();
}

#[test]
fn sha256_and_v3() {
    let mut fx = fixture()
        .git(["init", "-q", "--object-format=sha256"])
        .write("a", "a\n")
        .git(["add", "a"])
        .git(["commit", "-q", "-m", "a"])
        .tick()
        .write("a", "b\n")
        .git(["commit", "-q", "-a", "-m", "b"]);
    create(&mut fx, &["--all"]);
    create(&mut fx, &["HEAD~1..HEAD"]);
    fx.git(["bundle", "create", "v3.bdl", "--all"])
        .same(["bundle", "verify", "v3.bdl"])
        .same(["bundle", "create", "--version=2", "x.bdl", "--all"]);
    let mut fx = fixture()
        .git(["init", "-q"])
        .write("a", "a\n")
        .git(["add", "a"])
        .git(["commit", "-q", "-m", "a"]);
    let _ = fx
        .compare(["bundle", "create", "-q", "--version=3", "-", "--all"])
        .checks([Check::ExitCode, Check::Stdout, Check::Stderr])
        .normalize_stdout(rgit_difftest::Normalizer::regex("(?s)PACK.*", "PACK"))
        .run();
}
