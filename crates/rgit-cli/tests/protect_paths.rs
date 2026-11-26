// SPDX-License-Identifier: MIT
//! Differential tests of `core.protectNTFS` and `core.protectHFS` (#183)
//! for the commands that write index entries: paths that NTFS or HFS+ take
//! for `.git` (and symbolic links they take for `.gitmodules`) are refused
//! with git's messages, depending on the two settings.

use rgit_difftest::{Check, Fixture};

const STATE: [Check; 4] = [Check::ExitCode, Check::Stdout, Check::Stderr, Check::Index];

/// `.git` with a zero-width non-joiner, which HFS+ ignores.
const HFS_DOT_GIT: &str = ".g\u{200c}it";

const SETTINGS: [Option<&str>; 5] = [
    None,
    Some("core.protectNTFS=false"),
    Some("core.protectNTFS=true"),
    Some("core.protectHFS=true"),
    Some("core.protectHFS=false"),
];

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "a\n")
        .git(["add", "a"])
        .git(["commit", "-q", "-m", "one"])
}

fn with_setting<'a>(setting: Option<&'a str>, args: &[&'a str]) -> Vec<&'a str> {
    let mut argv = Vec::new();
    if let Some(s) = setting {
        argv.extend(["-c", s]);
    }
    argv.extend_from_slice(args);
    argv
}

#[test]
fn update_index_refuses_what_git_refuses() {
    let hfs_dir = format!("{HFS_DOT_GIT}/x");
    let hfs_modules = ".g\u{200c}itmodules".to_string();
    let paths = [
        ".git/x",
        ".GIT/x",
        "git~1/x",
        "GIT~1",
        ".git /x",
        ".git./x",
        ".git . ./x",
        ".git::$INDEX_ALLOCATION/x",
        ".git:stream",
        "a\\.git\\x",
        "a\\git~1",
        "sub/.git",
        "sub/git~1",
        "sub/GIT~1/x",
        HFS_DOT_GIT,
        &hfs_dir,
        "git~2/x",
        "git~1.",
        "git~1 /x",
        "git~1:x",
        "git~10",
        "~1",
        "con",
        "a:b",
        "trail.",
        "trail ",
        "x/../y",
        ".gitmodules ",
        ".gitattributes.",
        "gitmod~1",
        "gi7eba~1",
        ".gitx",
        "x.git",
    ];
    let links = [
        ".gitmodules",
        ".GITMODULES",
        "sub/.gitmodules",
        ".gitmodules ",
        ".gitmodules.",
        ".gitmodules::$DATA",
        "gitmod~1",
        "GITMOD~1",
        "gi7eba~1",
        "gitmod~4",
        "gitmod~5",
        "gi7eba~9",
        "gi7eba~0",
        "gitmod~1. .",
        "gitmod~1:x",
        "sub\\.gitmodules",
        &hfs_modules,
        ".gitmodulesx",
        ".gitignore",
    ];
    for setting in SETTINGS {
        let mut fx = fixture();
        if fx.is_skipped() {
            return;
        }
        let blob = String::from_utf8(fx.git_stdout(["rev-parse", ":a"])).unwrap();
        let blob = blob.trim();
        for (mode, list) in [("100644", &paths[..]), ("120000", &links[..])] {
            for path in list {
                let info = format!("{mode},{blob},{path}");
                let args = with_setting(setting, &["update-index", "--add", "--cacheinfo", &info]);
                fx.compare(args).checks(STATE).run();
            }
        }
        fx.compare(["ls-files", "-s"]).run();
    }
}

/// The names exist on disk: only where the file system can hold them.
#[cfg(target_os = "linux")]
#[test]
fn add_and_mv_refuse_what_git_refuses() {
    let cases: &[&[&str]] = &[
        &["add", "-A"],
        &["add", "git~1/x"],
        &["add", HFS_DOT_GIT],
        &["add", "d"],
        &["add", "-v", "bs\\.git\\x"],
        &["add", ".gitmodules "],
        &["add", "gitmod~1"],
        &["add", ".gitmodules"],
        &["mv", "a", "GIT~1"],
        &["mv", "a", "d/.git "],
        &["status", "--porcelain"],
    ];
    for setting in SETTINGS {
        for args in cases {
            let mut fx = fixture()
                .write("git~1/x", "x\n")
                .write(".git /y", "y\n")
                .write("d/git~1", "z\n")
                .write(HFS_DOT_GIT, "w\n")
                .write("bs\\.git\\x", "v\n")
                .symlink("a", ".gitmodules ")
                .symlink("a", "gitmod~1")
                .symlink("a", ".gitmodules");
            if fx.is_skipped() {
                return;
            }
            fx.compare(with_setting(setting, args)).checks(STATE).run();
        }
    }
}

#[test]
fn checkout_refuses_what_git_refuses() {
    // The tree is built with the protection off, then read with it on.
    for (path, setting) in [
        ("git~1/x", "core.protectNTFS=true"),
        (".git /x", "core.protectNTFS=true"),
        (HFS_DOT_GIT, "core.protectHFS=true"),
        ("fine", "core.protectNTFS=true"),
    ] {
        let fx = fixture();
        if fx.is_skipped() {
            return;
        }
        let blob = String::from_utf8(fx.git_stdout(["rev-parse", ":a"])).unwrap();
        let info = format!("100644,{},{path}", blob.trim());
        let mut fx = fx
            .git([
                "-c",
                "core.protectNTFS=false",
                "-c",
                "core.protectHFS=false",
                "update-index",
                "--add",
                "--cacheinfo",
                &info,
            ])
            .git([
                "-c",
                "core.protectNTFS=false",
                "-c",
                "core.protectHFS=false",
                "commit",
                "-q",
                "-m",
                "odd path",
            ])
            .git(["tag", "odd"])
            .git(["read-tree", "HEAD^"]);
        for args in [
            &["read-tree", "odd"][..],
            &["read-tree", "-m", "odd"],
            &["reset", "--mixed", "odd"],
        ] {
            fx.compare(with_setting(Some(setting), args))
                .checks(STATE)
                .run();
        }
    }
}
