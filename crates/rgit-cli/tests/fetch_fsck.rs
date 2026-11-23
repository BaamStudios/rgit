// SPDX-License-Identifier: MIT
//! `fetch.fsckObjects` / `transfer.fsckObjects` with `fetch.fsck.<msg-id>`
//! on the client side of fetch and clone, against the reference git.

use rgit_difftest::{Check, Fixture};

/// A server repository `srv` whose branch `bad` holds a commit with a
/// broken committer email.
fn fixture() -> Fixture {
    let fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return fx;
    }
    let fx = fx
        .git(["init", "-q", "srv"])
        .write("srv/a", "a\n")
        .git(["-C", "srv", "add", "a"])
        .git(["-C", "srv", "commit", "-q", "-m", "one"]);
    let tree = String::from_utf8(fx.git_stdout(["-C", "srv", "rev-parse", "HEAD^{tree}"])).unwrap();
    let text = format!(
        "tree {}\nauthor A <a@b> 1 +0000\ncommitter Bad <bad@b 1 +0000\n\nbad\n",
        tree.trim()
    );
    let fx = fx.write("bad-commit", text);
    let id = String::from_utf8(fx.git_stdout([
        "-C",
        "srv",
        "hash-object",
        "-t",
        "commit",
        "-w",
        "--literally",
        "../bad-commit",
    ]))
    .unwrap();
    fx.git(["-C", "srv", "update-ref", "refs/heads/bad", id.trim()])
        .git(["init", "-q", "dst"])
}

#[test]
fn clone_and_fetch_check_objects() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let checks = [Check::ExitCode, Check::Stderr];
    for args in [
        &[
            "-c",
            "transfer.fsckObjects=true",
            "clone",
            "-q",
            "--no-local",
            "srv",
            "c1",
        ][..],
        &[
            "-c",
            "fetch.fsckObjects=true",
            "clone",
            "-q",
            "--no-local",
            "srv",
            "c2",
        ],
        &[
            "-c",
            "fetch.fsckObjects=true",
            "-c",
            "fetch.fsck.badEmail=warn",
            "clone",
            "-q",
            "--no-local",
            "srv",
            "c3",
        ],
        &[
            "-c",
            "fetch.fsckObjects=true",
            "-c",
            "fetch.fsck.badEmail=ignore",
            "clone",
            "-q",
            "--no-local",
            "srv",
            "c4",
        ],
        &[
            "-C",
            "dst",
            "-c",
            "fetch.fsckObjects=true",
            "fetch",
            "-q",
            "../srv",
            "bad",
        ],
        // fsck.<msg-id> does not apply to fetch.
        &[
            "-C",
            "dst",
            "-c",
            "fetch.fsckObjects=true",
            "-c",
            "fsck.badEmail=ignore",
            "fetch",
            "-q",
            "../srv",
            "bad",
        ],
        &[
            "-C",
            "dst",
            "-c",
            "transfer.fsckObjects=true",
            "-c",
            "fetch.fsckObjects=false",
            "fetch",
            "-q",
            "../srv",
            "bad",
        ],
    ] {
        fx.compare(args).checks(checks).run();
    }
    // fetch.fsck.skipList leaves the listed objects unchecked.
    let bad = fx.git_stdout(["-C", "srv", "rev-parse", "bad"]);
    let mut list = b"# skipped\n".to_vec();
    list.extend(bad);
    let mut fx = fx.write("skip", list);
    let skip = format!(
        "fetch.fsck.skipList={}",
        fx.work_dir().join("skip").display()
    );
    fx.compare([
        "-C",
        "dst",
        "-c",
        "fetch.fsckObjects=true",
        "-c",
        &skip,
        "fetch",
        "-q",
        "../srv",
        "bad",
    ])
    .checks(checks)
    .run();
}
