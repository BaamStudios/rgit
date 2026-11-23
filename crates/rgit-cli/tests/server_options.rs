// SPDX-License-Identifier: MIT
//! Differential tests of server options (`-o`, `remote.<name>.serverOption`)
//! with the legacy protocol and invalid configuration, and of clone
//! defaults (t5702): an empty repository's object format and the branch a
//! clone checks out when the server does not advertise `HEAD`.

use rgit_difftest::{Check, Fixture, Normalizer};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "r"])
        .git(["-C", "r", "commit", "-q", "--allow-empty", "-m", "a"])
        .git(["clone", "-q", "r", "c"])
}

#[test]
fn server_options_need_protocol_v2() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let url = format!("file://{}/r", fx.work_dir().display());
    let cases: Vec<Vec<&str>> = vec![
        vec![
            "-c",
            "protocol.version=0",
            "ls-remote",
            "-o",
            "hello",
            &url,
            "main",
        ],
        vec!["-c", "remote.origin.serverOption", "ls-remote", &url],
        // Configured options are fine with the legacy protocol.
        vec![
            "-c",
            "protocol.version=0",
            "-c",
            "remote.origin.serverOption=foo",
            "ls-remote",
            "origin",
        ],
    ];
    for argv in &cases {
        fx.compare(argv)
            .in_dir("c")
            .env("GIT_TEST_PROTOCOL_VERSION", "0")
            // The server notices the hang-up at its own pace.
            .normalize_stderr(Normalizer::drop_lines("remote end hung up"))
            .checks(Check::OUTPUT)
            .run();
    }
}

#[test]
fn invalid_server_option_config_fails_clone() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let url = format!("file://{}/r", fx.work_dir().display());
    fx.compare([
        "-c",
        "protocol.version=2",
        "-c",
        "remote.origin.serverOption",
        "clone",
        &url,
        "x",
    ])
    .checks(Check::OUTPUT)
    .run();
}

#[test]
fn clone_of_an_empty_repository_keeps_its_object_format() {
    let fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit")).git([
        "init",
        "-q",
        "--object-format=sha256",
        "e",
    ]);
    if fx.is_skipped() {
        return;
    }
    let url = format!("file://{}/e", fx.work_dir().display());
    let mut fx = fx.same(["clone", "-q", "e", "d1"]);
    fx.compare(["clone", "-q", &url, "d2"])
        .checks(Check::OUTPUT)
        .run();
    let fx = fx.same(["clone", "-q", &url, "d3"]);
    let format = fx.git_stdout(["-C", "d3", "rev-parse", "--show-object-format"]);
    assert_eq!(String::from_utf8_lossy(&format).trim(), "sha256");
}

#[test]
fn clone_without_advertised_head_uses_the_default_branch() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "p"])
        .git(["-C", "p", "config", "lsrefs.unborn", "ignore"])
        .git(["-C", "p", "checkout", "-q", "-b", "branchwithstuff"])
        .write("p/stuff.t", "s\n")
        .git(["-C", "p", "add", "stuff.t"])
        .git(["-C", "p", "commit", "-q", "-m", "stuff"])
        .git([
            "-C",
            "p",
            "symbolic-ref",
            "HEAD",
            "refs/heads/mydefaultbranch",
        ]);
    if fx.is_skipped() {
        return;
    }
    let url = format!("file://{}/p", fx.work_dir().display());
    fx.compare([
        "-c",
        "init.defaultBranch=branchwithstuff",
        "-c",
        "protocol.version=2",
        "clone",
        "-q",
        &url,
        "child",
    ])
    .env("GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME", "")
    .run();
}
