// SPDX-License-Identifier: MIT
//! SHA-256 repositories (`extensions.objectFormat=sha256`, #12), end to
//! end against the reference git:
//!
//! - the same commands in a SHA-256 repository give the same output,
//!   objects, references, reflogs and index with git and with rgit;
//! - a repository built by rgit alone passes `git fsck --strict`, and one
//!   built by git alone (packs, `packed-refs`, reftable, index version 4)
//!   reads the same with rgit;
//! - clone, fetch and push over `file://` and smart HTTP, and the error
//!   messages when the two ends use different hash algorithms;
//! - signatures go into the `gpgsig-sha256` header.

use std::path::Path;
use std::process::Command;

use rgit_difftest::{Check, Fixture};

mod common;
use common::servers::{server_path, start_http};

const RGIT: &str = env!("CARGO_BIN_EXE_rgit");

/// A fixture where new repositories are SHA-256 repositories.
fn fixture() -> Fixture {
    Fixture::new().rgit(RGIT).env("GIT_DEFAULT_HASH", "sha256")
}

/// `r`: a SHA-256 repository with two branches, an annotated tag and a
/// subdirectory.
fn repository() -> Fixture {
    fixture()
        .git(["init", "-q", "-b", "main", "r"])
        .write("r/a", "a\n")
        .write("r/d/b", "b\n")
        .git(["-C", "r", "add", "."])
        .git(["-C", "r", "commit", "-q", "-m", "one"])
        .tick()
        .write("r/a", "a\na2\n")
        .git(["-C", "r", "commit", "-q", "-a", "-m", "two"])
        .git(["-C", "r", "tag", "-a", "-m", "annotated", "v1"])
        .git(["-C", "r", "branch", "side", "HEAD~1"])
        .tick()
}

