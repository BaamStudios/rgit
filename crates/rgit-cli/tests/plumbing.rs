// SPDX-License-Identifier: MIT
//! Differential tests of `hash-object`, `cat-file`, `mktree` and `mktag`
//! against the reference git (issue #15).

use std::collections::BTreeMap;

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

/// A repository with a few files, a symlink, a commit, an annotated and a
/// lightweight tag.
fn repo() -> Fixture {
    let fx = fixture()
        .git(["init", "-q"])
        .write("a", "hi\n")
        .write("d/b", "there\n")
        .write("path with spaces", "Hello World");
    #[cfg(unix)]
    let fx = fx.symlink("a", "l");
    fx.git(["add", "."])
        .git(["commit", "-q", "-m", "first"])
        .tick()
        .git(["tag", "-a", "-m", "msg", "v1"])
        .git(["tag", "lw"])
}

fn out(fx: &mut Fixture, args: &[&str]) {
    fx.compare(args).checks(Check::OUTPUT).run();
}

fn out_stdin(fx: &mut Fixture, args: &[&str], input: impl Into<Vec<u8>>) {
    fx.compare(args)
        .stdin(input.into())
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn hash_object_files_and_stdin() {
    let mut fx = repo();
    out(&mut fx, &["hash-object", "a", "d/b"]);
    out_stdin(&mut fx, &["hash-object", "--stdin"], "x\n");
    out_stdin(&mut fx, &["hash-object", "--stdin", "a"], "x\n");
    out_stdin(
        &mut fx,
        &["hash-object", "--stdin-paths"],
        "a\n\"d/b\"\nnope\n",
    );
    out(&mut fx, &["hash-object", "nope"]);
    out(&mut fx, &["hash-object", "d"]);
    out(&mut fx, &["hash-object", "-t", "bogus", "a"]);
    out(&mut fx, &["hash-object", "-t", "tree", "a"]);
    out(&mut fx, &["hash-object", "-t", "commit", "a"]);
    out(&mut fx, &["hash-object", "-t", "tree", "--literally", "a"]);
    out_stdin(&mut fx, &["hash-object", "--stdin", "--stdin"], "");
    out_stdin(&mut fx, &["hash-object", "--stdin-paths", "--stdin"], "");
    out(&mut fx, &["hash-object", "--no-filters", "--path=x", "a"]);
    out(&mut fx, &["hash-object", "-h"]);
    fx.compare(["hash-object", "a"])
        .in_dir("d")
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["hash-object", "b", "nope"])
        .in_dir("d")
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn hash_object_write() {
    let mut fx = repo();
    fx.compare(["hash-object", "-w", "a", "path with spaces"])
        .run();
    fx.compare(["hash-object", "-w", "--stdin"])
        .stdin(b"new content\n".to_vec())
        .run();
    let tag = "object 0000000000000000000000000000000000000001\ntype commit\ntag t\n\
               tagger T <t@example.com> 1 +0000\nextra\n\nmsg\n";
    fx.compare(["hash-object", "-w", "-t", "tag", "--stdin"])
        .stdin(tag.as_bytes().to_vec())
        .run();
}

#[test]
fn cat_file_single_object_modes() {
    let mut fx = repo();
    for name in [
        "HEAD",
        "HEAD^{tree}",
        "HEAD:a",
        "HEAD:d",
        "HEAD:l",
        "v1",
        "lw",
        ":a",
        "v1^{}",
        "@",
        "HEAD~0",
        "HEAD^0",
        "tags/v1",
        "HEAD:",
        "HEAD:d/",
        "HEAD:./a",
    ] {
        for mode in ["-t", "-s", "-p", "-e"] {
            out(&mut fx, &["cat-file", mode, name]);
        }
    }
    for (kind, name) in [
        ("commit", "v1"),
        ("tree", "HEAD"),
        ("blob", "HEAD"),
        ("tag", "v1"),
        ("bogus", "HEAD"),
        ("blob", "HEAD:a"),
    ] {
        out(&mut fx, &["cat-file", kind, name]);
    }
    for name in [
        "nope",
        "HEAD:nope",
        ":nope",
        ":1:a",
        "HEAD^{tag}",
        "HEAD~1",
        "HEAD@{5}",
        "deadbeef",
        "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        "HEAD:/a",
        "HEAD:a/",
    ] {
        for mode in ["-t", "-p", "-e"] {
            out(&mut fx, &["cat-file", mode, name]);
        }
    }
}

#[test]
fn cat_file_usage_errors() {
    let mut fx = repo();
    for args in [
        &["cat-file"][..],
        &["cat-file", "-e"],
        &["cat-file", "-e", "-p", "HEAD"],
        &["cat-file", "-p", "HEAD", "HEAD"],
        &["cat-file", "HEAD"],
        &["cat-file", "a", "b", "c"],
        &["cat-file", "--batch", "HEAD"],
        &["cat-file", "-t", "--batch"],
        &["cat-file", "--path=x", "--batch"],
        &["cat-file", "--buffer"],
        &["cat-file", "--follow-symlinks", "--buffer"],
        &["cat-file", "-z", "-Z"],
        &["cat-file", "--batch-all-objects", "-e", "HEAD"],
        &["cat-file", "--batch", "--batch-check"],
        &["cat-file", "--textconv"],
        &["cat-file", "--batch=%(foo)"],
        &["cat-file", "--batch-check=%(objectname"],
        &["cat-file", "-h"],
    ] {
        out_stdin(&mut fx, args, "");
    }
}

#[test]
fn cat_file_batch_formats() {
    let mut fx = repo();
    let input = "HEAD\nHEAD:a\nHEAD:d\nHEAD:l\nv1\nnope\n\nHEAD:path with spaces\n:a\nHEAD:a\r\n";
    for args in [
        &["cat-file", "--batch"][..],
        &["cat-file", "--batch-check"],
        &[
            "cat-file",
            "--batch-check=%(objectname) %(objecttype) %(objectsize) %(objectsize:disk) %(deltabase) [%(objectmode)] %(rest)",
        ],
        &["cat-file", "--batch=%(objecttype)"],
        &["cat-file", "--batch-check="],
        &["cat-file", "--batch-check", "-Z"],
        &["cat-file", "--batch", "-z"],
    ] {
        let data = if args.contains(&"-Z") || args.contains(&"-z") {
            input.replace('\n', "\0")
        } else {
            input.to_string()
        };
        out_stdin(&mut fx, args, data);
    }
    out_stdin(
        &mut fx,
        &["cat-file", "--batch-check=%(objectname) [%(rest)]"],
        "HEAD:a  rest of line\nHEAD\tx\nHEAD\n",
    );
    out(
        &mut fx,
        &["cat-file", "--batch-all-objects", "--batch-check"],
    );
    out(
        &mut fx,
        &["cat-file", "--batch-all-objects", "--batch=%(objectname)"],
    );
    out(
        &mut fx,
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname) %(objectsize:disk)",
        ],
    );
}

