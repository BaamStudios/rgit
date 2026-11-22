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

/// A pkt-line with `payload`.
fn pkt(payload: &str) -> String {
    format!("{:04x}{payload}", payload.len() + 4)
}

/// Fake servers that print a v0 advertisement and exit without reading
/// (t5512, t5704): an empty repository in the standard form, several
/// `symref` capabilities, and a bare `symref` capability.
#[test]
fn ls_remote_canned_v0_advertisements() {
    let oid = "1234567890123456789012345678901234567890";
    let zero = "0".repeat(40);
    let empty = format!(
        "{}0000",
        pkt(&format!("{zero} capabilities^{{}}\0caps-go-here\n"))
    );
    let first = format!(
        "{oid} HEAD\0some a-fake-symref-cap \
         symref=refs/remotes/origin/HEAD:refs/remotes/origin/main symref=HEAD:refs/heads/main\n"
    );
    let symrefs = format!(
        "{}{}{}0000",
        pkt(&first),
        pkt(&format!("{oid} refs/heads/main\n")),
        pkt(&format!("{oid} refs/remotes/origin/HEAD\n")),
    );
    let bare = format!(
        "{}0000",
        pkt(&format!(
            "{oid} HEAD\0symref object-format=sha1 symref=HEAD:refs/heads/foo\n"
        ))
    );
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "r"])
        .write("empty.adv", &empty)
        .write("symrefs.adv", &symrefs)
        .write("bare.adv", &bare)
        .write_executable("serve", "#!/bin/sh\nexec cat \"$ADV\"\n");
    if fx.is_skipped() {
        return;
    }
    let serve = format!("--upload-pack={}", fx.work_dir().join("serve").display());
    for (adv, args) in [
        ("empty.adv", &["--exit-code"][..]),
        ("symrefs.adv", &["--symref"]),
        ("bare.adv", &["--symref"]),
    ] {
        let mut argv = vec!["ls-remote"];
        argv.extend_from_slice(args);
        argv.extend_from_slice(&[&serve, "."]);
        let path = fx.work_dir().join(adv);
        fx.compare(&argv)
            .in_dir("r")
            .env("ADV", path)
            .env("GIT_TEST_PROTOCOL_VERSION", "0")
            .checks(Check::OUTPUT)
            .run();
    }
}
