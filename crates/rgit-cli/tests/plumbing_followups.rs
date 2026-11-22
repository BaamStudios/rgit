// SPDX-License-Identifier: MIT
//! Differential tests of smaller behaviours found with the conformance
//! scripts: an empty index file for `read-tree`, `update-index --remove`
//! of a file replaced by a directory, `checkout-index` with missing blobs,
//! submodules and a `--prefix` through a symbolic link, NTFS paths with
//! backslashes, `remote.<name>.followRemoteHEAD` warnings and
//! `core.preferSymlinkRefs`.

use rgit_difftest::{Check, Fixture};

fn repo() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .write("d/f", "f\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
}

#[test]
fn empty_index_file() {
    for args in [
        &["read-tree", "HEAD"][..],
        &["read-tree", "-m", "HEAD"],
        &["ls-files"],
    ] {
        let mut fx = repo().write("empty-index", "");
        if fx.is_skipped() {
            return;
        }
        fx.compare(args)
            .env("GIT_INDEX_FILE", "empty-index")
            .checks(Check::OUTPUT)
            .run();
    }
}

#[test]
fn update_index_remove_replaced_by_directory() {
    let mut fx = repo().remove("a").write("a/b", "b\n");
    if fx.is_skipped() {
        return;
    }
    fx.compare(["update-index", "--add", "--remove", "a", "a/b"])
        .run();
    let mut fx = repo().remove("a").write("a/b", "b\n");
    fx.compare(["update-index", "--add", "a", "a/b"]).run();
}

/// `--cacheinfo` entries whose objects do not exist.
fn missing_blobs() -> Fixture {
    let bogus = "0123456789abcdef0123456789abcdef01234567";
    repo()
        .git([
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{bogus},file"),
        ])
        .git([
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("120000,{bogus},symlink"),
        ])
        .git([
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{bogus},sub"),
        ])
}

#[test]
fn checkout_index_errors_and_prefix() {
    let names =
        || rgit_difftest::Normalizer::regex(r"\.merge_(file|link)_[A-Za-z0-9]{6}", ".merge_${1}_X");
    for args in [
        &["checkout-index", "file", "symlink", "a"][..],
        &["checkout-index", "-f", "-a"],
        &["checkout-index", "--temp", "symlink", "file", "a"],
        &["checkout-index", "--temp", "sub"],
    ] {
        let mut fx = missing_blobs();
        if fx.is_skipped() {
            return;
        }
        fx.compare(args)
            .checks(Check::OUTPUT)
            .normalize(names())
            .run();
    }
    // --prefix through a symbolic link to a directory.
    #[cfg(unix)]
    for prefix in ["--prefix=tmp/orary/", "--prefix=tmp/orary-"] {
        let mut fx = repo()
            .write("tmp1/orary/keep", "k\n")
            .symlink("tmp1", "tmp");
        fx.compare(["checkout-index", prefix, "-f", "-a"]).run();
    }
}

/// A tree with one entry at `path` (written by the reference git).
fn tree_with(fx: &Fixture, path: &str) -> String {
    let blob = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD:a"])).unwrap();
    let input = format!("100644 blob {}\t{path}\n", blob.trim());
    let mut child = std::process::Command::new(fx.reference_git())
        .args(["mktree"])
        .current_dir(fx.work_dir())
        .env("HOME", fx.home_dir())
        .env("XDG_CONFIG_HOME", fx.home_dir().join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[test]
fn ntfs_backslash_paths() {
    for path in [
        r".git\foobar",
        r".\.GIT\foobar",
        r"x\.git",
        r"x\git~1\y",
        r"a\b",
    ] {
        let mut fx = repo();
        if fx.is_skipped() {
            return;
        }
        let tree = tree_with(&fx, path);
        for cfg in ["true", "false"] {
            let protect = format!("core.protectNTFS={cfg}");
            fx.compare(["-c", protect.as_str(), "read-tree", tree.as_str()])
                .checks(Check::OUTPUT)
                .run();
        }
    }
}

#[test]
fn follow_remote_head_warning() {
    for value in [
        "bogus",
        "warn",
        "always",
        "never",
        "create",
        "warn-if-not-main",
        "WARN",
        "",
    ] {
        let mut fx = repo()
            .git(["remote", "add", "origin", "https://example.com/x.git"])
            .git(["config", "remote.origin.followRemoteHEAD", value]);
        if fx.is_skipped() {
            return;
        }
        for args in [
            &["remote", "-v"][..],
            &["remote"],
            &["remote", "show", "-n", "origin"],
        ] {
            fx.compare(args).checks(Check::OUTPUT).run();
        }
    }
}

#[cfg(unix)]
#[test]
fn prefer_symlink_refs() {
    for args in [
        &[
            "-c",
            "core.preferSymlinkRefs=true",
            "symbolic-ref",
            "HEAD",
            "refs/heads/other",
        ][..],
        &[
            "-c",
            "core.preferSymlinkRefs=false",
            "symbolic-ref",
            "HEAD",
            "refs/heads/main",
        ],
        &["symbolic-ref", "refs/heads/x", "refs/heads/main"],
        &[
            "-c",
            "core.preferSymlinkRefs=true",
            "checkout",
            "-q",
            "-b",
            "nb",
        ],
        &[
            "-c",
            "core.preferSymlinkRefs=true",
            "switch",
            "-q",
            "--orphan",
            "o",
        ],
    ] {
        let mut fx = repo();
        if fx.is_skipped() {
            return;
        }
        // (git also logs a symbolic-ref to the same target in HEAD's
        // reflog; not compared here.)
        fx.compare(args).skip(Check::Reflogs).run();
    }
}