/// Packs are found by their `.idx` file whatever their name (t5300 copies
/// packs as `test-1-<hash>.{pack,idx}`).
#[test]
fn cat_file_pack_with_other_name() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx.git(["repack", "-adq"]);
    let pack_dir = fx.work_dir().join(".git/objects/pack");
    for e in std::fs::read_dir(&pack_dir).unwrap().flatten() {
        let name = e.file_name().into_string().unwrap();
        if let Some(rest) = name.strip_prefix("pack-") {
            std::fs::rename(e.path(), pack_dir.join(format!("test-1-{rest}"))).unwrap();
        }
    }
    out(&mut fx, &["cat-file", "-t", "HEAD:a"]);
    out(&mut fx, &["cat-file", "commit", "HEAD"]);
    out(
        &mut fx,
        &["cat-file", "--batch-all-objects", "--batch-check"],
    );
}

#[test]
fn cat_file_packed_objects() {
    let mut fx = repo()
        .write("big", "x".repeat(20000))
        .write("big2", format!("{}y\n", "x".repeat(20000)))
        .git(["add", "big", "big2"])
        .git(["commit", "-q", "-m", "big"])
        .git(["repack", "-adq"]);
    out_stdin(
        &mut fx,
        &[
            "cat-file",
            "--batch-check=%(objectname) %(objecttype) %(objectsize) %(objectsize:disk) %(deltabase)",
        ],
        "HEAD\nHEAD:big\nHEAD:big2\nHEAD^\nv1\n",
    );
    out_stdin(&mut fx, &["cat-file", "--batch"], "HEAD:big2\nHEAD\n");
    out(
        &mut fx,
        &["cat-file", "--batch-all-objects", "--batch-check"],
    );
    out(&mut fx, &["cat-file", "-p", "HEAD^{tree}"]);
}