/// Run each command in `r` with both binaries and compare everything.
fn same(fx: &mut Fixture, commands: &[&[&str]]) {
    let mut failed = Vec::new();
    for args in commands {
        let argv: Vec<&str> = ["-C", "r"].iter().chain(args.iter()).copied().collect();
        if let Err(m) = fx.compare(argv).repo("r").try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

#[test]
fn object_format_is_reported() {
    let mut fx = repository();
    if fx.is_skipped() {
        return;
    }
    same(
        &mut fx,
        &[
            &["rev-parse", "--show-object-format"],
            &["rev-parse", "--show-object-format=storage"],
            &["rev-parse", "--show-object-format=input"],
            &["rev-parse", "--show-object-format=output"],
            &["rev-parse", "--show-object-format=bogus"],
            &["config", "extensions.objectformat"],
            &["config", "core.repositoryformatversion"],
        ],
    );
}

#[test]
fn reading_commands_same_as_git() {
    let mut fx = repository();
    if fx.is_skipped() {
        return;
    }
    same(
        &mut fx,
        &[
            &["rev-parse", "HEAD", "HEAD^{tree}", "HEAD:a", "v1", "v1^{}"],
            &["rev-parse", "--short", "HEAD"],
            &["rev-parse", "--short=40", "HEAD"],
            &["cat-file", "-p", "HEAD"],
            &["cat-file", "-p", "v1"],
            &["cat-file", "-p", "HEAD^{tree}"],
            &["cat-file", "-t", "HEAD:d"],
            &["cat-file", "--batch-all-objects", "--batch-check"],
            &["ls-tree", "-r", "HEAD"],
            &["ls-tree", "--abbrev=10", "HEAD"],
            &["ls-files", "-s"],
            &["log", "--format=%H %T %P %h %t %p"],
            &["log", "--format=raw", "-p", "--full-index"],
            &["log", "--oneline", "--decorate", "--all"],
            &["show", "-s", "--format=fuller", "v1"],
            &["diff", "--full-index", "HEAD~1", "HEAD"],
            &["diff", "--raw", "--no-abbrev", "HEAD~1", "HEAD"],
            &["diff", "--raw", "--abbrev", "HEAD~1", "HEAD"],
            &["diff-tree", "-r", "HEAD"],
            &["rev-list", "--objects", "--all"],
            &["for-each-ref"],
            &[
                "for-each-ref",
                "--format=%(objectname) %(objectname:short) %(*objectname)",
            ],
            &["show-ref", "-d"],
            &["describe", "HEAD"],
            &["name-rev", "--all"],
            &["merge-base", "main", "side"],
            &["reflog"],
            &["fsck", "--strict"],
            &["count-objects", "-v"],
            &["status"],
            &["blame", "-l", "a"],
            &["blame", "a"],
            // An object id of the other algorithm's length is no object id.
            &["rev-parse", "da39a3ee5e6b4b0d3255bfef95601890afd80709"],
            &["cat-file", "-t", "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"],
        ],
    );
}

#[test]
fn writing_commands_same_as_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    fx.compare(["init", "-b", "main", "r"]).repo("r").run();
    let mut fx = fx
        .write("r/a", "a\n")
        .write("r/d/b", "b\n")
        .write_executable("r/x", "#!/bin/sh\n");
    same(
        &mut fx,
        &[
            &["add", "."],
            &["write-tree"],
            &["commit", "-m", "one"],
            &["tag", "-a", "-m", "annotated", "v1"],
            &["tag", "light"],
            &["branch", "side"],
            &["update-ref", "refs/heads/other", "HEAD"],
            &["symbolic-ref", "refs/heads/sym", "refs/heads/main"],
        ],
    );
    let mut fx = fx.tick().write("r/a", "a\nmore\n").write("r/new", "new\n");
    same(
        &mut fx,
        &[
            &["add", "-A"],
            &["commit", "-m", "two"],
            &["checkout", "side"],
            &["mv", "d/b", "moved"],
            &["commit", "-m", "side"],
            &["checkout", "main"],
            &["merge", "--no-edit", "side"],
            &["cherry-pick", "-x", "HEAD^2"],
            &["revert", "--no-edit", "HEAD"],
            &["reset", "--hard", "HEAD~1"],
            &["rm", "x"],
            &["stash"],
            &["stash", "pop"],
            &["commit", "--amend", "-m", "amended"],
            &["rebase", "side"],
            &["pack-refs", "--all"],
            &["update-ref", "-d", "refs/heads/other"],
            &["branch", "-m", "side", "renamed"],
            &["tag", "-d", "light"],
            &["repack", "-a", "-d", "-q"],
            &["prune-packed"],
            &["fsck", "--strict"],
            &["gc", "-q"],
            &["reflog", "expire", "--expire=now", "--all"],
            &["fsck", "--strict", "--full"],
            &["count-objects", "-v"],
        ],
    );
}

#[test]
fn plumbing_same_as_git() {
    let mut fx = repository();
    if fx.is_skipped() {
        return;
    }
    let cmp = |fx: &mut Fixture, args: &[&str], stdin: &[u8]| {
        let argv: Vec<&str> = ["-C", "r"].iter().chain(args.iter()).copied().collect();
        fx.compare(argv).repo("r").stdin(stdin.to_vec()).run();
    };
    cmp(&mut fx, &["hash-object", "--stdin"], b"blob\n");
    cmp(&mut fx, &["hash-object", "-w", "--stdin"], b"blob\n");
    cmp(&mut fx, &["hash-object", "-t", "tree", "--stdin"], b"");
    let tree = String::from_utf8(fx.git_stdout(["-C", "r", "rev-parse", "HEAD^{tree}"])).unwrap();
    let head = String::from_utf8(fx.git_stdout(["-C", "r", "rev-parse", "HEAD"])).unwrap();
    let blob = String::from_utf8(fx.git_stdout(["-C", "r", "rev-parse", "HEAD:a"])).unwrap();
    let (tree, head, blob) = (tree.trim(), head.trim(), blob.trim());
    assert_eq!(head.len(), 64);
    cmp(
        &mut fx,
        &["commit-tree", tree, "-p", head, "-m", "by hand"],
        b"",
    );
    cmp(
        &mut fx,
        &["mktree"],
        format!("100644 blob {blob}\tfile\n040000 tree {tree}\tdir\n").as_bytes(),
    );
    cmp(
        &mut fx,
        &["mktag"],
        format!("object {head}\ntype commit\ntag t\ntagger T <t@example.com> 1 +0000\n\nm\n")
            .as_bytes(),
    );
    cmp(
        &mut fx,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{blob},cached"),
        ],
        b"",
    );
    cmp(
        &mut fx,
        &["update-ref", "--stdin"],
        format!("create refs/heads/new {head}\nupdate refs/heads/side {head}\n").as_bytes(),
    );
    cmp(
        &mut fx,
        &["cat-file", "--batch"],
        format!("{blob}\nHEAD\nmissing\n").as_bytes(),
    );
    cmp(&mut fx, &["read-tree", "--empty"], b"");
    cmp(&mut fx, &["read-tree", "HEAD"], b"");
    cmp(&mut fx, &["update-index", "--index-version", "4"], b"");
    cmp(&mut fx, &["ls-files", "-s"], b"");
    cmp(&mut fx, &["update-index", "--index-version", "2"], b"");
    cmp(&mut fx, &["diff-index", "--cached", "HEAD"], b"");
    // Packs: the names, trailers and index tables hold 32-byte ids.
    cmp(&mut fx, &["repack", "-a", "-d", "-q"], b"");
    let pack_dir = fx.work_dir().join("r/.git/objects/pack");
    let mut idx = None;
    for entry in std::fs::read_dir(&pack_dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".idx") {
            // pack-<64 hex>.idx
            assert_eq!(name.len(), "pack-.idx".len() + 64, "{name}");
            idx = Some(format!(".git/objects/pack/{name}"));
        }
    }
    let idx = idx.expect("a pack index");
    let pack = idx.replace(".idx", ".pack");
    cmp(&mut fx, &["verify-pack", &idx], b"");
    let idx_data = std::fs::read(fx.work_dir().join("r").join(&idx)).unwrap();
    cmp(&mut fx, &["show-index"], &idx_data);
    let data = std::fs::read(fx.work_dir().join("r").join(&pack)).unwrap();
    let mut fx = fx.git(["init", "-q", "--bare", "unpacked.git"]).git([
        "init",
        "-q",
        "--bare",
        "indexed.git",
    ]);
    fx.compare(["-C", "unpacked.git", "unpack-objects", "-q"])
        .stdin(data.clone())
        .repo("unpacked.git")
        .skip(Check::Worktree)
        .run();
    fx.compare(["-C", "indexed.git", "index-pack", "--stdin"])
        .stdin(data)
        .repo("indexed.git")
        .skip(Check::Worktree)
        .run();
    fx.compare(["-C", "indexed.git", "fsck", "--strict"])
        .output_only()
        .run();
    // Outside a repository the format has to be named.
    fx.compare([
        "index-pack",
        "--object-format=sha256",
        "-o",
        "out.idx",
        &format!("r/{pack}"),
    ])
    .output_only()
    .run();
}

