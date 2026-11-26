// SPDX-License-Identifier: MIT
//! Differential tests of the `promisor-remote` capability (#640) on the
//! client side: which advertised promisor remotes a clone accepts
//! (`promisor.acceptFromServer`, `promisor.acceptFromServerUrl`,
//! `promisor.checkFields`), what it writes to the configuration
//! (auto-created remotes, `promisor.storeFields`) and what it says; and
//! `cat-file --batch-command` `remote-object-info`.
//!
//! The server side is the reference git on both sides: `PATH` starts with
//! its directory.

use rgit_difftest::{Check, Fixture, Normalizer};

mod common;
use common::servers::server_path;

/// `srv`: one commit, filters allowed, advertising its promisor remotes
/// `lop` (a bare repository next to it) and `other`; `lop` with a filter
/// and a token the server sends.
fn fixture() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "srv"]);
    if fx.is_skipped() {
        return fx;
    }
    let lop = format!("file://{}/lop", fx.work_dir().display());
    let fx = fx
        .write("srv/a", "a\n")
        .git(["-C", "srv", "add", "a"])
        .git(["-C", "srv", "commit", "-q", "-m", "a"])
        .git(["init", "-q", "--bare", "lop"]);
    let mut fx = fx;
    for (k, v) in [
        ("uploadpack.allowfilter", "1"),
        ("promisor.advertise", "true"),
        ("promisor.sendFields", "partialCloneFilter, token"),
        ("remote.lop.url", lop.as_str()),
        ("remote.lop.promisor", "true"),
        ("remote.lop.partialCloneFilter", "blob:limit=8k"),
        ("remote.lop.token", "server-token"),
        ("remote.other.url", "https://invalid.invalid/x,y"),
        ("remote.other.partialCloneFilter", "blob:none"),
    ] {
        fx = fx.git(["-C", "srv", "config", k, v]);
    }
    fx
}

/// Clone `srv` with `config` (`-c` options) and compare output and the
/// new repository's configuration.
fn clone(fx: &mut Fixture, config: &[&str], filter: &str) {
    let path = server_path(fx);
    let url = format!("file://{}/srv", fx.work_dir().display());
    let mut argv: Vec<String> = vec!["clone".into(), "-q".into(), "--no-checkout".into()];
    for c in config {
        argv.push("-c".into());
        argv.push((*c).to_owned());
    }
    argv.push(format!("--filter={filter}"));
    argv.push(url);
    argv.push("cl".into());
    let work = fx.work_dir().display().to_string();
    fx.compare(&argv)
        .env("PATH", &path)
        .repo("cl")
        .normalize(Normalizer::replace(work, "<work>"))
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::GitFiles,
        ])
        .run();
}

#[test]
fn clone_accepts_promisor_remotes_like_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let lop = format!("remote.lop.url=file://{}/lop", fx.work_dir().display());
    let pattern = format!(
        "promisor.acceptFromServerUrl=file://{}/*",
        fx.work_dir().display()
    );
    let named = format!(
        "promisor.acceptFromServerUrl=cdn=file://{}/*",
        fx.work_dir().display()
    );
    let cases: Vec<Vec<&str>> = vec![
        vec!["promisor.acceptFromServer=All"],
        vec![
            "promisor.acceptFromServer=KnownName",
            "remote.lop.promisor=true",
            &lop,
        ],
        vec![
            "promisor.acceptFromServer=KnownUrl",
            "remote.lop.promisor=true",
            &lop,
        ],
        vec!["promisor.acceptFromServer=None", &pattern],
        vec!["promisor.acceptFromServer=None", &named],
        vec![&named, "remote.cdn.url=https://example.com/cdn"],
        vec![
            "promisor.acceptFromServer=All",
            "promisor.storeFields=partialCloneFilter,token",
            "remote.lop.partialCloneFilter=blob:none",
            "remote.lop.token=mine",
            &lop,
        ],
        vec![
            "promisor.acceptFromServer=All",
            "promisor.checkFields=token",
            "remote.lop.token=mine",
            &lop,
        ],
    ];
    for config in &cases {
        clone(&mut fx, config, "blob:none");
        fx = fx.remove("cl");
    }
    // `--filter=auto` takes the filters of the accepted remotes.
    clone(&mut fx, &["promisor.acceptFromServer=All"], "auto");
}

#[test]
fn remote_object_info_like_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx.git([
        "-C",
        "srv",
        "config",
        "transfer.advertiseObjectInfo",
        "true",
    ]);
    let path = server_path(&fx);
    let url = format!("file://{}/srv", fx.work_dir().display());
    let blob = String::from_utf8(fx.git_stdout(["-C", "srv", "rev-parse", "HEAD:a"])).unwrap();
    let blob = blob.trim();
    let tree = String::from_utf8(fx.git_stdout(["-C", "srv", "rev-parse", "HEAD^{tree}"])).unwrap();
    let tree = tree.trim();
    let unknown = "1111111111111111111111111111111111111111";
    let inputs = [
        format!("remote-object-info {url} {blob} {tree} {unknown}\ninfo {blob}\n"),
        format!("remote-object-info \"{url}\" {blob}\n"),
        format!("remote-object-info {url} {}\n", &blob[..7]),
        format!("remote-object-info {url} not-an-id\n"),
        format!("remote-object-info {url}\n"),
        "remote-object-info \n".to_owned(),
        "remote-object-info\n".to_owned(),
        format!("remote-object-info \"{url} {blob}\n"),
    ];
    let formats = [
        "--batch-command",
        "--batch-command=%(objectname) %(objectsize)",
        "--batch-command=%(objecttype)",
        "--batch-command=%(objectname)",
        "--batch-command=%(objectmode) %(objectsize:disk) %(rest) %(deltabase)",
    ];
    for input in &inputs {
        for format in formats {
            fx.compare(["-C", "srv", "cat-file", format])
                .stdin(input.as_bytes().to_vec())
                .env("PATH", &path)
                .checks(Check::OUTPUT)
                .run();
        }
    }
    let input = format!("remote-object-info {url} {blob}\n");
    fx.compare([
        "-c",
        "protocol.version=0",
        "-C",
        "srv",
        "cat-file",
        "--batch-command",
    ])
    .stdin(input.as_bytes().to_vec())
    .env("PATH", &path)
    .checks([Check::ExitCode, Check::Stdout])
    .run();
    let fx = fx.git([
        "-C",
        "srv",
        "config",
        "transfer.advertiseObjectInfo",
        "false",
    ]);
    let mut fx = fx;
    fx.compare(["-C", "srv", "cat-file", "--batch-command"])
        .stdin(input.into_bytes())
        .env("PATH", &path)
        .checks(Check::OUTPUT)
        .run();
}
