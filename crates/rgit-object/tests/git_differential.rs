// SPDX-License-Identifier: MIT

//! Differential tests against the reference `git` binary:
//!
//! - the strict validator ([`rgit_object::fsck`]) reports the same first
//!   finding as `git mktag` (which runs all checks as errors) and the same
//!   findings as `git hash-object -t <type> --stdin`;
//! - tree sorting gives the same tree as `git mktree`.
//!
//! Skipped when no `git` binary is available.

mod common;

use std::path::Path;

use common::{Rng, git, git_available, git_raw, scratch_repo};
use rgit_hash::{HashKind, ObjectId};
use rgit_object::fsck::{self, Control, MsgId, Options};
use rgit_object::{EntryMode, ObjectKind, Tree, TreeEntry};

/// Findings of our validator when every finding not in `ignored` is an
/// error (stop at the first), as with `git mktag` / `git hash-object`.
fn strict(kind: ObjectKind, hash: HashKind, data: &[u8], ignored: &[MsgId]) -> Vec<String> {
    let mut out = Vec::new();
    fsck::check_object(kind, hash, data, &Options::default(), &mut |f| {
        if ignored.contains(&f.id) {
            return Control::Continue;
        }
        out.push(f.to_string());
        Control::Stop
    });
    out
}

/// The findings git printed, in order.
fn git_findings(stderr: &[u8], prefix: &str) -> Vec<String> {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter_map(|l| l.strip_prefix(prefix).map(str::to_owned))
        .collect()
}

fn ident_variants(rng: &mut Rng) -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = [
        "A U Thor <a@b.c> 1234567890 +0000",
        "A <a@b.c> 0 +0000",
        "A <a@b.c> 0 -0000",
        "A <a@b.c> 1 +9999",
        "<a@b.c> 1 +0000",
        " <a@b.c> 1 +0000",
        "  <a@b.c> 1 +0000",
        "A <> 1 +0000",
        "A a@b.c> 1 +0000",
        "A<a@b.c> 1 +0000",
        "A <a@b.c 1 +0000",
        "A <a<b.c> 1 +0000",
        "A <a@b>c> 1 +0000",
        "A <a@b.c>1 +0000",
        "A <a@b.c>  1 +0000",
        "A <a@b.c> \t1 +0000",
        "A <a@b.c> 01 +0000",
        "A <a@b.c> 00 +0000",
        "A <a@b.c> 0x1 +0000",
        "A <a@b.c> -1 +0000",
        "A <a@b.c> -0 +0000",
        "A <a@b.c> +1 +0000",
        "A <a@b.c> 9223372036854775807 +0000",
        "A <a@b.c> 9223372036854775808 +0000",
        "A <a@b.c> 18446744073709551615 +0000",
        "A <a@b.c> 18446744073709551616 +0000",
        "A <a@b.c> 99999999999999999999999 +0000",
        "A <a@b.c> x +0000",
        "A <a@b.c> 1",
        "A <a@b.c> 1 ",
        "A <a@b.c> 1  +0000",
        "A <a@b.c> 1 +000",
        "A <a@b.c> 1 +00000",
        "A <a@b.c> 1 0000",
        "A <a@b.c> 1 +12a4",
        "A <a@b.c> 1 +0000 ",
        "A <a@b.c>",
        "A <a@b.c> ",
        "A <a@b.c>  +0000",
        "A <a@b.c> +0000",
        "",
        "A",
        "A <",
        ">",
        "A \x01 <a> 1 +0000",
    ]
    .iter()
    .map(|s| s.as_bytes().to_vec())
    .collect();
    let base = b"A U Thor <a@b.c> 1234567890 +0130";
    let alphabet = b" <>+-0123456789\tax";
    for _ in 0..1000 {
        let mut d = base.to_vec();
        for _ in 0..1 + rng.below(3) {
            let i = rng.below(d.len() + 1);
            match rng.below(3) {
                0 if i < d.len() => {
                    d.remove(i);
                }
                1 => d.insert(i, alphabet[rng.below(alphabet.len())]),
                _ if i < d.len() => d[i] = alphabet[rng.below(alphabet.len())],
                _ => {}
            }
        }
        v.push(d);
    }
    v
}

