// SPDX-License-Identifier: MIT
//! Differential tests of `rgit archive`, `upload-archive` and
//! `get-tar-commit-id` against the reference git. Archives are written
//! with `-o` so that the work tree comparison checks them byte for byte.
//! Deflated output (zip levels above 0, tgz) depends on the deflate
//! implementation and is checked by round trips in `rgit-archive` instead.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    let long = "x".repeat(120);
    let deep = "abcdefghi/".repeat(17);
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "simple\n")
        .write(format!("d/{long}/f"), "long dir\n")
        .write(&long, "long name\n")
        .write(format!("{deep}file"), "deep\n")
        .write(format!("{}/q", "p".repeat(154)), "prefix 154\n")
        .write(format!("{}/q", "r".repeat(155)), "prefix 155\n")
        .write("bin", b"\0\x01\x02".as_slice())
        .write("subst", "A$Format:%H %s$O\n")
        .write(".gitattributes", "subst export-subst\nign* export-ignore\n")
        .write("ignd/x", "ignored\n")
        .write("ignf", "ignored\n")
        .write_executable("run", "#!/bin/sh\n")
        .symlink("a", "link")
        .symlink(long.repeat(2), "longlink")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "first"])
        .git(["tag", "-a", "-m", "t", "v1"])
}

fn check(fx: &mut Fixture, args: &[&str]) {
    let _ = std::fs::remove_file(fx.work_dir().join("out.bin"));
    let mut argv = vec!["archive", "-o", "out.bin"];
    argv.extend_from_slice(args);
    fx.compare(&argv)
        .checks([Check::ExitCode, Check::Stderr, Check::Worktree])
        .run();
}

#[test]
fn tar_is_byte_identical() {
    let mut fx = fixture();
    for args in [
        &["HEAD"][..],
        &["v1"],
        &["--prefix=pre/fix/", "HEAD"],
        &["--prefix=olde-", "HEAD"],
        &["--mtime=2002-02-02T02:02:02-0200", "HEAD"],
        &["HEAD", "d", "link"],
        &["HEAD", ":(glob)**/f"],
        &["HEAD", ":!d"],
        &[
            "--prefix=a/",
            "--add-file=a",
            "--prefix=b/",
            "--add-virtual-file=v/w:hello",
            "HEAD",
        ],
        &["--format=tar", "-v", "HEAD"],
    ] {
        check(&mut fx, args);
    }
}

#[test]
fn tar_umask() {
    let mut fx = fixture().git(["config", "tar.umask", "022"]);
    check(&mut fx, &["HEAD"]);
}

#[test]
fn stored_zip_is_byte_identical() {
    let mut fx = fixture();
    for args in [
        &["--format=zip", "-0", "HEAD"][..],
        &["--format=zip", "-0", "--prefix=p/", "HEAD"],
    ] {
        check(&mut fx, args);
    }
}

#[test]
fn errors() {
    let mut fx = fixture();
    for args in [
        &["nope"][..],
        &["--format=foo", "HEAD"],
        &["-3", "HEAD"],
        &["--format=zip", "-10", "HEAD"],
        &["HEAD", "nope"],
        &["--add-virtual-file=nocolon", "HEAD"],
        &["--add-file=missing", "HEAD"],
    ] {
        let mut argv = vec!["archive"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
    fx.compare(["archive", "--list"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["archive", "--list", "extra"])
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn remote_archive() {
    let mut fx = fixture();
    check(&mut fx, &["--remote=.", "HEAD"]);
    check(&mut fx, &["--remote=.", "--format=zip", "-0", "HEAD", "d"]);
    check(
        &mut fx,
        &["--remote=.", "--mtime=2002-02-02 02:02:02 +0000", "HEAD:d"],
    );
    fx.compare(["archive", "--remote=.", "--list"])
        .checks(Check::OUTPUT)
        .run();
    // Client and server share standard error; the order of their last
    // lines is a race in git too.
    fx.compare(["archive", "--remote=.", "HEAD^"])
        .checks([Check::ExitCode, Check::Stdout])
        .run();
}

#[test]
fn get_tar_commit_id() {
    let mut fx = fixture();
    let tar = fx.git_stdout(["archive", "HEAD"]);
    fx.compare(["get-tar-commit-id"])
        .stdin(tar)
        .checks(Check::OUTPUT)
        .run();
    let tar = fx.git_stdout(["archive", "HEAD^{tree}"]);
    fx.compare(["get-tar-commit-id"])
        .stdin(tar)
        .checks(Check::OUTPUT)
        .run();
}
