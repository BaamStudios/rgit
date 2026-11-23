// SPDX-License-Identifier: MIT
//! `rgit fsck` against the reference git: exit code, stdout and stderr for
//! healthy repositories, dangling and unreachable objects, missing and
//! corrupt objects, object format findings with `fsck.<msg-id>`, broken
//! references, explicit heads, `--name-objects`, `--connectivity-only` and
//! `--lost-found`.

use std::path::Path;

use rgit_difftest::{Check, Fixture, Normalizer};

const OUTPUT: [Check; 3] = [Check::ExitCode, Check::Stdout, Check::Stderr];

/// git prints missing, dangling and unreachable objects in the order of
/// its object hash table, which depends on when objects were first met;
/// compare them sorted.
fn sorted() -> Normalizer {
    Normalizer::new("sorted lines", |s: &str| {
        let mut lines: Vec<&str> = s.lines().collect();
        lines.sort_unstable();
        lines.join("\n")
    })
}

fn fixture() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

fn commit(fx: Fixture, file: &str, content: &str) -> Fixture {
    fx.write(file, content)
        .git(["add", file])
        .git(["commit", "-q", "-m", file])
        .tick()
}

/// A small history with a subdirectory, an annotated and a light tag.
fn history() -> Fixture {
    let fx = fixture().git(["init", "-q"]);
    let fx = commit(fx, "a", "one\n");
    let fx = commit(fx, "sub/b", "two\n");
    let fx = commit(fx, "sub/deep/c", "three\n");
    fx.git(["tag", "-a", "-m", "ann", "v1"])
        .git(["tag", "light", "HEAD~1"])
}

fn out(fx: &Fixture, args: &[&str]) -> String {
    String::from_utf8(fx.git_stdout(args))
        .unwrap()
        .trim()
        .to_string()
}

fn loose_path(fx: &Fixture, oid: &str) -> std::path::PathBuf {
    fx.work_dir()
        .join(".git/objects")
        .join(&oid[..2])
        .join(&oid[2..])
}

/// Write `data` as an object with `hash-object --literally`.
fn literally(fx: Fixture, kind: &str, data: &[u8]) -> (Fixture, String) {
    let fx = fx.write("obj.tmp", data);
    let oid = out(
        &fx,
        &["hash-object", "--literally", "-w", "-t", kind, "obj.tmp"],
    );
    (fx.remove("obj.tmp"), oid)
}

#[track_caller]
fn same_all(fx: &mut Fixture, cases: &[&[&str]]) {
    let mut failures = Vec::new();
    for args in cases {
        let mut full = vec!["fsck"];
        full.extend_from_slice(args);
        if let Err(m) = fx
            .compare(&full)
            .checks(OUTPUT)
            .normalize_stdout(sorted())
            .try_run()
        {
            failures.push(m.to_string());
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

const VARIANTS: &[&[&str]] = &[
    &[],
    &["--no-dangling"],
    &["--unreachable"],
    &["--root", "--tags"],
    &["--connectivity-only"],
    &["--name-objects"],
    &["--no-reflogs"],
    &["--strict"],
    &["--cache"],
];

#[test]
fn healthy_and_dangling() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx;
    same_all(&mut fx, VARIANTS);
    // Dangling and unreachable objects of every type.
    let fx = fx.write("d1", "dangling\n");
    let d1 = out(&fx, &["hash-object", "-w", "d1"]);
    let fx = fx.write("u1", "unreferenced\n");
    let u1 = out(&fx, &["hash-object", "-w", "u1"]);
    let mut raw = b"100644 zz\0".to_vec();
    raw.extend(hex(&u1));
    let (fx, _tree) = literally(fx, "tree", &raw);
    let _ = d1;
    let mut fx = fx
        .git(["commit-tree", "-m", "dc", "-p", "HEAD", "HEAD^{tree}"])
        .git(["tag", "-a", "-m", "x", "tmp", "HEAD~2"])
        .git(["tag", "-d", "tmp"])
        .git(["reset", "-q", "--hard", "HEAD~1"]);
    same_all(&mut fx, VARIANTS);
    let head = out(&fx, &["rev-parse", "HEAD"]);
    same_all(
        &mut fx,
        &[
            &["HEAD~1"],
            &[&head],
            &["bogus"],
            &["0000000000000000000000000000000000000000"],
        ],
    );
}

#[test]
fn lost_found() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let fx = fx
        .write("d1", "dangling\n")
        .git(["hash-object", "-w", "d1"])
        .git(["commit-tree", "-m", "dc", "HEAD^{tree}"]);
    let mut fx = fx;
    fx.compare(["fsck", "--lost-found"])
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::Worktree,
        ])
        .normalize_stdout(sorted())
        .run();
    let _ = Path::new(".");
}