fn make_commit(repo: &Path) -> String {
    let tree = String::from_utf8(git(repo, &["mktree"], b"")).unwrap();
    let c = git(repo, &["commit-tree", "-m", "x", tree.trim()], b"");
    String::from_utf8(c).unwrap().trim().to_owned()
}

#[test]
fn mktag_findings_match() {
    if !git_available() {
        eprintln!("skipped: no git binary");
        return;
    }
    let mut rng = Rng::new(42);
    for hash in HashKind::ALL {
        let repo = scratch_repo("mktag", hash);
        let c = make_commit(&repo);
        let tagger = "tagger A U Thor <a@b.c> 1234567890 +0000";
        let mut inputs: Vec<(Vec<u8>, Vec<MsgId>)> = vec![];
        let mut add = |s: String| inputs.push((s.into_bytes(), vec![]));
        add(format!(
            "object {c}\ntype commit\ntag v1\n{tagger}\n\nmsg\n"
        ));
        add(format!("object {c}\ntype commit\ntag v1\n\nmsg\n"));
        add(format!(
            "object {c}\ntype commit\ntag v1\n{tagger}\nfoo bar\n\nmsg\n"
        ));
        add(format!("type commit\ntag v1\n{tagger}\n\nmsg\n"));
        add(format!("object zz\ntype commit\ntag v1\n{tagger}\n\nmsg\n"));
        add(format!(
            "object {c} \ntype commit\ntag v1\n{tagger}\n\nmsg\n"
        ));
        add(format!(
            "object {}\ntype commit\ntag v1\n{tagger}\n\nmsg\n",
            c.to_uppercase()
        ));
        add(format!("object {c}\ntype blah\ntag v1\n{tagger}\n\nmsg\n"));
        add(format!(
            "object {c}\ntype  commit\ntag v1\n{tagger}\n\nmsg\n"
        ));
        add(format!("object {c}\ntag v1\n{tagger}\n\nmsg\n"));
        add(format!("object {c}\ntype commit\n"));
        add(format!("object {c}\ntype commit\n\n"));
        add(format!(
            "object {c}\ntype commit\ntag v 1\n{tagger}\n\nmsg\n"
        ));
        add(format!("object {c}\ntype commit\ntag \n{tagger}\n\nmsg\n"));
        add(format!(
            "object {c}\ntype commit\ntag a..b\n{tagger}\n\nmsg\n"
        ));
        add(format!(
            "object {c}\ntype commit\ntag a.lock\n{tagger}\n\nmsg\n"
        ));
        add(format!(
            "object {c}\ntype commit\ntag .a\n{tagger}\n\nmsg\n"
        ));
        add(format!(
            "object {c}\ntype commit\ntag a/\n{tagger}\n\nmsg\n"
        ));
        add(format!(
            "object {c}\ntype commit\ntag a//b\n{tagger}\n\nmsg\n"
        ));
        add(format!(
            "object {c}\ntype commit\ntag a@{{b\n{tagger}\n\nmsg\n"
        ));
        add(format!("object {c}\ntype commit\ntag @\n{tagger}\n\nmsg\n"));
        add(format!(
            "object {c}\ntype commit\ntag a.\n{tagger}\n\nmsg\n"
        ));
        add(format!(
            "object {c}\ntype commit\ntag a/b\n{tagger}\n\nmsg\n"
        ));
        add(format!(
            "object {c}\ntype commit\ntag a\\b\n{tagger}\n\nmsg\n"
        ));
        add(format!("object {c}\ntype commit\ntag v1\n{tagger}"));
        add(format!("object {c}\ntype commit\ntag v1\n{tagger}\n"));
        add(format!(
            "object {c}\ntype commit\ntag v1\n{tagger}\n\nmsg\0nul\n"
        ));
        add(format!(
            "object {c}\ntype commit\ntag v1\0\n{tagger}\n\nmsg\n"
        ));
        add(format!(
            "object {c}\ntype commit\ntag v1\n{tagger}\n gpgsig x\n\nmsg\n"
        ));
        for id in ident_variants(&mut rng) {
            let mut d = format!("object {c}\ntype commit\ntag v1\ntagger ").into_bytes();
            d.extend_from_slice(&id);
            d.extend_from_slice(b"\n\nmsg\n");
            inputs.push((d, vec![]));
        }
        // Downgraded findings let the check continue.
        let more = [
            (
                format!("object {c}\ntype commit\ntag v1\nfoo bar\n\nmsg\n"),
                vec![MsgId::MissingTaggerEntry],
            ),
            (
                format!("object {c}\ntype commit\ntag v 1\nfoo\n\nm\n"),
                vec![MsgId::BadTagName, MsgId::MissingTaggerEntry],
            ),
            (
                format!("object {c}\ntype commit\ntag v1\ntagger A <a> 01 +0000\nx\n\nm\n"),
                vec![MsgId::ZeroPaddedDate],
            ),
            (
                format!("object {c}\ntype commit\ntag v1\ntagger A <a> 1 +0000\nx\n\nm\n"),
                vec![MsgId::ExtraHeaderEntry],
            ),
        ];
        inputs.extend(more.into_iter().map(|(s, ign)| (s.into_bytes(), ign)));

        for (data, ignored) in inputs {
            let mut args = vec![];
            let cfg: Vec<String> = ignored
                .iter()
                .map(|id| format!("fsck.{}=ignore", id.name()))
                .collect();
            for c in &cfg {
                args.extend(["-c", c.as_str()]);
            }
            args.push("mktag");
            let out = git_raw(&repo, &args, &data);
            let theirs = git_findings(&out.stderr, "error: tag input does not pass fsck: ");
            let ours = strict(ObjectKind::Tag, hash, &data, &ignored);
            let what = String::from_utf8_lossy(&data);
            if out.status.success() {
                assert!(ours.is_empty(), "git accepts, we report {ours:?}: {what:?}");
            } else {
                assert_eq!(
                    ours,
                    theirs,
                    "{what:?}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }
        std::fs::remove_dir_all(repo).unwrap();
    }
}

#[test]
fn hash_object_commit_findings_match() {
    if !git_available() {
        eprintln!("skipped: no git binary");
        return;
    }
    let mut rng = Rng::new(7);
    for hash in HashKind::ALL {
        let repo = scratch_repo("hocommit", hash);
        let t = hash.empty_tree().to_hex();
        let p = hash.empty_blob().to_hex();
        let a = "author A <a@b> 1 +0000";
        let c = "committer C <c@d> 1 +0000";
        let mut inputs: Vec<Vec<u8>> = [
            format!("tree {t}\n{a}\n{c}\n\nmsg\n"),
            format!("tree {t}\nparent {p}\nparent {p}\n{a}\n{c}\n\nmsg\n"),
            format!("{a}\n{c}\n\nmsg\n"),
            format!("parent {p}\ntree {t}\n{a}\n{c}\n\nmsg\n"),
            format!("tree zz\n{a}\n{c}\n\nmsg\n"),
            format!("tree {t} \n{a}\n{c}\n\nmsg\n"),
            format!("tree {t}\nparent zz\n{a}\n{c}\n\nmsg\n"),
            format!("tree {t}\nparent {p}0\n{a}\n{c}\n\nmsg\n"),
            format!("tree {t}\n{c}\n\nmsg\n"),
            format!("tree {t}\n{a}\n\nmsg\n"),
            format!("tree {t}\n{a}\n{a}\n{c}\n\nmsg\n"),
            format!("tree {t}\n{a}\n{c}\n{c}\n\nmsg\n"),
            format!("tree {t}\n{a}\n{c}\nencoding foo\n\nmsg\n"),
            format!("tree {t}\n{a}\nfoo bar\n{c}\n\nmsg\n"),
            format!("tree {t}\nfoo\n{a}\n{c}\n\nmsg\n"),
            format!("tree {t}\n{a}\n{c}\n\nmsg\0nul\n"),
            format!("tree {t}\n{a}\n{c}\nfoo\0bar\n\nmsg\n"),
            format!("tree {t}\n{a}\n{c}\n"),
            format!("tree {t}\n{a}\n{c}"),
            format!("tree {t}\n{a}\n{c}\n\n"),
            format!("tree {t}\n{a}\n{c}\ngpgsig a\n b\n\nmsg\n"),
            format!("tree {t}\n{a}\n{c}\ngpgsig a\n b"),
            format!(" tree {t}\n{a}\n{c}\n\nmsg\n"),
            format!("tree {t}\n{a}\nparent {p}\n{c}\n\nmsg\n"),
            format!("tree {t}\n\n{a}\n{c}\n\nmsg\n"),
            format!("tree {t}\nauthor\n{c}\n\nmsg\n"),
            format!("tree {t}\nauthor \n{c}\n\nmsg\n"),
            format!("tree {t}\n{a}\ncommitter\n\nmsg\n"),
            format!("tree {t}\nauthorX A <a@b> 1 +0000\n{c}\n\nmsg\n"),
            format!("tree {}\n{a}\n{c}\n\nmsg\n", t.to_uppercase()),
            String::new(),
            "\n".into(),
        ]
        .into_iter()
        .map(String::into_bytes)
        .collect();
        for id in ident_variants(&mut rng) {
            let mut d = format!("tree {t}\nauthor ").into_bytes();
            d.extend_from_slice(&id);
            d.extend_from_slice(b"\ncommitter ");
            d.extend_from_slice(&id);
            d.extend_from_slice(b"\n\nm\n");
            inputs.push(d);
        }
        for data in inputs {
            let out = git_raw(&repo, &["hash-object", "-t", "commit", "--stdin"], &data);
            let theirs = git_findings(&out.stderr, "error: object fails fsck: ");
            let ours = strict(ObjectKind::Commit, hash, &data, &[]);
            assert_eq!(ours, theirs, "{:?}", String::from_utf8_lossy(&data));
            assert_eq!(out.status.success(), ours.is_empty());
        }
        std::fs::remove_dir_all(repo).unwrap();
    }
}

fn tree_bytes(hash: HashKind, entries: &[(&[u8], &[u8])], oid: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    for (mode, name) in entries {
        v.extend_from_slice(mode);
        v.push(b' ');
        v.extend_from_slice(name);
        v.push(0);
        v.extend_from_slice(&oid[..hash.byte_len()]);
    }
    v
}

#[test]
fn hash_object_tree_findings_match() {
    if !git_available() {
        eprintln!("skipped: no git binary");
        return;
    }
    for hash in HashKind::ALL {
        let repo = scratch_repo("hotree", hash);
        let b = hash.empty_blob();
        let t = hash.empty_tree();
        let z = hash.null();
        let e = |m: &'static [u8], n: &'static [u8]| (m, n);
        let blob_cases: Vec<Vec<(&[u8], &[u8])>> = vec![
            vec![e(b"100644", b"a")],
            vec![e(b"100644", b"b"), e(b"100644", b"a")],
            vec![e(b"100644", b"a"), e(b"100644", b"a")],
            vec![e(b"100644", b"a"), e(b"100644", b"a.c"), e(b"100644", b"b")],
            vec![e(b"100664", b"a")],
            vec![e(b"100600", b"a")],
            vec![e(b"644", b"a")],
            vec![e(b"100000", b"a")],
            vec![e(b"0100644", b"a")],
            vec![e(b"0", b"a")],
            vec![e(b"100755", b"a"), e(b"120000", b"b"), e(b"160000", b"c")],
            vec![e(b"100648", b"a")],
            vec![e(b"", b"a")],
            vec![e(b"100644", b"")],
            vec![e(b"100644", b".")],
            vec![e(b"100644", b"..")],
            vec![e(b"100644", b".git")],
            vec![e(b"100644", b".GIT")],
            vec![e(b"100644", b".Git")],
            vec![e(b"100644", b"git~1")],
            vec![e(b"100644", b"GIT~1")],
            vec![e(b"100644", b"git~2")],
            vec![e(b"100644", b".git.")],
            vec![e(b"100644", b".git ")],
            vec![e(b"100644", b".git. . .")],
            vec![e(b"100644", b".git.x")],
            vec![e(b"100644", b".git::$INDEX_ALLOCATION")],
            vec![e(b"100644", b".gitx")],
            vec![e(b"100644", b"..git")],
            vec![e(b"100644", b"a\\.git")],
            vec![e(b"100644", b".git\\a")],
            vec![e(b"100644", b"x\\git~1")],
            vec![e(b"100644", ".g\u{200c}it".as_bytes())],
            vec![e(b"100644", "\u{feff}.git".as_bytes())],
            vec![e(b"100644", ".gi\u{206f}T".as_bytes())],
            vec![e(b"100644", ".git\u{200d}".as_bytes())],
            vec![e(b"100644", ".g\u{2000}it".as_bytes())],
            vec![e(b"100644", b"a/b")],
            vec![e(b"100644", b"/a")],
            vec![e(b"120000", b".gitmodules")],
            vec![e(b"120000", b".GITMODULES")],
            vec![e(b"120000", b".gitmodules.")],
            vec![e(b"120000", b".gitmodules ..")],
            vec![e(b"120000", b".gitmodules:x")],
            vec![e(b"120000", b"gitmod~1")],
            vec![e(b"120000", b"GITMOD~4")],
            vec![e(b"120000", b"gitmod~5")],
            vec![e(b"120000", b"gi7eba~1")],
            vec![e(b"120000", b"GI7EBA~9")],
            vec![e(b"120000", b"gi7eba~0")],
            vec![e(b"120000", ".gitmodul\u{200c}es".as_bytes())],
            vec![e(b"120000", b".gitattributes")],
            vec![e(b"120000", b"gitatt~1")],
            vec![e(b"120000", b"gi7d29~1")],
            vec![e(b"120000", b".gitignore")],
            vec![e(b"120000", b"gitign~2")],
            vec![e(b"120000", b"gi250a~3")],
            vec![e(b"120000", b".mailmap")],
            vec![e(b"120000", b"mailma~1")],
            vec![e(b"120000", b"maba30~1")],
            vec![e(b"120000", b".mailmap ")],
            vec![e(b"100644", b".mailmap")],
            vec![
                e(b"100664", b"."),
                e(b"040000", b"a/b"),
                e(b"100644", b".."),
                e(b"100644", b".."),
                e(b"100644", b".git"),
            ],
            vec![
                e(b"120000", b".gitignore"),
                e(b"120000", b".mailmap"),
                e(b"100644", b"a/b"),
            ],
        ];
        let mut cases: Vec<Vec<u8>> = blob_cases
            .iter()
            .map(|c| tree_bytes(hash, c, b.as_bytes()))
            .collect();
        let big = vec![b'x'; 4097];
        let ok = vec![b'x'; 4096];
        cases.push(tree_bytes(hash, &[(b"100644", &big)], b.as_bytes()));
        cases.push(tree_bytes(hash, &[(b"100644", &ok)], b.as_bytes()));
        cases.push(tree_bytes(
            hash,
            &[(b"100644", b"a"), (b"100644", &big)],
            z.as_bytes(),
        ));
        cases.push(tree_bytes(hash, &[(b"100644", b"a")], z.as_bytes()));
        cases.push(
            [
                tree_bytes(hash, &[(b"40000", b"a")], t.as_bytes()),
                tree_bytes(hash, &[(b"100644", b"a.c")], b.as_bytes()),
            ]
            .concat(),
        );
        cases.push(
            [
                tree_bytes(hash, &[(b"100644", b"a.c")], b.as_bytes()),
                tree_bytes(hash, &[(b"40000", b"a")], t.as_bytes()),
            ]
            .concat(),
        );
        cases.push(
            [
                tree_bytes(hash, &[(b"40000", b"a")], t.as_bytes()),
                tree_bytes(hash, &[(b"100644", b"a")], b.as_bytes()),
            ]
            .concat(),
        );
        cases.push(
            [
                tree_bytes(
                    hash,
                    &[(b"100644", b"a"), (b"100644", b"a.c")],
                    b.as_bytes(),
                ),
                tree_bytes(hash, &[(b"40000", b"a")], t.as_bytes()),
            ]
            .concat(),
        );
        cases.push(
            [
                tree_bytes(hash, &[(b"100644", b"a"), (b"100644", b"b")], b.as_bytes()),
                tree_bytes(hash, &[(b"40000", b"a")], t.as_bytes()),
            ]
            .concat(),
        );
        let full = tree_bytes(hash, &[(b"100644", b"a")], b.as_bytes());
        for n in [1, 3, 7, 9, full.len() - 1] {
            cases.push(full[..n].to_vec());
        }
        cases.push(b"100644a\0".iter().chain(b.as_bytes()).copied().collect());

        for data in cases {
            let out = git_raw(&repo, &["hash-object", "-t", "tree", "--stdin"], &data);
            let theirs = git_findings(&out.stderr, "error: object fails fsck: ");
            // hash-object treats every finding as an error but reports all of
            // a tree's findings.
            let ours: Vec<String> = fsck::findings(ObjectKind::Tree, hash, &data)
                .iter()
                .map(ToString::to_string)
                .collect();
            let what = String::from_utf8_lossy(&data);
            assert_eq!(ours, theirs, "{what:?}");
            if let Some(f) = fsck::findings(ObjectKind::Tree, hash, &data)
                .into_iter()
                .find(|f| f.id == MsgId::BadTree)
            {
                let cause = f.cause.unwrap().to_string();
                assert!(
                    String::from_utf8_lossy(&out.stderr).starts_with(&format!("error: {cause}\n")),
                    "{what:?}"
                );
            }
        }
        std::fs::remove_dir_all(repo).unwrap();
    }
}

#[test]
fn sorted_trees_match_mktree() {
    if !git_available() {
        eprintln!("skipped: no git binary");
        return;
    }
    let mut rng = Rng::new(99);
    for hash in HashKind::ALL {
        let repo = scratch_repo("mktree", hash);
        let names: [&[u8]; 12] = [
            b"a",
            b"a.c",
            b"a-b",
            b"a0",
            b"ab",
            b"b",
            b"A",
            b"a b",
            b"\xc3\xa4",
            b"z",
            b"a.",
            b"_",
        ];
        for _ in 0..40 {
            let mut tree = Tree::new();
            let mut input = Vec::new();
            let mut used = std::collections::HashSet::new();
            for _ in 0..1 + rng.below(8) {
                let name = names[rng.below(names.len())];
                if !used.insert(name) {
                    continue;
                }
                let mut oid = hash.null();
                for b in oid.as_bytes_mut() {
                    *b = rng.next() as u8;
                }
                let (mode, kind) = match rng.below(4) {
                    0 => (EntryMode::TREE, "tree"),
                    1 => (EntryMode::BLOB_EXECUTABLE, "blob"),
                    2 => (EntryMode::COMMIT, "commit"),
                    _ => (EntryMode::BLOB, "blob"),
                };
                input.extend_from_slice(format!("{mode} {kind} {oid}\t").as_bytes());
                input.extend_from_slice(name);
                input.push(b'\n');
                tree.entries.push(TreeEntry::new(mode, name, oid));
            }
            tree.sort();
            let theirs = git(&repo, &["mktree", "--missing"], &input);
            let theirs: ObjectId = std::str::from_utf8(&theirs)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let ours = rgit_object::Object::Tree(tree.clone()).id(hash).unwrap();
            assert_eq!(ours, theirs, "{tree:?}");
            let raw = git(&repo, &["cat-file", "tree", &theirs.to_hex()], b"");
            assert_eq!(raw, tree.serialize());
        }
        std::fs::remove_dir_all(repo).unwrap();
    }
}