#[test]
fn cat_file_batch_command_errors() {
    let mut fx = repo();
    for (args, input) in [
        (&["cat-file", "--batch-command"][..], "\n"),
        (&["cat-file", "--batch-command"], " info HEAD\n"),
        (&["cat-file", "--batch-command"], "bogus x\n"),
        (&["cat-file", "--batch-command"], "info\n"),
        (&["cat-file", "--batch-command"], "contents\n"),
        (&["cat-file", "--batch-command", "--buffer"], "flush x\n"),
        (&["cat-file", "--batch-command"], "flush\n"),
        (
            &["cat-file", "--batch-command"],
            "info HEAD\ninfo  HEAD\ninfo HEAD extra\n",
        ),
        (
            &["cat-file", "--batch-command=%(objectname) [%(rest)]"],
            "info HEAD extra\ncontents HEAD:a\n",
        ),
    ] {
        out_stdin(&mut fx, args, input);
    }
    fx.compare(["cat-file", "--batch-command", "--buffer"])
        .stdin(b"info HEAD\n".to_vec())
        .env("GIT_TEST_CAT_FILE_NO_FLUSH_ON_EXIT", "1")
        .checks(Check::OUTPUT)
        .run();
}

/// "Done when" of #15: a `--batch-command` session mixing `contents`,
/// `info` and `flush` for 1000 objects, including missing and ambiguous
/// names, is byte-identical to git's.
#[test]
fn cat_file_batch_command_session_1000_objects() {
    let mut fx = repo();
    for i in 0..1000 {
        fx = fx.write(format!("f/{i}"), format!("{i}\n"));
    }
    fx = fx.git(["add", "f"]).git(["commit", "-q", "-m", "many"]);
    if fx.is_skipped() {
        return;
    }
    let all = fx.git_stdout([
        "cat-file",
        "--batch-all-objects",
        "--batch-check=%(objectname)",
    ]);
    let ids: Vec<String> = String::from_utf8(all)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert!(ids.len() >= 1000);
    let mut by_prefix: BTreeMap<&str, usize> = BTreeMap::new();
    for id in &ids {
        *by_prefix.entry(&id[..4]).or_default() += 1;
    }
    let ambiguous: Vec<&str> = by_prefix
        .iter()
        .filter(|(_, n)| **n > 1)
        .map(|(p, _)| *p)
        .take(5)
        .collect();
    assert!(!ambiguous.is_empty(), "no ambiguous 4-digit prefix");
    let mut input = String::new();
    for (i, id) in ids.iter().enumerate() {
        let cmd = if i % 3 == 0 { "contents" } else { "info" };
        let name = if i % 7 == 0 { &id[..12] } else { id.as_str() };
        input.push_str(&format!("{cmd} {name}\n"));
        if i % 50 == 0 {
            input.push_str("flush\n");
        }
        if i % 97 == 0 {
            input.push_str(&format!("info {}\n", ambiguous[i % ambiguous.len()]));
            input.push_str("contents 0000000000000000000000000000000000000042\n");
            input.push_str(&format!("info HEAD:f/{i}\n"));
            input.push_str("info HEAD:f/nope\n");
        }
    }
    input.push_str("flush\n");
    for buffer in ["--buffer", "--no-buffer"] {
        let data = if buffer == "--buffer" {
            input.clone()
        } else {
            input.replace("flush\n", "")
        };
        out_stdin(&mut fx, &["cat-file", "--batch-command", buffer], data);
    }
}

#[cfg(unix)]
#[test]
fn cat_file_follow_symlinks() {
    let fx = fixture()
        .git(["init", "-q"])
        .write("morx", "Hello World")
        .write("dir/ind1", "Hello World")
        .write("dir/subdir/ind2", "Hello World")
        .symlink("morx", "same-dir-link")
        .symlink("dir", "link-to-dir")
        .symlink("../fleem", "out-of-repo-link")
        .symlink("..", "out-of-repo-link-dir")
        .symlink("nope", "broken-same-dir-link")
        .symlink("../morx", "dir/parent-dir-link")
        .symlink("..", "dir/link-dir")
        .symlink("../../../", "dir/subdir/out-of-repo-link-dir-trailing")
        .symlink("dir/../../morx", "up-up-down-file")
        .symlink("dir/..", "up-down")
        .symlink("loop1", "loop2")
        .symlink("loop2", "loop1")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "links"]);
    let mut fx = fx;
    let names = [
        "HEAD:morx",
        "HEAD:nope",
        "HEAD:same-dir-link",
        "HEAD:link-to-dir/ind1",
        "HEAD:broken-same-dir-link",
        "HEAD:dir/parent-dir-link",
        "HEAD:dir/parent-dir-link/nope",
        "HEAD:dir/link-dir/nope",
        "HEAD:dir/link-dir/morx",
        "HEAD:out-of-repo-link",
        "HEAD:out-of-repo-link-dir",
        "HEAD:dir/subdir/out-of-repo-link-dir-trailing",
        "HEAD:up-up-down-file",
        "HEAD:up-down",
        "HEAD:loop1",
    ];
    let input: String = names.iter().map(|n| format!("{n}\n")).collect();
    out_stdin(
        &mut fx,
        &["cat-file", "--batch-check", "--follow-symlinks"],
        input.clone(),
    );
    out_stdin(
        &mut fx,
        &["cat-file", "--batch", "--follow-symlinks", "-Z"],
        input.replace('\n', "\0"),
    );
}