/// An isolated command: fixed identity and dates, no user configuration.
fn cmd(program: &str, dir: &Path, home: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(program);
    c.current_dir(dir)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("TZ", "UTC")
        .env("LC_ALL", "C");
    c
}

/// Run and return standard output; the command has to succeed.
fn ok(program: &str, dir: &Path, home: &Path, args: &[&str]) -> String {
    let o = cmd(program, dir, home, args).output().unwrap();
    assert!(
        o.status.success(),
        "{program} {args:?}: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn have_git() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Build the same small history with `program` in `dir`: loose objects,
/// a merge, an annotated tag, packed references and a pack.
fn build(program: &str, dir: &Path, home: &Path, ref_format: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let run = |args: &[&str]| ok(program, dir, home, args);
    run(&[
        "init",
        "-q",
        "-b",
        "main",
        "--object-format=sha256",
        &format!("--ref-format={ref_format}"),
    ]);
    for i in 0..4 {
        std::fs::write(dir.join(format!("f{i}")), format!("content {i}\n")).unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", &format!("commit {i}")]);
    }
    run(&["tag", "-a", "-m", "annotated", "v1"]);
    run(&["checkout", "-q", "-b", "side", "HEAD~2"]);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/s"), "side\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "side"]);
    run(&["checkout", "-q", "main"]);
    run(&["merge", "-q", "--no-edit", "side"]);
    run(&["pack-refs", "--all"]);
    run(&["repack", "-a", "-d", "-q"]);
    std::fs::write(dir.join("f0"), "changed\n").unwrap();
    run(&["commit", "-q", "-a", "-m", "loose on top"]);
    run(&["branch", "topic"]);
}

