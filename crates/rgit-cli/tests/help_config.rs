// SPDX-License-Identifier: MIT
//! `help -c`, `--config-for-completion` and
//! `--config-sections-for-completion`: the configuration variables taken
//! from git's documentation at build time (exactness is checked by the
//! conformance script t0012 against the pinned git), and `rm
//! --pathspec-from-file` against the reference git.

use std::process::Command;

use rgit_difftest::{Check, Fixture};

fn help(arg: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(["help", arg])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{arg}");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn config_lists() {
    let all = help("-c");
    let (names, footer) = all.split_once("\n\n").expect("footer");
    assert_eq!(footer, "'git help config' for more information\n");
    let names: Vec<&str> = names.lines().collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
    for want in [
        "core.bare",
        "advice.detachedHead",
        "color.diff.newMoved",
        "fsck.badDate",
        "receive.fsck.nullSha1",
        "url.<base>.insteadOf",
        "column.ui",
    ] {
        assert!(names.contains(&want), "{want}");
    }
    assert!(
        !names
            .iter()
            .any(|n| n.ends_with("<slot>") || *n == "advice.*")
    );

    let completion = help("--config-for-completion");
    assert!(completion.lines().any(|l| l == "url."));
    assert!(completion.lines().any(|l| l == "fetch.fsck."));
    let sections = help("--config-sections-for-completion");
    assert!(sections.lines().any(|l| l == "core"));
    assert!(!sections.contains('.'));
}

#[test]
fn rm_pathspec_from_file() {
    let fixture = || {
        Fixture::new()
            .rgit(env!("CARGO_BIN_EXE_rgit"))
            .git(["init", "-q", "-b", "main"])
            .write("a", "a\n")
            .write("b", "b\n")
            .write("c", "c\n")
            .git(["add", "."])
            .git(["commit", "-q", "-m", "one"])
            .write("list", "a\nc\n")
            .write("nul", "b\0")
            .write("empty", "")
    };
    for args in [
        &["rm", "-q", "--pathspec-from-file=list"][..],
        &[
            "rm",
            "--cached",
            "--pathspec-from-file=nul",
            "--pathspec-file-nul",
        ],
        &["rm", "--pathspec-from-file=list", "--", "a"],
        &["rm", "--pathspec-file-nul"],
        &["rm", "--pathspec-from-file=empty"],
        &["rm", "--sparse", "a"],
    ] {
        let mut fx = fixture();
        if fx.is_skipped() {
            return;
        }
        fx.compare(args).run();
    }
    let mut fx = fixture();
    fx.compare(["rm", "--pathspec-from-file=-"])
        .stdin("b\n")
        .checks(Check::OUTPUT)
        .run();
}
