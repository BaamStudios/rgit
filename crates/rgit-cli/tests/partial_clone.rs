// SPDX-License-Identifier: MIT
//! Differential tests of partial clones (#24): object filters in
//! `rev-list`, missing and promisor objects, `clone`/`fetch --filter`,
//! promisor packs and fetching missing objects on demand.
//!
//! The server side (`upload-pack` and the `pack-objects` it runs) is the
//! reference git on both sides: `PATH` starts with its directory.

use rgit_difftest::{Check, Fixture, Normalizer};

mod common;
use common::servers::server_path;

fn commit(fx: Fixture, path: &str, text: &str) -> Fixture {
    fx.write(path, text)
        .git(["add", path])
        .git(["commit", "-q", "-m", path])
        .tick()
}

/// Two commits with files at several depths and a pattern blob for
/// `sparse:oid`.
fn repo() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    fx = commit(fx, "top", "top\n");
    fx = commit(fx, "dir1/a", "a\n");
    fx = commit(fx, "dir1/sub/b", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n");
    fx = commit(fx, "pattern", "dir1/\n");
    fx.git(["tag", "-m", "t", "tg"])
}

#[test]
fn rev_list_filters_same_as_git() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["rev-list", "--objects", "--filter=blob:none", "HEAD"][..],
        &["rev-list", "--objects", "--filter=blob:limit=10", "HEAD"],
        &["rev-list", "--objects", "--filter=blob:limit=1k", "HEAD"],
        &["rev-list", "--objects", "--filter=tree:0", "HEAD"],
        &["rev-list", "--objects", "--filter=tree:1", "HEAD"],
        &["rev-list", "--objects", "--filter=tree:2", "HEAD"],
        &["rev-list", "--objects", "--filter=tree:3", "HEAD"],
        &["rev-list", "--objects", "--filter=object:type=blob", "HEAD"],
        &["rev-list", "--objects", "--filter=object:type=tree", "HEAD"],
        &[
            "rev-list",
            "--objects",
            "--filter=object:type=commit",
            "HEAD",
        ],
        &["rev-list", "--objects", "--filter=object:type=tag", "tg"],
        &[
            "rev-list",
            "--objects",
            "--filter=object:type=blob",
            "--filter-provided-objects",
            "HEAD",
        ],
        &[
            "rev-list",
            "--objects",
            "--filter=sparse:oid=main:pattern",
            "HEAD",
        ],
        &[
            "rev-list",
            "--objects",
            "--filter=combine:tree:2+blob:none",
            "HEAD",
        ],
        &[
            "rev-list",
            "--objects",
            "--filter=tree:3",
            "--filter=blob:limit=5",
            "HEAD",
        ],
        &[
            "rev-list",
            "--objects",
            "--filter=blob:none",
            "HEAD",
            "HEAD:top",
        ],
        &[
            "rev-list",
            "--objects",
            "--filter=tree:0",
            "--filter-print-omitted",
            "HEAD~3",
        ],
        &["rev-list", "--objects", "--filter=tree:1", "HEAD^{tree}"],
        &[
            "rev-list",
            "--objects",
            "--count",
            "--filter=blob:none",
            "HEAD",
        ],
        &[
            "rev-list",
            "--objects",
            "--filter=blob:none",
            "--no-filter",
            "HEAD",
        ],
        // Errors.
        &["rev-list", "--filter=blob:none", "HEAD"],
        &["rev-list", "--objects", "--filter=blob:nope", "HEAD"],
        &["rev-list", "--objects", "--filter=tree:x", "HEAD"],
        &["rev-list", "--objects", "--filter=object:type=x", "HEAD"],
        &["rev-list", "--objects", "--filter=sparse:path=x", "HEAD"],
        &["rev-list", "--objects", "--filter=sparse:oid=nope", "HEAD"],
        &["rev-list", "--objects", "--filter=combine:", "HEAD"],
        &[
            "rev-list",
            "--objects",
            "--filter=combine:tree:2+blob~x",
            "HEAD",
        ],
        &["rev-list", "--objects", "--missing-only", "HEAD"],
        &[
            "rev-list",
            "--objects",
            "--missing=print",
            "--exclude-promisor-objects",
            "HEAD",
        ],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}

