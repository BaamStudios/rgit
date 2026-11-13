// SPDX-License-Identifier: MIT
//! `ls-remote` over the local transport (path and `file://`) against the
//! reference git: output and exit code.

use rgit_difftest::{Check, Fixture};

#[test]
fn ls_remote_same_as_git() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "r"])
        .git(["-C", "r", "commit", "-q", "--allow-empty", "-m", "a"])
        .git(["-C", "r", "tag", "-a", "v1", "-m", "t"])
        .git(["-C", "r", "tag", "lw"])
        .git(["-C", "r", "branch", "dev"]);
    if fx.is_skipped() {
        return;
    }
    let url = format!("file://{}", fx.work_dir().join("r").display());
    let cases: &[&[&str]] = &[
        &["r"],
        &[&url],
        &["--symref", "r"],
        &["--refs", "r"],
        &["--symref", "--refs", "r"],
        &["--tags", "r"],
        &["--heads", "r"],
        &["-h", "r"],
        &["-h"],
        &["-b", "r", "main"],
        &["r", "v1"],
        &["r", "refs/heads/*"],
        &["--exit-code", "r", "nope"],
        &["-q", "r"],
        &["--get-url", "r"],
    ];
    for args in cases {
        let mut argv = vec!["ls-remote"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
}

#[test]
fn ls_remote_remote_names_and_v0_same_as_git() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "r"])
        .git(["-C", "r", "commit", "-q", "--allow-empty", "-m", "a"])
        .git(["-C", "r", "tag", "-a", "v1", "-m", "t"])
        .git(["-C", "r", "branch", "dev"])
        .git(["clone", "-q", "r", "c"])
        .git(["-C", "c", "remote", "add", "other", "../r"])
        .git(["-C", "c", "remote", "add", "broken", "../nope"])
        .git(["init", "-q", "-b", "main", "lonely"]);
    if fx.is_skipped() {
        return;
    }
    // Inside the clone: remote names, the default remote and `From`.
    let cases: &[&[&str]] = &[
        &["ls-remote"],
        &["ls-remote", "-q"],
        &["ls-remote", "origin"],
        &["ls-remote", "other", "main"],
        &["ls-remote", "--get-url"],
        &["ls-remote", "--get-url", "other"],
        &["ls-remote", "--get-url", "nosuch"],
        &[
            "-c",
            "url.file:///x/.insteadOf=../",
            "ls-remote",
            "--get-url",
            "other",
        ],
        &["ls-remote", "broken"],
        &["ls-remote", "nosuch"],
        &["-c", "remote.origin.uploadpack=false", "ls-remote"],
        &[
            "-c",
            "protocol.version=0",
            "ls-remote",
            "--symref",
            "origin",
        ],
        &["-c", "protocol.version=1", "ls-remote", "origin", "v1"],
        &[
            "-c",
            "protocol.version=0",
            "ls-remote",
            "--refs",
            "-t",
            "origin",
        ],
        &["-c", "protocol.version=0", "ls-remote", "../lonely"],
        &["-c", "protocol.version=3", "ls-remote", "origin"],
    ];
    for argv in cases {
        fx.compare(*argv).in_dir("c").checks(Check::OUTPUT).run();
    }
    // `branch.<name>.remote` picks the default remote.
    let mut fx = fx
        .git(["-C", "c", "config", "branch.main.remote", "other"])
        .git(["-C", "lonely", "remote", "add", "up", "../r"]);
    fx.compare(["ls-remote"])
        .in_dir("c")
        .checks(Check::OUTPUT)
        .run();
    // The only configured remote is the default, also when not `origin`.
    fx.compare(["ls-remote", "--get-url"])
        .in_dir("lonely")
        .checks(Check::OUTPUT)
        .run();
    // Outside a repository: no default remote, but URLs work.
    let outside: &[&[&str]] = &[
        &["ls-remote"],
        &["ls-remote", "--get-url"],
        &["ls-remote", "r", "dev"],
    ];
    for argv in outside {
        fx.compare(*argv).checks(Check::OUTPUT).run();
    }
}