/// A repository made by rgit alone is a valid SHA-256 repository for git,
/// and one made by git alone reads the same with rgit.
#[test]
fn repositories_are_interchangeable() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    for ref_format in ["files", "reftable"] {
        let by_rgit = tmp.path().join(format!("rgit-{ref_format}"));
        let by_git = tmp.path().join(format!("git-{ref_format}"));
        build(RGIT, &by_rgit, &home, ref_format);
        build("git", &by_git, &home, ref_format);

        // Same content, so the same object ids.
        let head = ok("git", &by_git, &home, &["rev-parse", "HEAD"]);
        assert_eq!(head.trim().len(), 64);
        assert_eq!(ok(RGIT, &by_rgit, &home, &["rev-parse", "HEAD"]), head);

        // git accepts what rgit wrote.
        let fsck = cmd("git", &by_rgit, &home, &["fsck", "--strict", "--full"])
            .output()
            .unwrap();
        assert!(
            fsck.status.success() && fsck.stderr.is_empty(),
            "git fsck of the rgit repository ({ref_format}): {}",
            String::from_utf8_lossy(&fsck.stderr)
        );
        assert_eq!(
            ok(
                "git",
                &by_rgit,
                &home,
                &["rev-parse", "--show-object-format"]
            ),
            "sha256\n"
        );
        assert_eq!(ok("git", &by_rgit, &home, &["status", "--porcelain"]), "");

        // Each reads both repositories alike.
        let queries: &[&[&str]] = &[
            &["rev-parse", "--show-object-format", "--show-ref-format"],
            &["log", "--all", "--format=%H %T %P %s"],
            &["for-each-ref"],
            &["cat-file", "--batch-all-objects", "--batch-check"],
            &["rev-list", "--objects", "--all"],
            &["ls-files", "-s"],
            &["reflog", "show", "--format=%H %gs", "main"],
            &["fsck", "--strict"],
            &["status", "--porcelain"],
        ];
        for q in queries {
            let expected = ok("git", &by_git, &home, q);
            assert_eq!(ok(RGIT, &by_git, &home, q), expected, "rgit {q:?} in git's");
            assert_eq!(
                ok("git", &by_rgit, &home, q),
                expected,
                "git {q:?} in rgit's"
            );
            assert_eq!(
                ok(RGIT, &by_rgit, &home, q),
                expected,
                "rgit {q:?} in rgit's"
            );
        }

        // And each continues the other's repository.
        std::fs::write(by_git.join("more"), "more\n").unwrap();
        ok(RGIT, &by_git, &home, &["add", "more"]);
        ok(RGIT, &by_git, &home, &["commit", "-q", "-m", "by rgit"]);
        ok(RGIT, &by_git, &home, &["gc", "-q"]);
        std::fs::write(by_rgit.join("more"), "more\n").unwrap();
        ok("git", &by_rgit, &home, &["add", "more"]);
        ok("git", &by_rgit, &home, &["commit", "-q", "-m", "by rgit"]);
        ok("git", &by_rgit, &home, &["gc", "-q"]);
        for dir in [&by_git, &by_rgit] {
            ok("git", dir, &home, &["fsck", "--strict", "--full"]);
            ok(RGIT, dir, &home, &["fsck", "--strict"]);
        }
        assert_eq!(
            ok(RGIT, &by_rgit, &home, &["rev-parse", "HEAD"]),
            ok("git", &by_git, &home, &["rev-parse", "HEAD"])
        );
    }
}