#[test]
fn rev_list_missing_objects_same_as_git() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    let tree = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD:dir1/sub"])).unwrap();
    let tree = tree.trim();
    let path = format!(".git/objects/{}/{}", &tree[..2], &tree[2..]);
    let mut fx = fx.remove(path);
    for args in [
        &["rev-list", "--objects", "HEAD"][..],
        &["rev-list", "--objects", "--missing=allow-any", "HEAD"],
        &["rev-list", "--objects", "--missing=print", "HEAD"],
        &["rev-list", "--objects", "--missing=print-info", "HEAD"],
        &[
            "rev-list",
            "--objects",
            "--missing=print",
            "--missing-only",
            "HEAD",
        ],
        &["rev-list", "--objects", "--missing=allow-promisor", "HEAD"],
        &["rev-list", "--objects", "--missing=print", tree],
        // `--missing-only` lists, it does not count (#743).
        &[
            "rev-list",
            "--objects",
            "--missing=print",
            "--missing-only",
            "--count",
            "HEAD",
        ],
        &[
            "rev-list",
            "--objects",
            "--missing=print",
            "--missing-only",
            "--disk-usage",
            "HEAD",
        ],
        &["rev-list", "-z", "--objects", "--missing=print", "HEAD"],
        &[
            "rev-list",
            "-z",
            "--objects",
            "--no-object-names",
            "--missing=print-info",
            "HEAD",
        ],
        &[
            "rev-list",
            "-z",
            "--objects",
            "--missing=print-info",
            "--missing-only",
            "HEAD",
        ],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}