/// A repository like t1450's `create_repo_missing`, with one object
/// removed.
fn missing(what: &str) -> Fixture {
    let fx = fixture()
        .git(["init", "-q"])
        .git(["commit", "-q", "-m", "one", "--allow-empty"])
        .tick();
    let fx = commit(fx, "subdir/file", "content\n");
    let fx = fx.write("u", "unrelated\n");
    let unrelated = out(&fx, &["hash-object", "-w", "u"]);
    let fx = fx
        .remove("u")
        .git(["tag", "-m", "foo", "tag", &unrelated])
        .git(["reflog", "expire", "--expire=now", "--all"]);
    let oid = out(&fx, &["rev-parse", "--verify", what]);
    let path = loose_path(&fx, &oid);
    std::fs::remove_file(path).unwrap();
    fx
}

#[test]
fn missing_objects() {
    if fixture().is_skipped() {
        return;
    }
    let mut failures = Vec::new();
    for what in [
        "HEAD:subdir/file",
        "HEAD:subdir",
        "HEAD^{tree}",
        "HEAD^",
        "tag^{blob}",
        "HEAD",
        "tag",
    ] {
        let mut fx = missing(what);
        for args in [
            &["fsck"][..],
            &["fsck", "--name-objects"],
            &["fsck", "--unreachable"],
            &["fsck", "--connectivity-only"],
        ] {
            if let Err(m) = fx
                .compare(args)
                .checks(OUTPUT)
                .normalize_stdout(sorted())
                .try_run()
            {
                failures.push(format!("{what}: {m}"));
            }
        }
        let fx = fx.remove(".git/index");
        let mut fx = fx;
        if let Err(m) = fx
            .compare(["fsck", "--name-objects"])
            .checks(OUTPUT)
            .normalize_stdout(sorted())
            .try_run()
        {
            failures.push(format!("{what} (no index): {m}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn bad_objects() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let basis = fx.git_stdout(["cat-file", "commit", "HEAD"]);
    let basis = String::from_utf8(basis).unwrap();
    let tree = out(&fx, &["rev-parse", "HEAD^{tree}"]);
    let (fx, c1) = literally(fx, "commit", basis.replacen("@", "@x>", 1).as_bytes());
    let fx = fx.git(["update-ref", "refs/heads/bogus1", &c1]);
    let overflow = basis
        .lines()
        .map(|l| {
            if l.starts_with("author ") {
                let i = l.find("> ").unwrap();
                format!("{} 18446744073709551617 +0000", &l[..=i])
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let (fx, c2) = literally(fx, "commit", overflow.as_bytes());
    let fx = fx.git(["update-ref", "refs/heads/bogus2", &c2]);
    let multi = basis.replacen("committer", "author A <a@b> 1 +0000\ncommitter", 1);
    let (fx, c3) = literally(fx, "commit", multi.as_bytes());
    let fx = fx.git(["update-ref", "refs/heads/bogus3", &c3]);
    let tag = format!(
        "object {}\ntype commit\ntag wrong name format\n\nmsg\n",
        out(&fx, &["rev-parse", "HEAD"])
    );
    let (fx, t1) = literally(fx, "tag", tag.as_bytes());
    let fx = fx.git(["update-ref", "refs/tags/wrong", &t1]);
    let blob = out(&fx, &["rev-parse", "HEAD:a"]);
    let mut raw = b"100000 foo\0".to_vec();
    raw.extend(hex(&blob));
    let (fx, _bogus_mode) = literally(fx, "tree", &raw);
    let (fx, _short) = literally(fx, "tree", b"100644 x");
    let mut dup = Vec::new();
    for _ in 0..2 {
        dup.extend_from_slice(b"100644 x\0");
        dup.extend(hex(&blob));
    }
    let (fx, _dup) = literally(fx, "tree", &dup);
    let _ = tree;
    let mut fx = fx.write("skip", format!("{c1}\n"));
    same_all(
        &mut fx,
        &[
            &[],
            &["--strict"],
            &["--connectivity-only"],
            &["--tags"],
            &["--no-dangling"],
        ],
    );
    for cfg in [
        "fsck.badEmail=warn",
        "fsck.multipleAuthors=ignore",
        "fsck.badDateOverflow=ignore",
        "fsck.badEmail=bogus",
        "fsck.bogusId=warn",
        "fsck.skipList=skip",
        "fsck.skipList=nonexistent",
        "fsck.badFilemode=error",
        "fsck.largePathname=warn:2",
        "fsck.unterminatedHeader=warn",
    ] {
        fx.compare(["-c", cfg, "fsck", "--no-dangling"])
            .checks(OUTPUT)
            .normalize_stdout(sorted())
            .run();
    }
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn corrupt_loose_objects() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let fx = fx.write("e", "");
    let empty = out(&fx, &["hash-object", "-w", "--literally", "e"]);
    let path = loose_path(&fx, &empty);
    let mut data = std::fs::read(&path).unwrap();
    data.extend_from_slice(b"extra garbage\n");
    let fx = fx.write(".git/objects/tmp", &data);
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(fx.work_dir().join(".git/objects/tmp"), &path).unwrap();
    let mut fx = fx;
    same_all(&mut fx, &[&[], &["--connectivity-only"]]);

    // Not zlib at all (an unreferenced object).
    let fx = history().write("u", "unreferenced\n");
    let u = out(&fx, &["hash-object", "-w", "u"]);
    std::fs::remove_file(loose_path(&fx, &u)).unwrap();
    let fx = fx.write(".git/objects/x", "broken\n");
    std::fs::rename(fx.work_dir().join(".git/objects/x"), loose_path(&fx, &u)).unwrap();
    let mut fx = fx;
    same_all(&mut fx, &[&[]]);

    let fx = history().write("blobfile", "blob\n");
    let blob = out(&fx, &["hash-object", "-w", "blobfile"]);
    let other = format!("{}ff{}", &blob[..2], &blob[4..]);
    let to = loose_path(&fx, &other);
    std::fs::rename(loose_path(&fx, &blob), &to).unwrap();
    let mut fx = fx;
    same_all(&mut fx, &[&[]]);
}

#[test]
fn broken_refs() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let tree = out(&fx, &["rev-parse", "HEAD^{tree}"]);
    let head = out(&fx, &["rev-parse", "HEAD"]);
    let zero = "0".repeat(40);
    let cases: Vec<(&str, String)> = vec![
        (".git/refs/heads/invalid", format!("{tree}\n")),
        (".git/refs/heads/zero", format!("{zero}\n")),
        (".git/refs/heads/garbage", "garbage\n".into()),
        (".git/refs/heads/nonl", head.clone()),
        (".git/refs/heads/trail", format!("{head}  \n")),
        (".git/HEAD", "ref: refs/funny/place\n".into()),
        (
            ".git/refs/remotes/origin/HEAD",
            "ref: refs/heads/main\n".into(),
        ),
        (".git/refs/heads/dangle", "ref: refs/heads/nowhere\n".into()),
        (".git/HEAD", format!("{zero}\n")),
    ];
    for (file, content) in cases {
        let mut fx = history().write(file, content);
        fx.compare(["fsck"])
            .env("GIT_DIR", ".git")
            .checks(OUTPUT)
            .run();
    }
}

#[test]
fn names_through_merges_and_reflogs() {
    let fx = fixture()
        .git(["init", "-q"])
        .git(["config", "core.logAllRefUpdates", "false"]);
    let fx = commit(fx, "a", "a\n");
    let fx = commit(fx, "b", "b\n").git(["checkout", "-q", "-b", "s1"]);
    let fx = commit(fx, "s1a", "s1a\n");
    let fx = commit(fx, "s1b", "s1b\n");
    let fx = commit(fx, "s1c", "s1c\n").git(["checkout", "-q", "main"]);
    let fx = commit(fx, "m1", "m1\n").git(["merge", "-q", "--no-edit", "s1"]);
    let fx = commit(fx, "m2", "m2\n");
    if fx.is_skipped() {
        return;
    }
    for x in ["HEAD~1^2~2:s1a", "HEAD~1^2^:s1b", "HEAD~3:b", "HEAD~4:a"] {
        let oid = out(&fx, &["rev-parse", x]);
        std::fs::remove_file(loose_path(&fx, &oid)).unwrap();
    }
    let mut fx = fx.remove(".git/index");
    same_all(
        &mut fx,
        &[
            &["--name-objects"],
            &["--name-objects", "HEAD^"],
            &["--name-objects", "HEAD~2"],
        ],
    );
}

/// A bad tagger in a tag is reported, but a header line after it decides
/// the verdict (t3800).
#[test]
fn tag_verdict_quirk() {
    let h = "1".repeat(40);
    for tag in [
        format!("object {h}\ntype commit\ntag t\ntagger T <\n > 0 +0000\n\n"),
        format!("object {h}\ntype commit\ntag t\ntagger T <t@e>0 +0000\n\n"),
    ] {
        let fx = fixture().git(["init", "-q", "--bare", "repo"]);
        if fx.is_skipped() {
            return;
        }
        let fx = fx.write("tag.txt", tag);
        let _ = out(
            &fx,
            &[
                "-C",
                "repo",
                "hash-object",
                "--literally",
                "-w",
                "-t",
                "tag",
                "../tag.txt",
            ],
        );
        let mut fx = fx;
        fx.compare(["fsck"])
            .in_dir("repo")
            .repo("repo")
            .checks(OUTPUT)
            .normalize_stdout(sorted())
            .run();
    }
}

/// `--progress`: `Checking ref database`, `Checking object directories`
/// (one step per fan-out directory with objects) and `Checking objects`
/// (per pack); none of the object meters with `--connectivity-only`, and
/// no meters with `--verbose` or `--no-progress`.
#[test]
fn progress_meters() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let fx = fx.git(["repack", "-aq"]);
    let mut fx = commit(fx, "c", "three\n");
    for args in [
        &["fsck", "--progress"][..],
        &["fsck", "--progress", "--connectivity-only"],
        &["fsck", "--progress", "--no-dangling", "--no-full"],
        &["fsck", "--no-progress"],
    ] {
        fx.compare(args).checks(OUTPUT).run();
    }
}