/// `src`: a SHA-256 repository to clone; `sha1`: a SHA-1 one.
fn remotes() -> Fixture {
    Fixture::new()
        .rgit(RGIT)
        .git(["init", "-q", "-b", "main", "--object-format=sha256", "src"])
        .write("src/a", "a\n")
        .git(["-C", "src", "add", "."])
        .git(["-C", "src", "commit", "-q", "-m", "one"])
        .tick()
        .write("src/d/b", "b\n")
        .git(["-C", "src", "add", "."])
        .git(["-C", "src", "commit", "-q", "-m", "two"])
        .git(["-C", "src", "branch", "side", "HEAD~1"])
        .git(["-C", "src", "tag", "-a", "-m", "annotated", "v1"])
        .git([
            "init",
            "-q",
            "-b",
            "main",
            "--object-format=sha256",
            "empty",
        ])
        .git(["init", "-q", "-b", "main", "--object-format=sha1", "sha1"])
        .git(["-C", "sha1", "commit", "-q", "--allow-empty", "-m", "sha1"])
        .tick()
}

/// Compare `args` with the reference git as the server on both sides.
fn transport(fx: &mut Fixture, args: &[&str], dir: &str) {
    let path = server_path(fx);
    let c = fx.compare(args).repo(dir).env("PATH", path);
    if dir.ends_with(".git") {
        c.skip(Check::Worktree).run();
    } else {
        c.run();
    }
}

#[test]
fn clone_takes_over_the_object_format() {
    let mut fx = remotes();
    if fx.is_skipped() {
        return;
    }
    let url = format!("file://{}/src", fx.work_dir().display());
    let cases: &[(&[&str], &str)] = &[
        (&["clone", "--no-local", "src", "c1"], "c1"),
        (&["clone", &url, "c2"], "c2"),
        (&["clone", "--bare", &url, "c3.git"], "c3.git"),
        (&["clone", "--mirror", &url, "c4.git"], "c4.git"),
        (&["-c", "protocol.version=0", "clone", &url, "c5"], "c5"),
        (&["-c", "protocol.version=1", "clone", &url, "c6"], "c6"),
        (&["clone", "--no-local", "empty", "c7"], "c7"),
        (
            &[
                "-c",
                "protocol.version=0",
                "clone",
                "--no-local",
                "empty",
                "c8",
            ],
            "c8",
        ),
        (&["clone", "--depth=1", &url, "c9"], "c9"),
        (
            &["clone", "-b", "side", "--single-branch", &url, "c10"],
            "c10",
        ),
    ];
    for (args, dir) in cases {
        transport(&mut fx, args, dir);
        fx.compare(["-C", dir, "rev-parse", "--show-object-format"])
            .output_only()
            .run();
        fx.compare(["-C", dir, "fsck", "--strict"])
            .output_only()
            .run();
    }
    // `GIT_DEFAULT_HASH` does not decide: the remote does.
    let sha1 = format!("file://{}/sha1", fx.work_dir().display());
    let mut fx = fx.env("GIT_DEFAULT_HASH", "sha256");
    transport(&mut fx, &["clone", &sha1, "d1"], "d1");
    transport(&mut fx, &["clone", &url, "d2"], "d2");
    for dir in ["d1", "d2"] {
        fx.compare(["-C", dir, "rev-parse", "--show-object-format"])
            .output_only()
            .run();
    }
}

#[test]
fn clone_into_reftable() {
    let mut fx = remotes();
    if fx.is_skipped() {
        return;
    }
    let url = format!("file://{}/src", fx.work_dir().display());
    let path = server_path(&fx);
    // The tables differ in their random names; compare what they hold.
    fx.compare(["clone", "--ref-format=reftable", &url, "c1"])
        .env("PATH", &path)
        .output_only()
        .run();
    fx.compare(["clone", "--ref-format=reftable", "--bare", &url, "c2.git"])
        .env("PATH", &path)
        .output_only()
        .run();
    for dir in ["c1", "c2.git"] {
        for args in [
            &["rev-parse", "--show-object-format", "--show-ref-format"][..],
            &["for-each-ref"],
            &["fsck", "--strict"],
            &["log", "--format=%H", "--all"],
        ] {
            let argv: Vec<&str> = ["-C", dir].iter().chain(args.iter()).copied().collect();
            fx.compare(argv).output_only().run();
        }
    }
}