/// A server allowing filters, its partial clone, then the files read on
/// demand.
#[test]
fn partial_clone_same_as_git() {
    let mut fx = repo().git(["config", "uploadpack.allowfilter", "1"]).git([
        "config",
        "uploadpack.allowanysha1inwant",
        "1",
    ]);
    if fx.is_skipped() {
        return;
    }
    let url = format!("file://{}", fx.work_dir().display());
    let path = server_path(&fx);
    // The pack names depend on the pack contents only.
    fx.compare([
        "clone",
        "-q",
        "--no-checkout",
        "--filter=blob:none",
        &url,
        "pc",
    ])
    .repo("pc")
    .env("PATH", &path)
    .skip(Check::Worktree)
    .run();
    for args in [
        // (The order of `?` lines follows git's hash table.)
        &[
            "-C",
            "pc",
            "rev-list",
            "--objects",
            "--missing=allow-any",
            "HEAD",
        ][..],
        &[
            "-C",
            "pc",
            "rev-list",
            "--objects",
            "--missing=print",
            "HEAD~3",
        ],
        &[
            "-C",
            "pc",
            "rev-list",
            "--objects",
            "--exclude-promisor-objects",
            "HEAD",
        ],
        &[
            "-C",
            "pc",
            "rev-list",
            "--objects",
            "--missing=allow-promisor",
            "HEAD",
        ],
        &["-C", "pc", "fsck"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
    // Reading a missing blob fetches it.
    fx.compare(["-C", "pc", "cat-file", "-p", "HEAD:dir1/a"])
        .env("PATH", &path)
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["--no-lazy-fetch", "-C", "pc", "cat-file", "-e", "HEAD:top"])
        .checks(Check::OUTPUT)
        .run();
    // The checkout fetches what it needs.
    fx.compare(["-C", "pc", "checkout", "-q", "main"])
        .env("PATH", &path)
        .repo("pc")
        .normalize_state(Normalizer::pack_names())
        .checks([
            Check::Stdout,
            Check::Stderr,
            Check::ExitCode,
            Check::Worktree,
        ])
        .run();
}

/// `fetch --filter` makes a remote a promisor remote.
#[test]
fn fetch_filter_registers_promisor_same_as_git() {
    let fx = repo()
        .git(["config", "uploadpack.allowfilter", "1"])
        .git(["init", "-q", "other"]);
    if fx.is_skipped() {
        return;
    }
    let url = format!("file://{}", fx.work_dir().display());
    let path = server_path(&fx);
    let mut fx = fx.git(["-C", "other", "remote", "add", "o", &url]);
    fx.compare(["-C", "other", "fetch", "-q", "--filter=blob:limit=1k", "o"])
        .env("PATH", &path)
        .repo("other")
        .checks([
            Check::Stdout,
            Check::Stderr,
            Check::ExitCode,
            Check::GitFiles,
        ])
        .run();
    fx.compare(["-C", "other", "fetch", "--filter=bogus", "o"])
        .env("PATH", &path)
        .checks(Check::OUTPUT)
        .run();
}

/// After a partial clone (#743): `rev-list --exclude-promisor-objects`
/// stops at the objects of promisor packs only, `pack-objects
/// --stdin-packs` refuses a promisor pack with
/// `--exclude-promisor-objects`, and a fetch without a filter stores its
/// objects like any other fetch.
#[test]
fn promisor_packs_after_partial_clone_same_as_git() {
    let fx = repo().git(["config", "uploadpack.allowfilter", "1"]).git([
        "config",
        "uploadpack.allowanysha1inwant",
        "1",
    ]);
    if fx.is_skipped() {
        return;
    }
    let url = format!("file://{}", fx.work_dir().display());
    let path = server_path(&fx);
    let mut fx = fx
        .git([
            "clone",
            "-q",
            "--no-checkout",
            "--filter=blob:none",
            &url,
            "pc",
        ])
        .git(["-C", "pc", "config", "maintenance.auto", "false"]);
    // A blob outside the promisor pack that a promisor tree names.
    let blob = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD:top"])).unwrap();
    let blob = blob.trim().to_owned();
    fx = fx
        .write("pc/top-copy", "top\n")
        .git(["-C", "pc", "hash-object", "-w", "top-copy"]);
    for args in [
        &[
            "-C",
            "pc",
            "rev-list",
            "--objects",
            "--exclude-promisor-objects",
            &blob,
        ][..],
        &[
            "-C",
            "pc",
            "rev-list",
            "--objects",
            "--exclude-promisor-objects",
            "HEAD^{tree}",
        ],
        &[
            "-C",
            "pc",
            "rev-list",
            "--objects",
            "--exclude-promisor-objects",
            "--all",
        ],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
    let pack_dir = fx.work_dir().join("pc/.git/objects/pack");
    let promisor = std::fs::read_dir(&pack_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.ends_with(".promisor"))
        .expect("a promisor pack");
    let pack = promisor.replace(".promisor", ".pack");
    fx.compare([
        "-C",
        "pc",
        "pack-objects",
        "--stdin-packs",
        "--exclude-promisor-objects",
        "--stdout",
    ])
    .stdin(format!("{pack}\n"))
    .checks(Check::OUTPUT)
    .run();
    // New history on the server, fetched without a filter.
    fx = commit(fx, "later", "later\n");
    fx.compare(["-C", "pc", "fetch", "-q", "--no-filter", "origin"])
        .env("PATH", &path)
        .repo("pc")
        .normalize_state(Normalizer::pack_names())
        .skip(Check::Worktree)
        .run();
}

/// `upload-pack` dies on `uploadpackfilter.*` values it cannot parse.
#[test]
fn upload_pack_filter_config_same_as_git() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    for config in [
        "uploadpackfilter.tree.maxdepth",
        "uploadpackfilter.tree.maxdepth=x",
        "uploadpackfilter.tree.maxdepth=-1",
        "uploadpackfilter.tree.maxdepth=5k",
        "uploadpackfilter.allow=maybe",
        "uploadpackfilter.blob:none.allow=maybe",
        "uploadpackfilter.blob:none.allow",
        "uploadpackfilter.Tree.maxDepth",
    ] {
        // (The advertisement names the agent: compare the errors.)
        fx.compare(["-c", config, "upload-pack", "."])
            .stdin("0000")
            .checks([Check::Stderr, Check::ExitCode])
            .run();
    }
}
