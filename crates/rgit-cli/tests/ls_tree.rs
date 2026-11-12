// SPDX-License-Identifier: MIT
//! Differential tests of `rgit ls-tree` against the reference git.

use rgit_difftest::{Check, Fixture};

/// A repository with nested directories, odd file names, a tag, a
/// gitlink, and a packed copy of some objects.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a/b/f", "x\n")
        .write("a/g", "y\n")
        .write("c/\"q", "w\n")
        .write("c/\u{e4}\tq", "z\x7f")
        .write("c/d\x7f", "q\x7fz")
        .write("top", "1\n")
        .write("a[a]/three", "")
        .write_executable("bin/run", "#!/bin/sh\n")
        .symlink("top", "link")
        .git(["add", "."])
        .git([
            "update-index",
            "--add",
            "--cacheinfo",
            "160000,1111111111111111111111111111111111111111,sub",
        ])
        .git(["commit", "-q", "-m", "first"])
        .git(["tag", "-a", "-m", "t", "v1"])
        .git(["repack", "-a", "-d", "-q"])
        .tick()
        .write("late", "new\n")
        .git(["add", "late"])
        .git(["commit", "-q", "-m", "second"])
}

fn check(fx: &mut Fixture, dir: &str, args: &[&str]) {
    let mut argv = vec!["ls-tree"];
    argv.extend_from_slice(args);
    fx.compare(&argv).in_dir(dir).checks(Check::OUTPUT).run();
}

#[test]
fn listing_modes() {
    let mut fx = fixture();
    for args in [
        &["HEAD"][..],
        &["-r", "HEAD"],
        &["-r", "-t", "HEAD"],
        &["-d", "HEAD"],
        &["-d", "-r", "HEAD"],
        &["-r", "-l", "HEAD"],
        &["-l", "-t", "-r", "HEAD"],
        &["--name-only", "-r", "HEAD"],
        &["--name-status", "HEAD"],
        &["--object-only", "-r", "HEAD"],
        &["-z", "-r", "HEAD"],
        &["-z", "--name-only", "-r", "HEAD"],
        &["HEAD^{tree}"],
        &["v1"],
        &["HEAD~1"],
        &["HEAD^"],
        &["HEAD:a"],
        &["main"],
        &["refs/tags/v1^{}"],
    ] {
        check(&mut fx, "", args);
    }
}

#[test]
fn quoting() {
    let mut fx = fixture();
    check(&mut fx, "", &["-r", "HEAD", "c"]);
    fx.compare(["-c", "core.quotePath=false", "ls-tree", "-r", "HEAD"])
        .checks(Check::OUTPUT)
        .run();
    check(&mut fx, "", &["-r", "-z", "HEAD", "c"]);
}