#[test]
fn fetch_and_push_file_url() {
    let mut fx = remotes()
        .git(["clone", "-q", "--no-local", "src", "cl"])
        .git(["init", "-q", "--bare", "--object-format=sha256", "dst.git"])
        .write("src/new", "new\n")
        .git(["-C", "src", "add", "."])
        .git(["-C", "src", "commit", "-q", "-m", "three"])
        .git(["-C", "src", "tag", "v2"]);
    if fx.is_skipped() {
        return;
    }
    let dst = format!("file://{}/dst.git", fx.work_dir().display());
    transport(&mut fx, &["-C", "cl", "fetch"], "cl");
    transport(
        &mut fx,
        &["-C", "cl", "merge", "--ff-only", "origin/main"],
        "cl",
    );
    transport(
        &mut fx,
        &["-C", "cl", "-c", "protocol.version=0", "fetch", "--tags"],
        "cl",
    );
    transport(&mut fx, &["-C", "cl", "ls-remote", "origin"], "cl");
    transport(
        &mut fx,
        &[
            "-C",
            "cl",
            "-c",
            "protocol.version=0",
            "ls-remote",
            "origin",
        ],
        "cl",
    );
    // A full object name has 64 digits here; 40 digits name a reference.
    let oid = String::from_utf8(fx.git_stdout(["-C", "src", "rev-parse", "side"])).unwrap();
    let oid = oid.trim();
    let short = "a".repeat(40);
    transport(&mut fx, &["-C", "cl", "fetch", "origin", oid], "cl");
    transport(
        &mut fx,
        &[
            "-C",
            "cl",
            "fetch",
            "origin",
            &format!("{oid}:refs/heads/by-oid"),
        ],
        "cl",
    );
    transport(
        &mut fx,
        &[
            "-C",
            "cl",
            "-c",
            "protocol.version=0",
            "fetch",
            "origin",
            oid,
        ],
        "cl",
    );
    transport(&mut fx, &["-C", "cl", "fetch", "origin", &short], "cl");
    transport(
        &mut fx,
        &[
            "-C",
            "cl",
            "fetch",
            "origin",
            "refs/heads/*:refs/x/*",
            &format!("^{oid}"),
        ],
        "cl",
    );
    transport(
        &mut fx,
        &[
            "-C",
            "cl",
            "fetch",
            "origin",
            "refs/heads/*:refs/y/*",
            &format!("^{short}"),
        ],
        "cl",
    );
    for args in [
        &["-C", "cl", "push", &dst, "main"][..],
        &["-C", "cl", "push", &dst, "main:copy", "v1", "v2"],
        &["-C", "cl", "push", &dst, ":copy"],
        &[
            "-C",
            "cl",
            "push",
            "--porcelain",
            &dst,
            "side:refs/heads/side",
        ],
    ] {
        let path = server_path(&fx);
        fx.compare(args)
            .env("PATH", path)
            .repo("dst.git")
            .skip(Check::Worktree)
            .run();
    }
    fx.compare(["-C", "dst.git", "fsck", "--strict"])
        .output_only()
        .run();
}

