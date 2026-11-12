// SPDX-License-Identifier: MIT

//! A corpus of hand-made odd but valid objects. Each one is written with
//! `git hash-object --literally -w`, read back with `git cat-file`, parsed,
//! re-serialised and compared byte for byte; the id must match git's.
//! Without a `git` binary only the local round trip is checked.

mod common;

use common::{git, git_available, scratch_repo};
use rgit_hash::{HashKind, hash_object};
use rgit_object::{Commit, EntryMode, Object, ObjectKind, Tag, Tree};

fn tree_bytes(hash: HashKind, entries: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut v = Vec::new();
    for (i, (mode, name)) in entries.iter().enumerate() {
        v.extend_from_slice(mode);
        v.push(b' ');
        v.extend_from_slice(name);
        v.push(0);
        let mut id = vec![0x11u8; hash.byte_len()];
        id[0] = i as u8 + 1;
        v.extend_from_slice(&id);
    }
    v
}

fn corpus(hash: HashKind) -> Vec<(ObjectKind, &'static str, Vec<u8>)> {
    let t = hash.empty_tree().to_hex();
    let p = hash.empty_blob().to_hex();
    let a = "author A U Thor <author@example.com> 1112911993 -0700";
    let c = "committer C O Mitter <committer@example.com> 1112912053 -0700";
    let commit = |s: String| (ObjectKind::Commit, s.into_bytes());
    let tag = |s: String| (ObjectKind::Tag, s.into_bytes());
    let sig = "-----BEGIN PGP SIGNATURE-----\n \n iQEcBAABAgAGBQJXYRjRAAoJEGEJLoW3InGJ\n =jKHM\n -----END PGP SIGNATURE-----";
    let mut v: Vec<(ObjectKind, &'static str, Vec<u8>)> = vec![];
    let mut add = |name: &'static str, (k, d): (ObjectKind, Vec<u8>)| v.push((k, name, d));

    add(
        "plain commit",
        commit(format!("tree {t}\n{a}\n{c}\n\nmsg\n")),
    );
    add(
        "root commit, empty message",
        commit(format!("tree {t}\n{a}\n{c}\n\n")),
    );
    add(
        "no message separator",
        commit(format!("tree {t}\n{a}\n{c}\n")),
    );
    add(
        "octopus",
        commit(format!(
            "tree {t}\nparent {p}\nparent {t}\nparent {p}\n{a}\n{c}\n\nm\n"
        )),
    );
    add(
        "encoding",
        commit(format!(
            "tree {t}\n{a}\n{c}\nencoding ISO-8859-1\n\nGr\u{fc}\n"
        )),
    );
    add(
        "latin-1 bytes in ident and message",
        (ObjectKind::Commit, {
            let mut d = format!("tree {t}\nauthor J").into_bytes();
            d.push(0xf6);
            d.extend_from_slice(
                format!("rg <j@x> 1 +0000\n{c}\nencoding ISO-8859-1\n\n").as_bytes(),
            );
            d.extend_from_slice(&[0xe4, 0xf6, 0xfc, b'\n']);
            d
        }),
    );
    add(
        "gpgsig",
        commit(format!("tree {t}\n{a}\n{c}\ngpgsig {sig}\n\nsigned\n")),
    );
    add(
        "gpgsig and gpgsig-sha256",
        commit(format!(
            "tree {t}\n{a}\n{c}\ngpgsig {sig}\ngpgsig-sha256 {sig}\n\nsigned\n"
        )),
    );
    add(
        "mergetag",
        commit(format!(
            "tree {t}\nparent {p}\nparent {t}\n{a}\n{c}\nmergetag object {t}\n type tree\n tag v1\n \
             tagger T <t@x> 1 +0000\n \n tag msg\n {sig}\n\nMerge tag 'v1'\n"
        )),
    );
    add(
        "unknown headers",
        commit(format!(
            "tree {t}\n{a}\n{c}\nx-custom foo\nhg-extra a\n b\n\nm\n"
        )),
    );
    add(
        "header without value",
        commit(format!("tree {t}\n{a}\n{c}\nlonely\n\nm\n")),
    );
    add(
        "empty header value",
        commit(format!("tree {t}\n{a}\n{c}\nfoo \n\nm\n")),
    );
    add(
        "two committers",
        commit(format!("tree {t}\n{a}\n{c}\n{c}\n\nm\n")),
    );
    add(
        "odd timezone",
        commit(format!(
            "tree {t}\nauthor A <a> 1 +1700\ncommitter C <c> 0 -0000\n\nm\n"
        )),
    );
    add(
        "empty name and email",
        commit(format!("tree {t}\nauthor  <> 1 +0000\n{c}\n\nm\n")),
    );
    add(
        "message without trailing newline",
        commit(format!("tree {t}\n{a}\n{c}\n\nno newline")),
    );
    add(
        "message with NUL",
        commit(format!("tree {t}\n{a}\n{c}\n\na\0b\n")),
    );
    add(
        "message with CR LF",
        commit(format!("tree {t}\n{a}\n{c}\n\nline\r\n\r\n")),
    );
    add(
        "leading blank lines",
        commit(format!("tree {t}\n{a}\n{c}\n\n\n\nm\n")),
    );
    add(
        "uppercase hex",
        commit(format!("tree {}\n{a}\n{c}\n\nm\n", t.to_uppercase())),
    );
    add("missing author", commit(format!("tree {t}\n{c}\n\nm\n")));
    add(
        "header continuation of nothing",
        commit(format!(" odd\ntree {t}\n{a}\n{c}\n\nm\n")),
    );

    add(
        "plain tag",
        tag(format!(
            "object {t}\ntype tree\ntag v1\ntagger T <t@x> 1 +0000\n\nmsg\n"
        )),
    );
    add(
        "tag without tagger",
        tag(format!("object {t}\ntype tree\ntag v0.99\n\nold style\n")),
    );
    add(
        "tag without message",
        tag(format!(
            "object {t}\ntype tree\ntag v1\ntagger T <t@x> 1 +0000\n"
        )),
    );
    add(
        "signed tag",
        tag(format!(
            "object {t}\ntype tree\ntag signed\ntagger T <t@x> 1 +0000\n\nsigned\n\
             -----BEGIN PGP SIGNATURE-----\n\nabc\n-----END PGP SIGNATURE-----\n"
        )),
    );
    add(
        "ssh signed tag",
        tag(format!(
            "object {t}\ntype tree\ntag s\ntagger T <t@x> 1 +0000\n\ns\n\
             -----BEGIN SSH SIGNATURE-----\nU1NIU0lH\n-----END SSH SIGNATURE-----\n"
        )),
    );
    add(
        "tag with extra header",
        tag(format!(
            "object {t}\ntype tree\ntag v1\ntagger T <t@x> 1 +0000\nextra x\n\nm\n"
        )),
    );
    add(
        "tag of a tag type",
        tag(format!(
            "object {t}\ntype tag\ntag nested\ntagger T <t@x> 1 +0000\n\nm\n"
        )),
    );
    add(
        "tag with gpgsig header",
        tag(format!(
            "object {t}\ntype tree\ntag v1\ntagger T <t@x> 1 +0000\ngpgsig {sig}\n\nm\n"
        )),
    );

    let tr = |e: &[(&[u8], &[u8])]| (ObjectKind::Tree, tree_bytes(hash, e));
    add("empty tree", (ObjectKind::Tree, Vec::new()));
    add(
        "standard modes",
        tr(&[
            (b"100644", b"a"),
            (b"100755", b"b"),
            (b"120000", b"c"),
            (b"40000", b"d"),
            (b"160000", b"e"),
        ]),
    );
    add("zero-padded dir mode", tr(&[(b"040000", b"dir")]));
    add("group-writable mode", tr(&[(b"100664", b"old")]));
    add(
        "odd modes",
        tr(&[
            (b"100600", b"a"),
            (b"644", b"b"),
            (b"0", b"c"),
            (b"0100644", b"d"),
        ]),
    );
    add(
        "dir/file order",
        tr(&[(b"100644", b"a.c"), (b"40000", b"a"), (b"100644", b"a0")]),
    );
    add("unsorted", tr(&[(b"100644", b"b"), (b"100644", b"a")]));
    add("duplicates", tr(&[(b"100644", b"a"), (b"100644", b"a")]));
    add(
        "non-UTF-8 name",
        tr(&[(b"100644", b"\xff\xfe"), (b"100644", "\u{e4}".as_bytes())]),
    );
    add("name with newline and tab", tr(&[(b"100644", b"a\nb\tc")]));
    add("blob", (ObjectKind::Blob, b"\0\x01binary\xff".to_vec()));
    add("empty blob", (ObjectKind::Blob, Vec::new()));
    v
}

#[test]
fn odd_objects_round_trip() {
    let with_git = git_available();
    for hash in HashKind::ALL {
        let repo = with_git.then(|| scratch_repo("odd", hash));
        for (kind, name, data) in corpus(hash) {
            let obj = Object::parse(kind, hash, &data)
                .unwrap_or_else(|e| panic!("{name} ({hash:?}): {e}"));
            assert!(
                obj.serialize() == data,
                "{name} ({hash:?}) does not round-trip"
            );
            let id = obj.id(hash).unwrap();
            assert_eq!(id, hash_object(hash, kind.name(), &data).unwrap());
            if let Some(repo) = &repo {
                let out = git(
                    repo,
                    &[
                        "hash-object",
                        "--literally",
                        "-w",
                        "--stdin",
                        "-t",
                        kind.name(),
                    ],
                    &data,
                );
                assert_eq!(
                    std::str::from_utf8(&out).unwrap().trim(),
                    id.to_hex(),
                    "{name} ({hash:?})"
                );
                let back = git(repo, &["cat-file", kind.name(), &id.to_hex()], b"");
                assert!(back == data, "{name} ({hash:?}): cat-file differs");
            }
        }
        if let Some(repo) = repo {
            std::fs::remove_dir_all(repo).unwrap();
        }
    }
}

#[test]
fn typed_access_on_odd_objects() {
    let hash = HashKind::Sha1;
    let corpus = corpus(hash);
    let get = |n: &str| &corpus.iter().find(|(_, name, _)| *name == n).unwrap().2;

    let c = Commit::parse(hash, get("mergetag")).unwrap();
    assert_eq!(c.parents().unwrap().len(), 2);
    let mt: Vec<_> = c.mergetags().collect();
    assert_eq!(mt.len(), 1);
    let inner = Tag::parse(hash, &[mt[0], b"\n"].concat()).unwrap();
    assert_eq!(inner.name().unwrap(), b"v1");
    assert!(inner.message_and_signature().1.is_some());

    let c = Commit::parse(hash, get("no message separator")).unwrap();
    assert_eq!(c.block().message, None);
    let c = Commit::parse(hash, get("root commit, empty message")).unwrap();
    assert_eq!(c.block().message.as_deref(), Some(&b""[..]));

    let c = Commit::parse(hash, get("latin-1 bytes in ident and message")).unwrap();
    assert_eq!(c.author().unwrap().name, b"J\xf6rg");
    assert_eq!(c.encoding().unwrap(), b"ISO-8859-1");

    let c = Commit::parse(hash, get("missing author")).unwrap();
    assert!(c.author().is_none());
    assert!(c.tree().is_ok());

    let t = Tag::parse(hash, get("tag without tagger")).unwrap();
    assert!(t.tagger_raw().is_none());

    let tree = Tree::parse(hash, get("zero-padded dir mode")).unwrap();
    assert!(tree.entries[0].mode.is_tree());
    assert!(!tree.entries[0].mode.is_canonical_text());
    let tree = Tree::parse(hash, get("group-writable mode")).unwrap();
    assert_eq!(tree.entries[0].mode.canonical(), EntryMode::BLOB);
}