#[test]
fn mktree_from_ls_tree_lines() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    let top = fx.git_stdout(["ls-tree", "HEAD"]);
    let mut reversed: Vec<&[u8]> = top
        .split(|&c| c == b'\n')
        .filter(|l| !l.is_empty())
        .collect();
    reversed.reverse();
    let mut rev = reversed.join(&b'\n');
    rev.push(b'\n');
    fx.compare(["mktree"]).stdin(top.clone()).run();
    fx.compare(["mktree"]).stdin(rev).run();
    let all = fx.git_stdout(["ls-tree", "-r", "HEAD"]);
    out_stdin(&mut fx, &["mktree"], all);
    let blob = "45b983be36b73c0788dc9cbcb76cbb80fc7bb057";
    for input in [
        String::new(),
        "\n".to_string(),
        format!("100644 blob {blob}\tx\n100644 blob {blob}\tx\n"),
        format!("100644 blob {blob} x\n"),
        format!("100644 tree {blob}\tx\n"),
        format!("040000 blob {blob}\tx\n"),
        "100644 blob deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\tx\n".to_string(),
        "160000 commit deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\tsub\n".to_string(),
        format!("160000 commit {blob}\tsub\n"),
        format!("100644 bogus {blob}\tx\n"),
        format!("100644 blob {blob}\t\"q\\tx\"\n"),
        format!("100644 blob {blob}\t\"bad\n"),
        format!("100644 blob {blob}\ta/b\n"),
        format!("100664 blob {blob}\tx\n644 blob {blob}\ty\n 100644 blob {blob}\tz\n"),
        format!("100644 blob {blob}\tx\n\n100644 blob {blob}\ty\n"),
    ] {
        fx.compare(["mktree"])
            .stdin(input.clone().into_bytes())
            .run();
    }
    for input in [
        format!("100644 blob {blob}\tx\n\n\n100644 blob {blob}\ty"),
        "\n".to_string(),
        String::new(),
    ] {
        fx.compare(["mktree", "--batch"])
            .stdin(input.into_bytes())
            .run();
    }
    fx.compare(["mktree", "--missing"])
        .stdin(
            "100644 blob deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\tx\n"
                .to_string()
                .into_bytes(),
        )
        .run();
    fx.compare(["mktree", "-z"])
        .stdin(format!("100644 blob {blob}\tx\n\0").into_bytes())
        .run();
}

#[test]
fn mktag_checks() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    let head = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD"])).unwrap();
    let head = head.trim();
    let tagger = "tagger T A Gger <tagger@example.com> 1206478233 -0500";
    let cases = [
        format!("object {head}\ntype commit\ntag mytag\n{tagger}\n"),
        "too short for a tag\n".to_string(),
        format!("object {head}\ntype commit\ntag my\ttag\ntagger . <> 0 +0000\n\n"),
        format!("object {head}\ntype commit\ntag mytag\n\nThis is filler\n"),
        "object deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\ntype tag\ntag mytag\ntagger . <> 0 +0000\n\n".to_string(),
        format!("object {head}\ntype tree\ntag mytag\ntagger . <> 0 +0000\n\n"),
        format!("object {head}\ntype tagggg\ntag mytag\ntagger . <> 0 +0000\n\n"),
        format!(
            "object {head}\ntype commit\ntag mytag\n{tagger}\nthis line should not be here\n\n"
        ),
        format!("object {head}\ntype commit\ntag mytag\ntagger T A Gger <\n > 0 +0000\n\n"),
        format!(
            "object {head}\ntype commit\ntag mytag\ntagger T A Gger <tagger@example.com> Tue Mar 25 15:47:44 2008\n\n"
        ),
        String::new(),
    ];
    for input in &cases {
        for args in [
            &["mktag"][..],
            &["mktag", "--no-strict"],
            &["-c", "fsck.extraHeaderEntry=warn", "mktag", "--no-strict"],
            &["-c", "fsck.badDate=ignore", "mktag"],
        ] {
            fx.compare(args).stdin(input.clone().into_bytes()).run();
        }
    }
    for config in [
        "fsck.extraHeaderEntry=bogus",
        "fsck.nosuchid=warn",
        "fsck.unterminatedHeader=ignore",
        "fsck.skipList=/nonexistent",
    ] {
        out_stdin(&mut fx, &["-c", config, "mktag"], cases[0].clone());
    }
    out_stdin(&mut fx, &["mktag", "--unknown-option"], "");
}