/// Both directions over smart HTTP, with the reference `http-backend`.
#[test]
fn smart_http() {
    let fx = remotes()
        .git(["init", "-q", "--bare", "--object-format=sha256", "dst.git"])
        .git(["-C", "dst.git", "config", "http.receivepack", "true"])
        .git(["-C", "src", "config", "http.receivepack", "true"]);
    if fx.is_skipped() {
        return;
    }
    let base = start_http(&fx);
    let mut fx = fx;
    let src = format!("{base}/src");
    let dst = format!("{base}/dst.git");
    transport(&mut fx, &["clone", &src, "h1"], "h1");
    transport(
        &mut fx,
        &["-c", "protocol.version=0", "clone", &src, "h2"],
        "h2",
    );
    transport(
        &mut fx,
        &["clone", "--bare", &format!("{base}/empty"), "h3.git"],
        "h3.git",
    );
    let mut fx = fx
        .write("src/new", "new\n")
        .git(["-C", "src", "add", "."])
        .git(["-C", "src", "commit", "-q", "-m", "three"]);
    transport(&mut fx, &["-C", "h1", "fetch"], "h1");
    transport(&mut fx, &["-C", "h1", "ls-remote"], "h1");
    for args in [
        &["-C", "h1", "push", &dst, "origin/main:refs/heads/main"][..],
        &["-C", "h1", "push", &dst, "v1", "main:other"],
        &[
            "-C",
            "h1",
            "-c",
            "protocol.version=0",
            "push",
            &dst,
            ":other",
        ],
    ] {
        let path = server_path(&fx);
        fx.compare(args)
            .env("PATH", path)
            .repo("dst.git")
            .skip(Check::Worktree)
            .run();
    }
    fx.compare(["-C", "dst.git", "fsck", "--strict"])
        .output_only()
        .run();
}

/// The two ends use different algorithms: nothing is transferred, and the
/// message is git's. The server's own complaint about the closed
/// connection may come before or after it, so only the client's line is
/// compared.
#[test]
fn mixed_algorithms_are_refused() {
    let mut fx = remotes()
        .git(["clone", "-q", "--no-local", "src", "cl"])
        .git(["init", "-q", "--bare", "--object-format=sha1", "sha1.git"]);
    if fx.is_skipped() {
        return;
    }
    let src = format!("file://{}/src", fx.work_dir().display());
    let cases: &[(&str, &[&str], &str)] = &[
        // SHA-256 client, SHA-1 server.
        (
            "cl",
            &["fetch", "../sha1"],
            "fatal: mismatched algorithms: client sha256; server sha1",
        ),
        (
            "cl",
            &["-c", "protocol.version=0", "fetch", "../sha1"],
            "fatal: Server does not support this repository's object format",
        ),
        (
            "cl",
            &["-c", "protocol.version=1", "fetch", "../sha1"],
            "fatal: Server does not support this repository's object format",
        ),
        (
            "cl",
            &["push", "../sha1.git", "main"],
            "fatal: the receiving end does not support this repository's hash algorithm",
        ),
        (
            "cl",
            &["pull", "../sha1"],
            "fatal: mismatched algorithms: client sha256; server sha1",
        ),
        // SHA-1 client, SHA-256 server.
        (
            "sha1",
            &["fetch", &src],
            "fatal: mismatched algorithms: client sha1; server sha256",
        ),
        (
            "sha1",
            &["-c", "protocol.version=0", "fetch", &src],
            "fatal: Server does not support this repository's object format",
        ),
        (
            "sha1",
            &["push", "../src", "main:from-sha1"],
            "fatal: the receiving end does not support this repository's hash algorithm",
        ),
    ];
    for (dir, args, message) in cases {
        let argv: Vec<&str> = ["-C", dir].iter().chain(args.iter()).copied().collect();
        let path = server_path(&fx);
        let out = fx
            .compare(&argv)
            .env("PATH", path)
            .repo(*dir)
            .skip(Check::Stderr)
            .run()
            .unwrap();
        for (who, o) in [("git", &out.git), ("rgit", &out.rgit)] {
            let err = String::from_utf8_lossy(&o.stderr);
            assert!(err.lines().any(|l| l == *message), "{who} {argv:?}: {err}");
        }
    }
    // Listing needs no local repository format.
    transport(&mut fx, &["-C", "sha1", "ls-remote", "../src"], "sha1");
    transport(&mut fx, &["-C", "cl", "ls-remote", "../sha1"], "cl");
}