#[test]
fn abbreviations() {
    let mut fx = fixture();
    for args in [
        &["--abbrev", "HEAD"][..],
        &["--abbrev=2", "-r", "HEAD"],
        &["--abbrev=0", "HEAD"],
        &["--abbrev=99", "HEAD"],
        &["--no-abbrev", "HEAD"],
        &["--abbrev=5", "--object-only", "HEAD"],
        &["--abbrev=5", "-l", "HEAD"],
        &["--abbrev=x", "HEAD"],
    ] {
        check(&mut fx, "", args);
    }
    fx.compare(["-c", "core.abbrev=12", "ls-tree", "--abbrev", "HEAD"])
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn path_patterns() {
    let mut fx = fixture();
    for args in [
        &["HEAD", "a/b"][..],
        &["HEAD", "a/b/"],
        &["HEAD", "a/b/f/"],
        &["-d", "HEAD", "a/b"],
        &["-d", "HEAD", "a/b/f", "a"],
        &["-r", "-d", "HEAD", "a/b/f"],
        &["-t", "HEAD", "a/b/f"],
        &["-r", "-t", "HEAD", "a/b"],
        &["HEAD", "a", "a/b/f"],
        &["HEAD", "top", "./top", ".//top", "a/./g", "a//"],
        &["HEAD", "pa", "a/x", "top/"],
        &["-r", "HEAD", "a[a]"],
        &["HEAD", "*"],
        &["HEAD", ":(literal)a"],
        &["HEAD", ":(glob)a/*"],
        &["HEAD", ":(exclude)a"],
        &["HEAD", "--", "a"],
        &["-r", "HEAD", "sub", "sub/x"],
    ] {
        check(&mut fx, "", args);
    }
}

#[test]
fn from_subdirectory() {
    let mut fx = fixture();
    for args in [
        &["HEAD"][..],
        &["-r", "HEAD"],
        &["HEAD", "."],
        &["HEAD", ".."],
        &["HEAD", "../c", "../top"],
        &["-r", "HEAD", "../../x"],
        &["--full-name", "HEAD", "g"],
        &["--no-full-name", "HEAD", "g"],
        &["--full-tree", "HEAD"],
        &["--full-tree", "HEAD", "../c"],
        &["--full-tree", "-r", "HEAD", "c"],
        &["HEAD", ":/top"],
        &["HEAD:./b"],
    ] {
        check(&mut fx, "a", args);
    }
    check(&mut fx, "a/b", &["-r", "-t", "HEAD", "../.."]);
    check(&mut fx, "a/b", &["HEAD", "../../a"]);
}

#[test]
fn formats() {
    let mut fx = fixture();
    for fmt in [
        "--format=%(objectmode) %(objecttype) %(objectname)%x09%(path)",
        "--format=%(objectmode)|%(objecttype)|%(objectname)|%(objectsize)|%(objectsize:padded)|%(path)|%%|%x41%n",
        "--format=",
        "--format=%(path)%x00",
        "--format=%(bogus)",
        "--format=%(objectname",
        "--format=%x0|%(path)",
        "--format=a%",
        "--format=%(objectsize:paddedx)",
    ] {
        check(&mut fx, "", &[fmt, "-r", "-t", "HEAD"]);
        check(&mut fx, "", &[fmt, "-z", "--abbrev=6", "HEAD", "c", "top"]);
    }
    check(&mut fx, "", &["--format=%(bogus)", "HEAD", "nosuch"]);
}

#[test]
fn usage_errors() {
    let mut fx = fixture();
    for args in [
        &[][..],
        &["-h"],
        &["--name-only", "--object-only", "HEAD"],
        &["--name-status", "--name-only", "HEAD"],
        &["-l", "--name-only", "HEAD"],
        &["--long", "--object-only", "HEAD"],
        &["--format=x", "-l", "HEAD"],
        &["--object-only", "--format=x", "HEAD"],
        &["--no-name-only", "HEAD"],
    ] {
        check(&mut fx, "", args);
    }
}

#[test]
fn bad_objects() {
    let mut fx = fixture();
    for args in [
        &["nosuch"][..],
        &["HEAD:top"],
        &["HEAD:nosuch"],
        &["HEAD^{blob}"],
        &["HEAD^{foo}"],
        &["1234567890123456789012345678901234567890"],
        &["v1^{tag}"],
    ] {
        check(&mut fx, "", args);
    }
}

#[test]
fn missing_subtree() {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a/one", "")
        .write("b", "")
        .write("c/two", "2\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "m"]);
    if fx.is_skipped() {
        return;
    }
    let tree = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD:c"])).unwrap();
    let tree = tree.trim();
    let mut fx = fx.remove(format!(".git/objects/{}/{}", &tree[..2], &tree[2..]));
    check(&mut fx, "", &["HEAD"]);
    check(&mut fx, "", &["-r", "HEAD"]);
    check(&mut fx, "", &["-r", "-t", "-l", "HEAD"]);
    check(&mut fx, "", &["HEAD:c"]);
}
