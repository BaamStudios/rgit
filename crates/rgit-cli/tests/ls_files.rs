// SPDX-License-Identifier: MIT
//! Differential tests of `rgit ls-files` against the reference git.

use rgit_difftest::{Check, Fixture};

const BLOB_A: &str = "1234567890123456789012345678901234567890";

/// Tracked, modified, deleted, unmerged, skip-worktree, assume-unchanged,
/// intent-to-add and untracked files, plus file/directory conflicts for
/// `--killed`.
fn fixture() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a/b/f", "x\n")
        .write("a/g", "y\n")
        .write("mod", "m\n")
        .write("del", "d\n")
        .write("skip", "s\n")
        .write("assume", "u\n")
        .write("k/kf", "k\n")
        .write("kfile", "kk\n")
        .write("q\"uote", "q\n")
        .write("\u{e4}", "umlaut\n")
        .symlink("mod", "link")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "m"])
        .write("mod", "changed\n")
        .remove("del")
        .git(["update-index", "--skip-worktree", "skip"])
        .git(["update-index", "--assume-unchanged", "assume"])
        .write("assume", "changed\n")
        .remove("kfile")
        .write("kfile/inner", "z\n")
        .remove("k")
        .write("k", "notdir\n")
        .write("untracked", "new\n")
        .write("un/dir/x", "n\n")
        .write("ign/x.o", "o\n")
        .write(".gitignore", "*.o\n")
        .write("ita", "n\n")
        .git(["add", "-N", "ita"]);
    if fx.is_skipped() {
        return fx;
    }
    let info =
        format!("100644 {BLOB_A} 1\tconf\n100644 {BLOB_A} 2\tconf\n100644 {BLOB_A} 3\tconf\n");
    let mut fx = fx;
    fx.compare(["update-index", "--index-info"])
        .stdin(info)
        .checks([Check::ExitCode])
        .run();
    fx.write("conf", "conflict\n")
}

fn check(fx: &mut Fixture, dir: &str, args: &[&str]) {
    let mut argv = vec!["ls-files"];
    argv.extend_from_slice(args);
    fx.compare(&argv).in_dir(dir).checks(Check::OUTPUT).run();
}

#[test]
fn listing_modes() {
    let mut fx = fixture();
    for args in [
        &[][..],
        &["-s"],
        &["-t", "-c", "-d", "-m", "-o", "-k", "-u"],
        &["-v"],
        &["-v", "-s"],
        &["-f"],
        &["-d", "-m"],
        &["-d", "-m", "--deduplicate"],
        &["--deduplicate", "conf"],
        &["-s", "-m"],
        &["-u", "-m"],
        &["-u"],
        &["-m"],
        &["-d"],
        &["-o"],
        &["-k"],
        &["-o", "--directory"],
        &["-o", "--directory", "--no-empty-directory"],
        &["-z", "-s"],
        &["-z", "-t", "-o"],
        &["--debug", "a/g", "ita"],
        &["--abbrev", "-s", "a"],
        &["--abbrev=5", "-s"],
        &["--full-name"],
        &["--sparse"],
    ] {
        check(&mut fx, "", args);
    }
    fx.compare(["-c", "core.quotepath=false", "ls-files"])
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn pathspecs_and_subdirectories() {
    let mut fx = fixture();
    for args in [
        &["a/*"][..],
        &[":(exclude)a"],
        &["nosuch"],
        &["-o", "un"],
        &["-o", "un/*"],
        &["-k", "kfile"],
        &["--error-unmatch", "nosuch", "a/g", "other"],
        &["--error-unmatch", "-o", "nosuch"],
        &["-m", "--error-unmatch", "mod", "a/g"],
        &["--error-unmatch", ":(glob)nosuch", "a/*"],
    ] {
        check(&mut fx, "", args);
    }
    for args in [
        &[][..],
        &["../mod", "../a*"],
        &["--full-name"],
        &["-o", ".."],
        &["-t", ".."],
    ] {
        check(&mut fx, "a", args);
    }
    check(&mut fx, "a/b", &["-t", "../.."]);
}

#[test]
fn excludes() {
    let mut fx = fixture();
    for args in [
        &["-o", "--exclude-standard"][..],
        &["-o", "-i", "--exclude-standard"],
        &["-o", "-x", "u*"],
        &["-o", "-x", "u*", "-X", "nosuchfile"],
        &["-o", "-x", "u*", "--exclude-per-directory=.gitignore"],
        &["-o", "-i", "-x", "u*"],
        &["-o", "-i", "-x", "un", "--directory"],
        &["-c", "-i", "-x", "a"],
        &["-c", "-i", "-x", "b"],
        &["-i"],
        &["-i", "-o"],
    ] {
        check(&mut fx, "", args);
    }
}

#[test]
fn formats_and_errors() {
    let mut fx = fixture();
    for args in [
        &[
            "--format=%(objectmode)|%(objecttype)|%(objectname)|%(objectsize)|%(objectsize:padded)|%(stage)|%(path)",
            "a",
            "conf",
            "link",
        ][..],
        &["--format=%(bogus)", "a"],
        &["--format=%(path)", "--debug", "a"],
        &["--format=x", "-s"],
        &["--format=x", "-t"],
        &["--format=x", "-v"],
        &["--format=x", "--deduplicate"],
        &["--format=%(path)", "-z", "-m"],
        &["--with-tree=HEAD", "-s"],
        &["--with-tree=nosuch"],
        &["--with-tree=HEAD"],
        &["--recurse-submodules", "--with-tree=HEAD"],
        &["-h"],
    ] {
        check(&mut fx, "", args);
    }
}

#[test]
fn with_tree_after_removal() {
    let mut fx = fixture().git(["rm", "-q", "--cached", "a/g"]);
    check(&mut fx, "", &["--with-tree=HEAD", "-t"]);
    check(&mut fx, "", &["--with-tree=HEAD", "--error-unmatch", "a/g"]);
    check(&mut fx, "", &["--error-unmatch", "a/g"]);
    check(&mut fx, "a", &["--with-tree=HEAD"]);
}