/// A nested repository of the other hash algorithm cannot become a
/// gitlink: `add`, `update-index` and `submodule add` refuse it.
#[test]
fn submodule_of_another_algorithm() {
    for (outer, inner) in [("sha256", "sha1"), ("sha1", "sha256")] {
        let outer_format = format!("--object-format={outer}");
        let inner_format = format!("--object-format={inner}");
        let mut fx = Fixture::new()
            .rgit(RGIT)
            .git(["init", "-q", "-b", "main", &outer_format, "r"])
            .write("r/a", "a\n")
            .git(["-C", "r", "add", "a"])
            .git(["-C", "r", "commit", "-q", "-m", "a"])
            .git(["init", "-q", "-b", "main", &inner_format, "r/sub"])
            .git(["-C", "r/sub", "commit", "-q", "--allow-empty", "-m", "s"])
            .git(["init", "-q", "-b", "main", &inner_format, "r/unborn"])
            .git(["init", "-q", "-b", "main", &outer_format, "r/same"])
            .git(["-C", "r/same", "commit", "-q", "--allow-empty", "-m", "s"])
            .write("r/other", "other\n");
        if fx.is_skipped() {
            return;
        }
        same(
            &mut fx,
            &[
                &["add", "sub"],
                &["add", "-n", "sub"],
                &["add", "--ignore-errors", "sub", "other"],
                &["add", "unborn"],
                &["update-index", "--add", "sub"],
                &["update-index", "sub"],
                &["update-index", "--add", "unborn"],
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    "../x",
                    "sub",
                ],
                &["status", "--short"],
                &["add", "--no-warn-embedded-repo", "same"],
                &["ls-files", "-s"],
            ],
        );
    }
}

/// Stands in for gpg: signs with a fixed signature, verifies with `GOODSIG`.
const GPG_STUB: &str = "#!/bin/sh\n\
case \"$*\" in\n\
*-bsau*) cat >/dev/null; echo '[GNUPG:] SIG_CREATED D 1 8 00 1 X' >&2;\n\
  printf -- '-----BEGIN PGP SIGNATURE-----\\n\\nc2ln\\n-----END PGP SIGNATURE-----\\n';;\n\
*) cat >/dev/null; printf '[GNUPG:] NEWSIG\\n[GNUPG:] GOODSIG K1 N <e>\\n[GNUPG:] TRUST_FULLY 0\\n';\n\
  echo 'gpg: Good signature' >&2;;\n\
esac\n";

#[test]
fn signatures_use_the_sha256_header() {
    let mut fx = fixture()
        .git(["init", "-q", "-b", "main", "r"])
        .write_executable("r/gpg-stub", GPG_STUB)
        .write("r/.git/info/exclude", "gpg-stub\n")
        .git(["-C", "r", "config", "gpg.program", "./gpg-stub"])
        .git(["-C", "r", "commit", "-q", "--allow-empty", "-m", "unsigned"])
        .tick();
    if fx.is_skipped() {
        return;
    }
    same(
        &mut fx,
        &[
            &["commit", "--allow-empty", "-S", "-m", "signed"],
            &["cat-file", "-p", "HEAD"],
            &["verify-commit", "-v", "HEAD"],
            &["log", "--format=%H %G? %GS", "-2"],
            &["tag", "-s", "-m", "signed tag", "st"],
            &["verify-tag", "-v", "st"],
            &["checkout", "-q", "-b", "side", "HEAD~1"],
            &["commit", "--allow-empty", "-m", "side"],
            &["tag", "-s", "-m", "merged tag", "mt"],
            &["checkout", "-q", "main"],
            &["merge", "--no-ff", "--no-edit", "mt"],
            &["cat-file", "-p", "HEAD"],
            &["fsck", "--strict"],
        ],
    );
    let commit = String::from_utf8(fx.git_stdout(["-C", "r", "cat-file", "-p", "HEAD~1"])).unwrap();
    assert!(
        commit.contains("\ngpgsig-sha256 -----BEGIN PGP SIGNATURE-----\n"),
        "{commit}"
    );
    assert!(!commit.contains("\ngpgsig -----BEGIN"), "{commit}");
}
